//! 音声入出力モジュール。
//!
//! - 送信: マイク入力 → モノラル化 → 48kHzへ変換 → 送信判定(常時/VAD/PTT)
//!   → 20msフレームでOpusエンコード → `OutPacket`
//! - 受信: `AudioHandler`(tsclientlib付属)がOpusデコードと話者ミキシングを行い、
//!   48kHzステレオのサンプルをデバイスのレート/チャンネル数に変換して再生する
//!
//! cpal(WASAPI共有モード)はデバイス既定のフォーマットしか受け付けないため、
//! ストリームは既定レートで開き、48kHzとの差は線形補間で吸収する。
//! cpalの`Stream`は`Send`ではないため、専用スレッドで作成・保持し、
//! デバイス変更は`AudioCommand`で依頼してスレッド内で作り直す。

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{bail, Context as _, Result};
use audiopus::coder::Encoder;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use serde::{Deserialize, Serialize};
use tsclientlib::ClientId;
use tsproto_packets::packets::{AudioData, CodecType, OutAudio, OutPacket};

pub type AudioHandler = tsclientlib::audio::AudioHandler<ClientId>;

const TS_RATE: u32 = 48000;
/// 20ms @ 48kHz モノラル
const FRAME_SAMPLES: usize = 960;
/// Opusフレームの最大サイズ (RFC 6716)
const MAX_OPUS_FRAME_SIZE: usize = 1275;
/// VADで無音判定後も送信を続けるフレーム数 (20ms x 15 = 300ms)
const VAD_HANGOVER_FRAMES: u32 = 15;
/// レベルメーターの下限
const SILENCE_DB: f32 = -100.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VoiceMode {
    /// 常時送信
    Always = 0,
    /// ボイスアクティベーション(入力レベルが閾値を超えたら送信)
    VoiceActivation = 1,
    /// プッシュトゥトーク(キーを押している間だけ送信)
    PushToTalk = 2,
}

impl VoiceMode {
    fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::VoiceActivation,
            2 => Self::PushToTalk,
            _ => Self::Always,
        }
    }
}

/// UIとキャプチャコールバックが共有する送信設定。
/// コールバック内でロックを取らずに済むよう、すべてアトミックで持つ。
pub struct Controls {
    pub muted: AtomicBool,
    mode: AtomicU8,
    /// dB x 10 (小数1桁を整数に畳む)
    vad_threshold_db10: AtomicI32,
    ptt_vk: AtomicI32,
    /// 現在の入力レベル dB x 10 (メーター表示用、コールバックが書き込む)
    input_level_db10: AtomicI32,
}

impl Controls {
    fn new(cfg: &crate::config::Config) -> Self {
        Self {
            muted: AtomicBool::new(false),
            mode: AtomicU8::new(cfg.voice_mode as u8),
            vad_threshold_db10: AtomicI32::new((cfg.vad_threshold_db * 10.0) as i32),
            ptt_vk: AtomicI32::new(cfg.ptt_vk),
            input_level_db10: AtomicI32::new((SILENCE_DB * 10.0) as i32),
        }
    }

    pub fn mode(&self) -> VoiceMode {
        VoiceMode::from_u8(self.mode.load(Ordering::Relaxed))
    }
    pub fn set_mode(&self, mode: VoiceMode) {
        self.mode.store(mode as u8, Ordering::Relaxed);
    }
    pub fn vad_threshold_db(&self) -> f32 {
        self.vad_threshold_db10.load(Ordering::Relaxed) as f32 / 10.0
    }
    pub fn set_vad_threshold_db(&self, db: f32) {
        self.vad_threshold_db10.store((db * 10.0) as i32, Ordering::Relaxed);
    }
    pub fn ptt_vk(&self) -> i32 {
        self.ptt_vk.load(Ordering::Relaxed)
    }
    pub fn set_ptt_vk(&self, vk: i32) {
        self.ptt_vk.store(vk, Ordering::Relaxed);
    }
    pub fn input_level_db(&self) -> f32 {
        self.input_level_db10.load(Ordering::Relaxed) as f32 / 10.0
    }
}

