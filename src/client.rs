//! TS3サーバとの接続をバックグラウンドスレッドで管理するモジュール。
//!
//! UIスレッドとは2本のチャネルでやり取りする:
//! - `Command` (UI → ワーカー): 接続/切断の指示
//! - `Update` (ワーカー → UI): 接続状態・チャンネル一覧・ログ
//!
//! 音声は`audio`モジュールが担当し、ワーカーは
//! 「エンコード済みパケットの送信」と「受信パケットのデコーダ投入」だけを中継する。

use std::sync::{Arc, Mutex};

use eframe::egui;
use futures::prelude::*;
use tsclientlib::{ClientId, Connection, DisconnectOptions, StreamItem};
use tsproto_packets::packets::{AudioData, OutPacket};

use crate::audio;

#[derive(Debug)]
pub enum Command {
    Connect { address: String, nickname: String },
    Disconnect,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Disconnected,
    Connecting,
    Connected { server_name: String },
    Error(String),
}

#[derive(Debug, Clone)]
pub struct ClientInfo {
    pub id: u16,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct ChannelInfo {
    pub name: String,
    pub clients: Vec<ClientInfo>,
}

#[derive(Debug)]
pub enum Update {
    Status(Status),
    Snapshot(Vec<ChannelInfo>),
    Log(String),
}

pub struct ClientHandle {
    pub commands: tokio::sync::mpsc::UnboundedSender<Command>,
    pub updates: std::sync::mpsc::Receiver<Update>,
    /// 送信モード・ミュート・レベルメーターなど(UIから直接読み書きする)
    pub audio_controls: Arc<audio::Controls>,
    /// 音声デバイスの切替依頼
    pub audio_commands: std::sync::mpsc::Sender<audio::AudioCommand>,
    /// 受信音声のキュー(再生中の相手の音量を即時変更するのに使う)
    pub audio_handler: Arc<Mutex<audio::AudioHandler>>,
    /// 相手ごとの再生音量(ニックネーム → 倍率)。新しい話者のキュー作成時に適用される
    pub volumes: Arc<Mutex<std::collections::HashMap<String, f32>>>,
}

/// 音声システムとワーカースレッドを起動する。`ctx`はUpdate送信時の再描画要求に使う。
pub fn spawn(ctx: egui::Context, config: &crate::config::Config) -> ClientHandle {
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    let (update_tx, update_rx) = std::sync::mpsc::channel();

    let audio_log = {
        let tx = update_tx.clone();
        let ctx = ctx.clone();
        move |message: String| {
            tracing::info!("audio: {message}");
            let _ = tx.send(Update::Log(message));
            ctx.request_repaint();
        }
    };
    let (audio_system, audio_rx) = audio::start(config, audio_log);
    let volumes = Arc::new(Mutex::new(config.volumes.clone()));

    let handle = ClientHandle {
        commands: cmd_tx.clone(),
        updates: update_rx,
        audio_controls: audio_system.controls.clone(),
        audio_commands: audio_system.commands,
        audio_handler: audio_system.handler.clone(),
        volumes: volumes.clone(),
    };

    // StreamDeck連携用HTTP APIの共有状態
    let status = Arc::new(Mutex::new(Status::Disconnected));
    let api_state = Arc::new(crate::api::ApiState {
        commands: cmd_tx,
        controls: audio_system.controls,
        status: status.clone(),
        log: Box::new({
            let tx = update_tx.clone();
            let ctx = ctx.clone();
            move |message| {
                tracing::info!("{message}");
                let _ = tx.send(Update::Log(message));
                ctx.request_repaint();
            }
        }),
    });
    let api_port = config.api_port;

    let handler = audio_system.handler;
    let effects = audio_system.effects;
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokioランタイムの作成に失敗");
        rt.spawn(crate::api::serve(api_port, api_state));
        rt.block_on(run_worker(
            cmd_rx, update_tx, ctx, handler, audio_rx, status, volumes, effects,
        ));
    });

    handle
}

struct Sender {
    tx: std::sync::mpsc::Sender<Update>,
    ctx: egui::Context,
    /// HTTP APIの/api/statusにも状態を映す
    status: Arc<Mutex<Status>>,
    /// 接続/切断チャイム用
    effects: audio::EffectsQueue,
}

impl Sender {
    fn send(&self, update: Update) {
        if let Update::Status(new_status) = &update {
            let mut status = crate::lock(&self.status);
            // 状態遷移で効果音を鳴らす(UI/API/切断イベントのどこから来ても通る)
            let was_connected = matches!(&*status, Status::Connected { .. });
            let is_connected = matches!(new_status, Status::Connected { .. });
            if is_connected && !was_connected {
                audio::queue_effect(&self.effects, audio::SoundEffect::Connect);
            } else if was_connected && !is_connected {
                audio::queue_effect(&self.effects, audio::SoundEffect::Disconnect);
            }
            *status = new_status.clone();
        }
        let _ = self.tx.send(update);
        self.ctx.request_repaint();
    }

