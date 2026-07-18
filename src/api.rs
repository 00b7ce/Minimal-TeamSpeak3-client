//! StreamDeck連携用のローカルHTTP API。
//!
//! `http://127.0.0.1:{port}` で待ち受ける(LAN外には公開しない)。
//! StreamDeck側はHTTPリクエストを送れるプラグイン(API Ninja等)から叩く想定。
//! GET/POST両対応にしてブラウザからの動作確認も可能にしている。
//!
//! エンドポイント:
//! - `/api/connect`        選択中のサーバへ接続
//! - `/api/connect/{name}`  登録済みサーバへ表示名を指定して接続
//! - `/api/disconnect`  切断
//! - `/api/mute/on` `/api/mute/off` `/api/mute/toggle`  マイクミュート操作
//! - `/api/status`      接続状態とミュート状態をJSONで返す

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::get;
use serde::Serialize;

use crate::audio::Controls;
use crate::client::{Command, Status};

pub struct ApiState {
    pub commands: tokio::sync::mpsc::UnboundedSender<Command>,
    pub controls: Arc<Controls>,
    pub status: Arc<Mutex<Status>>,
    pub log: Box<dyn Fn(String) + Send + Sync>,
}

pub async fn serve(port: u16, state: Arc<ApiState>) {
    let router = axum::Router::new()
        .route("/api/connect", get(connect).post(connect))
        .route("/api/connect/{name}", get(connect_named).post(connect_named))
        .route("/api/disconnect", get(disconnect).post(disconnect))
        .route("/api/mute/{action}", get(mute).post(mute))
        .route("/api/status", get(status))
        .with_state(state.clone());

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => {
            (state.log)(format!("StreamDeck用API待受中: http://127.0.0.1:{port}/api/"));
            if let Err(e) = axum::serve(listener, router).await {
                (state.log)(format!("APIサーバエラー: {e}"));
            }
        }
        Err(e) => (state.log)(format!("APIサーバの起動に失敗 (ポート{port}): {e}")),
    }
}

async fn connect(State(state): State<Arc<ApiState>>) -> Result<&'static str, StatusCode> {
    // プロファイルはUIが保存した最新の設定ファイルから読む
    let cfg = crate::config::Config::load();
    request_connect(&state, cfg.selected())
}

/// プロファイルの内容を確認して接続要求を送る
fn request_connect(
    state: &ApiState,
    profile: &crate::config::Profile,
) -> Result<&'static str, StatusCode> {
    if profile.address.trim().is_empty() || profile.nickname.trim().is_empty() {
        (state.log)(format!(
            "API: プロファイル「{}」はアドレスまたはニックネームが未設定です",
            profile.name
        ));
        return Err(StatusCode::BAD_REQUEST);
    }
    (state.log)(format!("API: 接続要求 ({})", profile.name));
    let _ = state.commands.send(Command::Connect {
        address: profile.address.trim().to_owned(),
        nickname: profile.nickname.trim().to_owned(),
    });
    Ok("connecting\n")
}

async fn connect_named(
    State(state): State<Arc<ApiState>>,
    Path(name): Path<String>,
) -> Result<&'static str, StatusCode> {
    let cfg = crate::config::Config::load();
    let Some(profile) = cfg.profiles.iter().find(|p| p.name == name) else {
        (state.log)(format!("API: 未登録のプロファイル名「{name}」への接続要求"));
        return Err(StatusCode::NOT_FOUND);
    };
    request_connect(&state, profile)
}

async fn disconnect(State(state): State<Arc<ApiState>>) -> &'static str {
    (state.log)("API: 切断要求".to_owned());
    let _ = state.commands.send(Command::Disconnect);
    "disconnecting\n"
}

async fn mute(
    State(state): State<Arc<ApiState>>,
    Path(action): Path<String>,
) -> Result<String, StatusCode> {
    let muted = &state.controls.muted;
    match action.as_str() {
        "on" => muted.store(true, Ordering::Relaxed),
        "off" => muted.store(false, Ordering::Relaxed),
        "toggle" => {
            muted.fetch_xor(true, Ordering::Relaxed);
        }
        _ => return Err(StatusCode::NOT_FOUND),
    }
    let now = muted.load(Ordering::Relaxed);
    (state.log)(format!("API: ミュート {}", if now { "ON" } else { "OFF" }));
    Ok(format!("muted={now}\n"))
}

#[derive(Serialize)]
struct StatusResponse {
    status: &'static str,
    server_name: Option<String>,
    error: Option<String>,
    muted: bool,
}

async fn status(State(state): State<Arc<ApiState>>) -> Json<StatusResponse> {
    let (status, server_name, error) = match &*crate::lock(&state.status) {
        Status::Disconnected => ("disconnected", None, None),
        Status::Connecting => ("connecting", None, None),
        Status::Connected { server_name } => ("connected", Some(server_name.clone()), None),
        Status::Error(e) => ("error", None, Some(e.clone())),
    };
    Json(StatusResponse {
        status,
        server_name,
        error,
        muted: state.controls.muted.load(Ordering::Relaxed),
    })
}