/// 音声スレッドへの依頼。Noneは「既定のデバイス」
#[derive(Debug)]
pub enum AudioCommand {
    SetInputDevice(Option<String>),
    SetOutputDevice(Option<String>),
}

pub struct AudioSystem {
    /// 受信音声のデコード/ミキシング。ワーカーが受信パケットを入れ、再生コールバックが取り出す
    pub handler: Arc<Mutex<AudioHandler>>,
    pub controls: Arc<Controls>,
    pub commands: std::sync::mpsc::Sender<AudioCommand>,
}

/// 選択可能なデバイス名の一覧 (入力, 出力)
pub fn list_devices() -> (Vec<String>, Vec<String>) {
    fn collect<I: Iterator<Item = cpal::Device>>(devices: Result<I, cpal::Error>) -> Vec<String> {
        devices.map(|it| it.map(|d| device_name(&d)).collect()).unwrap_or_default()
    }
    let host = cpal::default_host();
    (collect(host.input_devices()), collect(host.output_devices()))
}

/// 音声デバイスを専用スレッドで開く。
/// 戻り値: (共有状態, エンコード済み送信パケットの受け口)
pub fn start(
    cfg: &crate::config::Config,
    log: impl Fn(String) + Send + 'static,
) -> (AudioSystem, tokio::sync::mpsc::Receiver<OutPacket>) {
    let handler = Arc::new(Mutex::new(AudioHandler::new()));
    let controls = Arc::new(Controls::new(cfg));
    let (packet_tx, packet_rx) = tokio::sync::mpsc::channel(8);
    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<AudioCommand>();

    let system = AudioSystem {
        handler: handler.clone(),
        controls: controls.clone(),
        commands: cmd_tx,
    };
    let mut input_name = cfg.input_device.clone();
    let mut output_name = cfg.output_device.clone();

    std::thread::spawn(move || {
        let mut out_stream = match build_playback(&output_name, handler.clone(), &log) {
            Ok(s) => Some(s),
            Err(e) => {
                log(format!("再生デバイスの初期化に失敗: {e:#}"));
                None
            }
        };
        let mut in_stream =
            match build_capture(&input_name, controls.clone(), packet_tx.clone(), &log) {
                Ok(s) => Some(s),
                Err(e) => {
                    log(format!("録音デバイスの初期化に失敗: {e:#}"));
                    None
                }
            };

        // デバイス変更依頼を待つ(チャネルが閉じたら終了)
        while let Ok(cmd) = cmd_rx.recv() {
            match cmd {
                AudioCommand::SetInputDevice(name) => {
                    input_name = name;
                    drop(in_stream.take()); // 先に既存ストリームを止める
                    match build_capture(&input_name, controls.clone(), packet_tx.clone(), &log) {
                        Ok(s) => in_stream = Some(s),
                        Err(e) => log(format!("録音デバイスの切替に失敗: {e:#}")),
                    }
                }
                AudioCommand::SetOutputDevice(name) => {
                    output_name = name;
                    drop(out_stream.take());
                    match build_playback(&output_name, handler.clone(), &log) {
                        Ok(s) => out_stream = Some(s),
                        Err(e) => log(format!("再生デバイスの切替に失敗: {e:#}")),
                    }
                }
            }
        }
    });

    (system, packet_rx)
}

/// 名前でデバイスを探す。見つからなければ既定のデバイスにフォールバックする
fn find_device(
    name: &Option<String>,
    is_input: bool,
    log: &impl Fn(String),
) -> Result<cpal::Device> {
    let host = cpal::default_host();
    if let Some(name) = name {
        let devices = if is_input { host.input_devices() } else { host.output_devices() };
        if let Ok(mut devices) = devices {
            if let Some(device) = devices.find(|d| &device_name(d) == name) {
                return Ok(device);
            }
        }
        log(format!("デバイス「{name}」が見つからないため既定のデバイスを使います"));
    }
    let device =
        if is_input { host.default_input_device() } else { host.default_output_device() };
    device.context("既定のデバイスが見つかりません")
}