    fn log(&self, message: impl Into<String>) {
        self.send(Update::Log(message.into()));
    }
}

/// ハンドシェイク(接続開始から最初の状態同期まで)の上限
const HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
/// 切断処理の完了待ちの上限(サーバ無応答でもワーカーを固まらせない)
const DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(6);
/// 自動再接続の間隔と上限回数
const RECONNECT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3);
const MAX_RECONNECT_ATTEMPTS: u32 = 20;

/// connected_loopの終了理由
enum LoopEnd {
    /// UIが終了した(ワーカーも終了する)
    UiClosed,
    /// ユーザーが切断を指示した
    UserDisconnect,
    /// 接続が失われた。establishedは一度でも接続確立していたか
    Lost { established: bool },
}

async fn run_worker(
    mut cmd_rx: tokio::sync::mpsc::UnboundedReceiver<Command>,
    tx: std::sync::mpsc::Sender<Update>,
    ctx: egui::Context,
    audio_handler: Arc<Mutex<audio::AudioHandler>>,
    mut audio_rx: tokio::sync::mpsc::Receiver<OutPacket>,
    status: Arc<Mutex<Status>>,
    volumes: Arc<Mutex<std::collections::HashMap<String, f32>>>,
    effects: audio::EffectsQueue,
) {
    let tx = Sender { tx, ctx, status, effects };

    'idle: loop {
        // 未接続: Connectコマンドを待つ
        let (mut address, mut nickname) = loop {
            match cmd_rx.recv().await {
                Some(Command::Connect { address, nickname }) => break (address, nickname),
                Some(Command::Disconnect) => {}
                None => return, // UI終了
            }
        };

        // 接続試行ループ(切断されたら自動再接続する)
        let mut attempts: u32 = 0;
        loop {
            tx.send(Update::Status(Status::Connecting));
            tx.log(format!("{address} に {nickname} として接続します"));

            let end = match Connection::build(address.clone()).name(nickname.clone()).connect() {
                Ok(mut con) => {
                    // 未接続の間に溜まった送信パケットを捨てる
                    while audio_rx.try_recv().is_ok() {}
                    let end = connected_loop(
                        &mut con,
                        &mut cmd_rx,
                        &mut audio_rx,
                        &audio_handler,
                        &volumes,
                        &tx,
                    )
                    .await;
                    crate::lock(&audio_handler).reset();
                    tx.send(Update::Snapshot(Vec::new()));
                    end
                }
                Err(e) => {
                    tx.send(Update::Status(Status::Error(e.to_string())));
                    LoopEnd::Lost { established: false }
                }
            };

            match end {
                LoopEnd::UiClosed => return,
                LoopEnd::UserDisconnect => {
                    tx.send(Update::Status(Status::Disconnected));
                    continue 'idle;
                }
                LoopEnd::Lost { established } => {
                    // 一度確立した接続が切れた場合はカウントを最初から
                    if established {
                        attempts = 0;
                    } else if attempts == 0 {
                        // 手動での接続がそもそも失敗した(アドレス誤りなど): 再試行しない
                        // (エラーstatusはconnected_loop側で送信済み)
                        continue 'idle;
                    }
                    attempts += 1;
                    if attempts > MAX_RECONNECT_ATTEMPTS {
                        tx.send(Update::Status(Status::Error(format!(
                            "再接続を{MAX_RECONNECT_ATTEMPTS}回試みましたが失敗しました"
                        ))));
                        continue 'idle;
                    }
                    tx.log(format!(
                        "{}秒後に再接続します ({attempts}/{MAX_RECONNECT_ATTEMPTS})",
                        RECONNECT_INTERVAL.as_secs()
                    ));
                    tx.send(Update::Status(Status::Connecting));
                    // 待機中もコマンドは受け付ける
                    match tokio::time::timeout(RECONNECT_INTERVAL, cmd_rx.recv()).await {
                        Err(_) => {} // 時間切れ → 再接続へ
                        Ok(None) => return,
                        Ok(Some(Command::Disconnect)) => {
                            tx.log("再接続を中止します");
                            tx.send(Update::Status(Status::Disconnected));
                            continue 'idle;
                        }
                        Ok(Some(Command::Connect { address: a, nickname: n })) => {
                            address = a;
                            nickname = n;
                            attempts = 0;
                        }
                    }
                }
            }
        }
    }
}

/// 正規の切断手順の完了を待つ。サーバが応答しなくても`DRAIN_TIMEOUT`で切り上げる
async fn drain_connection(con: &mut Connection) {
    if con.disconnect(DisconnectOptions::new()).is_ok() {
        let _ = tokio::time::timeout(
            DRAIN_TIMEOUT,
            con.events().for_each(|_| future::ready(())),
        )
        .await;
    }
}

