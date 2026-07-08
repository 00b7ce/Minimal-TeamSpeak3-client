//! 音声入出力モジュール。
//!
//! - 送信: マイク入力 → モノラル化 → 48kHzへ変換 → 20msフレームでOpusエンコード → `OutPacket`
//! - 受信: `AudioHandler`(tsclientlib付属)がOpusデコードと話者ミキシングを行い、
//!   48kHzステレオのサンプルをデバイスのレート/チャンネル数に変換して再生する
//!
//! cpal(WASAPI共有モード)はデバイス既定のフォーマットしか受け付けないため、
//! ストリームは既定レートで開き、48kHzとの差は線形補間で吸収する。
//! cpalの`Stream`は`Send`ではないため、専用スレッドで作成してそのまま保持する。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{bail, Context as _, Result};
use audiopus::coder::Encoder;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use tsclientlib::ClientId;
use tsproto_packets::packets::{AudioData, CodecType, OutAudio, OutPacket};

pub type AudioHandler = tsclientlib::audio::AudioHandler<ClientId>;

const TS_RATE: u32 = 48000;
/// 20ms @ 48kHz モノラル
const FRAME_SAMPLES: usize = 960;
/// Opusフレームの最大サイズ (RFC 6716)
const MAX_OPUS_FRAME_SIZE: usize = 1275;

pub struct AudioSystem {
    /// 受信音声のデコード/ミキシング。ワーカーが受信パケットを入れ、再生コールバックが取り出す
    pub handler: Arc<Mutex<AudioHandler>>,
    /// マイクミュート(UIから切り替え、キャプチャコールバックが参照)
    pub muted: Arc<AtomicBool>,
}

/// 音声デバイスを専用スレッドで開く。
/// 戻り値: (共有状態, エンコード済み送信パケットの受け口)
pub fn start(
    log: impl Fn(String) + Send + 'static,
) -> (AudioSystem, tokio::sync::mpsc::Receiver<OutPacket>) {
    let handler = Arc::new(Mutex::new(AudioHandler::new()));
    let muted = Arc::new(AtomicBool::new(false));
    let (packet_tx, packet_rx) = tokio::sync::mpsc::channel(8);

    let system = AudioSystem { handler: handler.clone(), muted: muted.clone() };

    std::thread::spawn(move || {
        match build_streams(handler, muted, packet_tx, &log) {
            Ok(_streams) => {
                // ストリームはドロップすると停止するため、このスレッドで保持し続ける
                loop {
                    std::thread::park();
                }
            }
            Err(e) => log(format!("音声デバイスの初期化に失敗: {e:#}")),
        }
    });

    (system, packet_rx)
}

fn build_streams(
    handler: Arc<Mutex<AudioHandler>>,
    muted: Arc<AtomicBool>,
    packet_tx: tokio::sync::mpsc::Sender<OutPacket>,
    log: &impl Fn(String),
) -> Result<(cpal::Stream, cpal::Stream)> {
    let host = cpal::default_host();

    let output = host.default_output_device().context("出力デバイスが見つかりません")?;
    let out_stream = build_playback(&output, handler, log).context("再生側の初期化に失敗")?;

    let input = host.default_input_device().context("入力デバイス(マイク)が見つかりません")?;
    let in_stream =
        build_capture(&input, muted, packet_tx, log).context("録音側の初期化に失敗")?;

    Ok((in_stream, out_stream))
}

fn build_playback(
    device: &cpal::Device,
    handler: Arc<Mutex<AudioHandler>>,
    log: &impl Fn(String),
) -> Result<cpal::Stream> {
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
    log(format!("再生デバイス: {} ({dev_rate}Hz {dev_channels}ch)", device_name(device)));
    Ok(stream)
}

fn build_capture(
    device: &cpal::Device,
    muted: Arc<AtomicBool>,
    packet_tx: tokio::sync::mpsc::Sender<OutPacket>,
    log: &impl Fn(String),
) -> Result<cpal::Stream> {
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

    // 出力(48kHz)1サンプルあたりに進めるデバイスレートのサンプル数
    let step = dev_rate as f64 / TS_RATE as f64;
    let mut src: Vec<f32> = Vec::new(); // デバイスレートのモノラル未消費分
    let mut pos: f64 = 0.0;
    let mut frame_buf: Vec<f32> = Vec::with_capacity(FRAME_SAMPLES * 4); // 48kHzモノラル
    let mut opus_out = [0u8; MAX_OPUS_FRAME_SIZE];

    let stream = device
        .build_input_stream(
            config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                if muted.load(Ordering::Relaxed) {
                    src.clear();
                    frame_buf.clear();
                    pos = 0.0;
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

                // 20msフレームごとにエンコードして送る
                while frame_buf.len() >= FRAME_SAMPLES {
                    match encoder.encode_float(&frame_buf[..FRAME_SAMPLES], &mut opus_out[..]) {
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
                    frame_buf.drain(..FRAME_SAMPLES);
                }
            },
            |e| tracing::error!("録音ストリームエラー: {e}"),
            None,
        )
        .context("録音ストリームの作成に失敗")?;
    stream.play().context("録音ストリームの開始に失敗")?;
    log(format!("録音デバイス: {} ({dev_rate}Hz {dev_channels}ch)", device_name(device)));
    Ok(stream)
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