fn build_playback(
    name: &Option<String>,
    handler: Arc<Mutex<AudioHandler>>,
    log: &impl Fn(String),
) -> Result<cpal::Stream> {
    let device = find_device(name, false, log)?;
    let default = device.default_output_config().context("既定の出力設定を取得できません")?;
    if default.sample_format() != cpal::SampleFormat::F32 {
        bail!("出力デバイスがf32非対応です: {:?}", default.sample_format());
    }
    let dev_rate = default.sample_rate();
    let dev_channels = default.channels() as usize;
    let config = cpal::StreamConfig {
        channels: default.channels(),
        sample_rate: dev_rate,
        buffer_size: cpal::BufferSize::Default,
    };

    // 出力1フレームあたりに進める48kHzソースのフレーム数
    let step = TS_RATE as f64 / dev_rate as f64;
    // 48kHzステレオの未消費サンプル(コールバックをまたいで補間位置を保持する)
    let mut src: Vec<f32> = Vec::new();
    let mut pos: f64 = 0.0;
    let mut fetch_buf: Vec<f32> = Vec::new();

    let stream = device
        .build_output_stream(
            config,
            move |buf: &mut [f32], _: &cpal::OutputCallbackInfo| {
                let out_frames = buf.len() / dev_channels;
                // 補間に必要なソースフレーム数(+1は補間の右端用)
                let needed = (pos + out_frames as f64 * step).ceil() as usize + 1;
                let have = src.len() / 2;
                if needed > have {
                    fetch_buf.clear();
                    fetch_buf.resize((needed - have) * 2, 0.0);
                    handler.lock().unwrap().fill_buffer(&mut fetch_buf);
                    src.extend_from_slice(&fetch_buf);
                }

                for frame in 0..out_frames {
                    let p = pos + frame as f64 * step;
                    let i = p as usize;
                    let frac = (p - i as f64) as f32;
                    let (l, r) = lerp_stereo(&src, i, frac);
                    let out = &mut buf[frame * dev_channels..(frame + 1) * dev_channels];
                    match dev_channels {
                        1 => out[0] = (l + r) * 0.5,
                        _ => {
                            out.fill(0.0);
                            out[0] = l;
                            out[1] = r;
                        }
                    }
                }

                // 消費済みフレームを捨てて補間位置を先頭に寄せる
                pos += out_frames as f64 * step;
                let consumed = (pos as usize).min(src.len() / 2);
                src.drain(..consumed * 2);
                pos -= consumed as f64;
            },
            |e| tracing::error!("再生ストリームエラー: {e}"),
            None,
        )
        .context("再生ストリームの作成に失敗")?;
    stream.play().context("再生ストリームの開始に失敗")?;
    log(format!("再生デバイス: {} ({dev_rate}Hz {dev_channels}ch)", device_name(&device)));
    Ok(stream)
}