/// 接続中のメインループ。
async fn connected_loop(
    con: &mut Connection,
    cmd_rx: &mut tokio::sync::mpsc::UnboundedReceiver<Command>,
    audio_rx: &mut tokio::sync::mpsc::Receiver<OutPacket>,
    audio_handler: &Arc<Mutex<audio::AudioHandler>>,
    volumes: &Arc<Mutex<std::collections::HashMap<String, f32>>>,
    tx: &Sender,
) -> LoopEnd {
    let mut established = false;
    let handshake_deadline = tokio::time::Instant::now() + HANDSHAKE_TIMEOUT;
    loop {
        enum Step {
            Cmd(Option<Command>),
            AudioOut(Option<OutPacket>),
            Event(Option<Result<StreamItem, tsclientlib::Error>>),
            HandshakeTimeout,
        }
        // con.events()のストリームはselect!のスコープ内でのみ生存させ、
        // 分岐を抜けたあとにget_state()でconを再借用できるようにする
        let step = tokio::select! {
            cmd = cmd_rx.recv() => Step::Cmd(cmd),
            packet = audio_rx.recv() => Step::AudioOut(packet),
            item = async { con.events().next().await } => Step::Event(item),
            _ = tokio::time::sleep_until(handshake_deadline), if !established => Step::HandshakeTimeout,
        };

        match step {
            Step::HandshakeTimeout => {
                tx.log("接続タイムアウト: サーバからの応答がありません");
                tx.send(Update::Status(Status::Error("接続タイムアウト".to_owned())));
                drain_connection(con).await;
                return LoopEnd::Lost { established: false };
            }
            Step::Cmd(Some(Command::Connect { .. })) => {
                tx.log("すでに接続中です。先に切断してください");
            }
            // Disconnect指示、またはUI終了(チャネル切断)なら正規に切断する
            Step::Cmd(cmd) => {
                let ui_closed = cmd.is_none();
                tx.log("切断します");
                drain_connection(con).await;
                tx.log("切断しました");
                return if ui_closed { LoopEnd::UiClosed } else { LoopEnd::UserDisconnect };
            }
            Step::AudioOut(Some(packet)) => {
                if let Err(e) = con.send_audio(packet) {
                    tx.log(format!("音声送信エラー: {e}"));
                }
            }
            Step::AudioOut(None) => {
                tx.log("音声キャプチャが停止しました");
            }
            Step::Event(Some(Ok(StreamItem::Audio(packet)))) => {
                let from = match packet.data().data() {
                    AudioData::S2C { from, .. } | AudioData::S2CWhisper { from, .. } => {
                        ClientId(*from)
                    }
                    _ => continue,
                };
                match crate::lock(audio_handler).handle_packet(from, packet) {
                    // 新しく話し始めた相手: 保存済みの音量をキューに適用する
                    Ok(Some(new_talker)) => {
                        let name = con
                            .get_state()
                            .ok()
                            .and_then(|s| s.clients.get(&new_talker).map(|c| c.name.clone()));
                        if let Some(volume) =
                            name.and_then(|n| crate::lock(volumes).get(&n).copied())
                        {
                            let mut handler = crate::lock(audio_handler);
                            if let Some(queue) = handler.get_mut_queues().get_mut(&new_talker) {
                                queue.volume = volume;
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(e) => tracing::debug!("受信音声の処理に失敗: {e}"),
                }
            }
            Step::Event(Some(Ok(StreamItem::BookEvents(events)))) => {
                for event in &events {
                    tx.log(format!("{event:?}"));
                }
                if let Ok(state) = con.get_state() {
                    established = true;
                    tx.send(Update::Status(Status::Connected {
                        server_name: state.server.name.clone(),
                    }));
                    tx.send(Update::Snapshot(build_snapshot(state)));
                }
            }
            Step::Event(Some(Ok(_))) => {}
            Step::Event(Some(Err(e))) => {
                tracing::warn!("接続エラー: {e:?}");
                tx.send(Update::Status(Status::Error(e.to_string())));
                return LoopEnd::Lost { established };
            }
            Step::Event(None) => {
                tx.log("サーバから切断されました");
                tx.send(Update::Status(Status::Error("接続が失われました".to_owned())));
                return LoopEnd::Lost { established };
            }
        }
    }
}

fn build_snapshot(state: &tsclientlib::data::Connection) -> Vec<ChannelInfo> {
    let mut channels: Vec<_> = state.channels.iter().collect();
    channels.sort_by_key(|(id, _)| id.0);
    channels
        .into_iter()
        .map(|(id, channel)| {
            let mut clients: Vec<ClientInfo> = state
                .clients
                .iter()
                .filter(|(_, c)| c.channel == *id)
                .map(|(cid, c)| ClientInfo { id: cid.0, name: c.name.clone() })
                .collect();
            clients.sort_by(|a, b| a.name.cmp(&b.name));
            ChannelInfo { name: channel.name.clone(), clients }
        })
        .collect()
}
