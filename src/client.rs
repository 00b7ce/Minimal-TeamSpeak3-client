//! TS3サーバとの接続をバックグラウンドスレッドで管理するモジュール。
//!
//! UIスレッドとは2本のチャネルでやり取りする:
//! - `Command` (UI → ワーカー): 接続/切断の指示
//! - `Update` (ワーカー → UI): 接続状態・チャンネル一覧・ログ

use eframe::egui;
use futures::prelude::*;
use tsclientlib::{Connection, DisconnectOptions, StreamItem};

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
pub struct ChannelInfo {
    pub name: String,
    pub clients: Vec<String>,
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
}

/// ワーカースレッドを起動する。`ctx`はUpdate送信時の再描画要求に使う。
pub fn spawn(ctx: egui::Context) -> ClientHandle {
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    let (update_tx, update_rx) = std::sync::mpsc::channel();

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokioランタイムの作成に失敗");
        rt.block_on(run_worker(cmd_rx, update_tx, ctx));
    });

    ClientHandle { commands: cmd_tx, updates: update_rx }
}

struct Sender {
    tx: std::sync::mpsc::Sender<Update>,
    ctx: egui::Context,
}

impl Sender {
    fn send(&self, update: Update) {
        let _ = self.tx.send(update);
        self.ctx.request_repaint();
    }

    fn log(&self, message: impl Into<String>) {
        self.send(Update::Log(message.into()));
    }
}

async fn run_worker(
    mut cmd_rx: tokio::sync::mpsc::UnboundedReceiver<Command>,
    tx: std::sync::mpsc::Sender<Update>,
    ctx: egui::Context,
) {
    let tx = Sender { tx, ctx };

    loop {
        // 未接続: Connectコマンドを待つ
        let (address, nickname) = loop {
            match cmd_rx.recv().await {
                Some(Command::Connect { address, nickname }) => break (address, nickname),
                Some(Command::Disconnect) => {}
                None => return, // UI終了
            }
        };

        tx.send(Update::Status(Status::Connecting));
        tx.log(format!("{address} に {nickname} として接続します"));

        let mut con = match Connection::build(address).name(nickname).connect() {
            Ok(con) => con,
            Err(e) => {
                tx.send(Update::Status(Status::Error(e.to_string())));
                continue;
            }
        };

        // 接続中: イベントとコマンドを並行して処理する
        let exit = connected_loop(&mut con, &mut cmd_rx, &tx).await;

        tx.send(Update::Status(Status::Disconnected));
        tx.send(Update::Snapshot(Vec::new()));
        if exit {
            return;
        }
    }
}

/// 接続中のメインループ。UI終了時はtrueを返す。
async fn connected_loop(
    con: &mut Connection,
    cmd_rx: &mut tokio::sync::mpsc::UnboundedReceiver<Command>,
    tx: &Sender,
) -> bool {
    loop {
        enum Step {
            Cmd(Option<Command>),
            Event(Option<Result<StreamItem, tsclientlib::Error>>),
        }
        // con.events()のストリームはselect!のスコープ内でのみ生存させ、
        // 分岐を抜けたあとにget_state()でconを再借用できるようにする
        let step = tokio::select! {
            cmd = cmd_rx.recv() => Step::Cmd(cmd),
            item = async { con.events().next().await } => Step::Event(item),
        };

        match step {
            Step::Cmd(Some(Command::Connect { .. })) => {
                tx.log("すでに接続中です。先に切断してください");
            }
            // Disconnect指示、またはUI終了(チャネル切断)なら正規に切断する
            Step::Cmd(cmd) => {
                let ui_closed = cmd.is_none();
                tx.log("切断します");
                if con.disconnect(DisconnectOptions::new()).is_ok() {
                    con.events().for_each(|_| future::ready(())).await;
                }
                tx.log("切断しました");
                return ui_closed;
            }
            Step::Event(Some(Ok(StreamItem::BookEvents(events)))) => {
                for event in &events {
                    tx.log(format!("{event:?}"));
                }
                if let Ok(state) = con.get_state() {
                    tx.send(Update::Status(Status::Connected {
                        server_name: state.server.name.clone(),
                    }));
                    tx.send(Update::Snapshot(build_snapshot(state)));
                }
            }
            Step::Event(Some(Ok(_))) => {}
            Step::Event(Some(Err(e))) => {
                tx.send(Update::Status(Status::Error(e.to_string())));
                return false;
            }
            Step::Event(None) => {
                tx.log("サーバから切断されました");
                return false;
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
            let mut clients: Vec<String> = state
                .clients
                .values()
                .filter(|c| c.channel == *id)
                .map(|c| c.name.clone())
                .collect();
            clients.sort();
            ChannelInfo { name: channel.name.clone(), clients }
        })
        .collect()
}