fn build_capture(
    name: &Option<String>,
    controls: Arc<Controls>,
    packet_tx: tokio::sync::mpsc::Sender<OutPacket>,
    log: &impl Fn(String),
) -> Result<cpal::Stream> {
    let device = find_device(name, true, log)?;
    let default = device.default_input_config().context("既定の入力設定を取得できません")?;
    if default.sample_format() != cpal::SampleFormat::F32 {
        bail!("入力デバイスがf32非対応です: {:?}", default.sample_format());
    }
    let dev_rate = default.sample_rate();
    let dev_channels = default.channels() as usize;
    let config = cpal::StreamConfig {
        channels: default.channels(),
        sample_rate: dev_rate,
        buffer_size: cpal::BufferSize::Default,
    };

    let encoder = Encoder::new(
        audiopus::SampleRate::Hz48000,
        audiopus::Channels::Mono,
        audiopus::Application::Voip,
    )
    .context("Opusエンコーダの作成に失敗")?;

    // 48kHz1サンプルあたりに進めるデバイスレートのサンプル数
    let step = dev_rate as f64 / TS_RATE as f64;
    let mut src: Vec<f32> = Vec::new(); // デバイスレートのモノラル未消費分
    let mut pos: f64 = 0.0;
    let mut frame_buf: Vec<f32> = Vec::with_capacity(FRAME_SAMPLES * 4); // 48kHzモノラル
    let mut opus_out = [0u8; MAX_OPUS_FRAME_SIZE];
    let mut vad_hangover: u32 = 0;

    let stream = device
        .build_input_stream(
            config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                if controls.muted.load(Ordering::Relaxed) {
                    src.clear();
                    frame_buf.clear();
                    pos = 0.0;
                    controls.input_level_db10.store((SILENCE_DB * 10.0) as i32, Ordering::Relaxed);
                    return;
                }
                for frame in data.chunks_exact(dev_channels) {
                    src.push(frame.iter().sum::<f32>() / dev_channels as f32);
                }

                // 48kHzへ変換(線形補間)
                while (pos.ceil() as usize) + 1 < src.len() {
                    let i = pos as usize;
                    let frac = (pos - i as f64) as f32;
                    frame_buf.push(src[i] + (src[i + 1] - src[i]) * frac);
                    pos += step;
                }
                let consumed = (pos as usize).min(src.len());
                src.drain(..consumed);
                pos -= consumed as f64;

                // 20msフレームごとに送信判定してエンコード
                while frame_buf.len() >= FRAME_SAMPLES {
                    let frame = &frame_buf[..FRAME_SAMPLES];
                    let level_db = rms_db(frame);
                    controls.input_level_db10.store((level_db * 10.0) as i32, Ordering::Relaxed);

                    let send = match controls.mode() {
                        VoiceMode::Always => true,
                        VoiceMode::PushToTalk => is_key_down(controls.ptt_vk()),
                        VoiceMode::VoiceActivation => {
                            if level_db >= controls.vad_threshold_db() {
                                vad_hangover = VAD_HANGOVER_FRAMES;
                            } else {
                                vad_hangover = vad_hangover.saturating_sub(1);
                            }
                            vad_hangover > 0
                        }
                    };

                    if send {
                        match encoder.encode_float(frame, &mut opus_out[..]) {
                            Ok(len) => {
                                let packet = OutAudio::new(&AudioData::C2S {
                                    id: 0,
                                    codec: CodecType::OpusVoice,
                                    data: &opus_out[..len],
                                });
                                // 未接続時やワーカーが詰まっているときは捨てる
                                let _ = packet_tx.try_send(packet);
                            }
                            Err(e) => tracing::error!("Opusエンコードエラー: {e}"),
                        }
                    }
                    frame_buf.drain(..FRAME_SAMPLES);
                }
            },
            |e| tracing::error!("録音ストリームエラー: {e}"),
            None,
        )
        .context("録音ストリームの作成に失敗")?;
    stream.play().context("録音ストリームの開始に失敗")?;
    log(format!("録音デバイス: {} ({dev_rate}Hz {dev_channels}ch)", device_name(&device)));
    Ok(stream)
}

/// フレームのRMSレベル(dBFS)
fn rms_db(frame: &[f32]) -> f32 {
    let mean_sq = frame.iter().map(|s| s * s).sum::<f32>() / frame.len() as f32;
    if mean_sq <= 1e-10 { SILENCE_DB } else { 10.0 * mean_sq.log10() }
}

/// Windows仮想キーコードのキーが押されているか(グローバル、ウィンドウ非フォーカスでも有効)
fn is_key_down(vk: i32) -> bool {
    // SAFETY: GetAsyncKeyStateは任意のスレッドから呼べる読み取り専用API
    unsafe {
        (windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(vk) as u16 & 0x8000)
            != 0
    }
}

/// ステレオ・インターリーブ配列の`i`番目と`i+1`番目のフレームを`frac`で線形補間する
fn lerp_stereo(src: &[f32], i: usize, frac: f32) -> (f32, f32) {
    let get = |idx: usize| -> (f32, f32) {
        if idx * 2 + 1 < src.len() { (src[idx * 2], src[idx * 2 + 1]) } else { (0.0, 0.0) }
    };
    let (l0, r0) = get(i);
    let (l1, r1) = get(i + 1);
    (l0 + (l1 - l0) * frac, r0 + (r1 - r0) * frac)
}

fn device_name(device: &cpal::Device) -> String {
    device
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_else(|_| "不明".into())
}
