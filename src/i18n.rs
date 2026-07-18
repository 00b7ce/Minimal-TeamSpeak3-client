//! UI文言の多言語対応(日本語/英語)。
//!
//! - 現在の言語はグローバルに保持し、`t()`で対訳表(`Texts`)を引く。
//!   実行中に`set()`で切り替えると、次のフレームからUI全体に反映される
//! - 対訳は`Texts`構造体の定数2つ(`EN`/`JA`)。フィールドを増やすと
//!   両方に訳を書かない限りコンパイルが通らないため、訳漏れが起きない
//! - 深部の内部エラー(anyhowのコンテキスト等)は英語固定とする方針
//!   (発生頻度が低く、issue報告時に英語のほうが扱いやすいため)

use std::sync::atomic::{AtomicU8, Ordering};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Lang {
    En,
    Ja,
}

static CURRENT: AtomicU8 = AtomicU8::new(0);

pub fn set(lang: Lang) {
    CURRENT.store(lang as u8, Ordering::Relaxed);
}

pub fn lang() -> Lang {
    if CURRENT.load(Ordering::Relaxed) == Lang::Ja as u8 { Lang::Ja } else { Lang::En }
}

/// 現在の言語の対訳表
pub fn t() -> &'static Texts {
    match lang() {
        Lang::En => &EN,
        Lang::Ja => &JA,
    }
}

/// WindowsのUI言語が日本語なら`Ja`、それ以外は`En`
pub fn detect_os_lang() -> Lang {
    // SAFETY: 引数なしの読み取り専用API
    let id = unsafe { windows_sys::Win32::Globalization::GetUserDefaultUILanguage() };
    const LANG_JAPANESE: u16 = 0x11;
    if id & 0x3FF == LANG_JAPANESE { Lang::Ja } else { Lang::En }
}

/// テンプレート中の`{0}` `{1}`...を順に置換する簡易フォーマッタ
pub fn fmt(template: &str, args: &[&str]) -> String {
    let mut out = template.to_owned();
    for (i, arg) in args.iter().enumerate() {
        out = out.replace(&format!("{{{i}}}"), arg);
    }
    out
}

pub struct Texts {
    // ---- メインウィンドウ ----
    pub profile_label: &'static str,
    pub connect: &'static str,
    pub disconnect: &'static str,
    pub mute: &'static str,
    pub settings_button: &'static str,
    pub status_disconnected: &'static str,
    pub status_connecting: &'static str,
    /// {0}=サーバ名
    pub status_connected: &'static str,
    /// {0}=エラー内容
    pub status_error: &'static str,
    pub log_heading: &'static str,
    pub no_channels: &'static str,
    pub volume_hover: &'static str,
    /// {0}=ニックネーム
    pub volume_of: &'static str,
    pub reset_100: &'static str,
    pub connect_hint: &'static str,

    // ---- 設定ウィンドウ ----
    pub settings_title: &'static str,
    pub profile_settings: &'static str,
    pub audio_settings: &'static str,
    pub general_settings: &'static str,
    pub col_profile_name: &'static str,
    pub col_address: &'static str,
    pub col_nickname: &'static str,
    pub delete_profile: &'static str,
    pub add_profile: &'static str,
    pub api_hint: &'static str,
    pub input_device: &'static str,
    pub output_device: &'static str,
    pub default_device: &'static str,
    pub voice_mode: &'static str,
    pub mode_always: &'static str,
    pub mode_vad: &'static str,
    pub mode_ptt: &'static str,
    pub vad_threshold: &'static str,
    pub input_level: &'static str,
    pub ptt_key: &'static str,
    pub ptt_note: &'static str,
    pub auto_start: &'static str,
    /// {0}=エラー内容
    pub auto_start_failed: &'static str,
    pub api_label: &'static str,
    pub language_label: &'static str,
    pub show_log: &'static str,

    // ---- トレイ ----
    pub tray_open: &'static str,
    pub tray_exit: &'static str,

    // ---- 接続ワーカーのログ ----
    /// {0}=アドレス {1}=ニックネーム
    pub connecting_to: &'static str,
    pub already_connected: &'static str,
    pub disconnecting: &'static str,
    pub disconnected_done: &'static str,
    pub server_closed: &'static str,
    pub connection_lost: &'static str,
    pub handshake_timeout_log: &'static str,
    pub handshake_timeout_status: &'static str,
    /// {0}=エラー内容
    pub send_audio_error: &'static str,
    pub capture_stopped: &'static str,
    /// {0}=試行上限
    pub reconnect_giveup: &'static str,
    /// {0}=秒 {1}=試行回数 {2}=上限
    pub reconnect_in: &'static str,
    pub reconnect_cancel: &'static str,

    // ---- 音声デバイスのログ ----
    /// {0}=デバイス名 {1}=サンプルレート {2}=チャンネル数
    pub playback_device: &'static str,
    /// {0}=デバイス名 {1}=サンプルレート {2}=チャンネル数
    pub capture_device: &'static str,
    /// {0}=デバイス名
    pub device_fallback: &'static str,
    /// {0}=エラー内容
    pub playback_init_failed: &'static str,
    pub capture_init_failed: &'static str,
    pub playback_switch_failed: &'static str,
    pub capture_switch_failed: &'static str,
    pub playback_rebuild: &'static str,
    pub capture_rebuild: &'static str,
    /// {0}=エラー内容
    pub playback_rebuild_failed: &'static str,
    pub capture_rebuild_failed: &'static str,

    // ---- HTTP APIのログ ----
    /// {0}=URL
    pub api_listening: &'static str,
    /// {0}=エラー内容
    pub api_error: &'static str,
    /// {0}=ポート {1}=エラー内容
    pub api_bind_failed: &'static str,
    /// {0}=プロファイル名
    pub api_connect_req: &'static str,
    pub api_disconnect_req: &'static str,
    /// {0}=ON/OFF
    pub api_mute: &'static str,
    /// {0}=プロファイル名
    pub api_unknown_profile: &'static str,
    /// {0}=プロファイル名
    pub api_profile_incomplete: &'static str,

    // ---- プッシュトゥトークのキー名 ----
    pub key_mouse_side1: &'static str,
    pub key_mouse_side2: &'static str,
    pub key_lshift: &'static str,
    pub key_lctrl: &'static str,
    pub key_lalt: &'static str,
    pub key_muhenkan: &'static str,
    pub key_f12: &'static str,
}

pub static EN: Texts = Texts {
    profile_label: "Profile:",
    connect: "Connect",
    disconnect: "Disconnect",
    mute: "Mute",
    settings_button: "⚙ Settings",
    status_disconnected: "Not connected",
    status_connecting: "Connecting...",
    status_connected: "Connected: {0}",
    status_error: "Error: {0}",
    log_heading: "Log",
    no_channels: "(no channel data)",
    volume_hover: "Right-click to adjust volume",
    volume_of: "Volume of {0}",
    reset_100: "Reset to 100%",
    connect_hint: "Set the profile address and nickname in Settings first",

    settings_title: "Settings - TS3 Client",
    profile_settings: "Profiles",
    audio_settings: "Audio",
    general_settings: "General",
    col_profile_name: "Profile name",
    col_address: "Address",
    col_nickname: "Nickname",
    delete_profile: "Delete this profile",
    add_profile: "Add a profile",
    api_hint: "Connect by name from StreamDeck: /api/connect/<profile name>",
    input_device: "Input device:",
    output_device: "Output device:",
    default_device: "(default device)",
    voice_mode: "Transmission:",
    mode_always: "Continuous",
    mode_vad: "Voice activation",
    mode_ptt: "Push to talk",
    vad_threshold: "Threshold:",
    input_level: "Input level:",
    ptt_key: "Key:",
    ptt_note: "(works while the window is unfocused)",
    auto_start: "Start with Windows (minimized to tray)",
    auto_start_failed: "Failed to configure autostart: {0}",
    api_label: "StreamDeck API:",
    language_label: "Language:",
    show_log: "Show log panel",

    tray_open: "Open",
    tray_exit: "Exit",

    connecting_to: "Connecting to {0} as {1}",
    already_connected: "Already connected. Disconnect first",
    disconnecting: "Disconnecting",
    disconnected_done: "Disconnected",
    server_closed: "Disconnected by the server",
    connection_lost: "Connection lost",
    handshake_timeout_log: "Connection timeout: no response from the server",
    handshake_timeout_status: "Connection timeout",
    send_audio_error: "Failed to send audio: {0}",
    capture_stopped: "Audio capture stopped",
    reconnect_giveup: "Gave up after {0} reconnect attempts",
    reconnect_in: "Reconnecting in {0}s ({1}/{2})",
    reconnect_cancel: "Reconnect cancelled",

    playback_device: "Playback device: {0} ({1}Hz {2}ch)",
    capture_device: "Capture device: {0} ({1}Hz {2}ch)",
    device_fallback: "Device \"{0}\" not found, using the default device",
    playback_init_failed: "Failed to initialize the playback device: {0}",
    capture_init_failed: "Failed to initialize the capture device: {0}",
    playback_switch_failed: "Failed to switch the playback device: {0}",
    capture_switch_failed: "Failed to switch the capture device: {0}",
    playback_rebuild: "Playback stream error detected, rebuilding",
    capture_rebuild: "Capture stream error detected, rebuilding",
    playback_rebuild_failed: "Failed to rebuild the playback stream: {0} (pick a device again in the audio settings)",
    capture_rebuild_failed: "Failed to rebuild the capture stream: {0} (pick a device again in the audio settings)",

    api_listening: "StreamDeck API listening at {0}",
    api_error: "API server error: {0}",
    api_bind_failed: "Failed to start the API server (port {0}): {1}",
    api_connect_req: "API: connect request ({0})",
    api_disconnect_req: "API: disconnect request",
    api_mute: "API: mute {0}",
    api_unknown_profile: "API: connect request for unknown profile \"{0}\"",
    api_profile_incomplete: "API: profile \"{0}\" is missing an address or nickname",

    key_mouse_side1: "Mouse Side 1",
    key_mouse_side2: "Mouse Side 2",
    key_lshift: "Left Shift",
    key_lctrl: "Left Ctrl",
    key_lalt: "Left Alt",
    key_muhenkan: "Muhenkan (JP key)",
    key_f12: "F12",
};

pub static JA: Texts = Texts {
    profile_label: "プロファイル:",
    connect: "接続",
    disconnect: "切断",
    mute: "ミュート",
    settings_button: "⚙ 設定",
    status_disconnected: "未接続",
    status_connecting: "接続中...",
    status_connected: "接続済: {0}",
    status_error: "エラー: {0}",
    log_heading: "ログ",
    no_channels: "(チャンネル情報なし)",
    volume_hover: "右クリックで音量調整",
    volume_of: "{0} の音量",
    reset_100: "100%に戻す",
    connect_hint: "設定でプロファイルのアドレスとニックネームを入力してください",

    settings_title: "設定 - TS3 Client",
    profile_settings: "プロファイル設定",
    audio_settings: "音声設定",
    general_settings: "一般設定",
    col_profile_name: "プロファイル名",
    col_address: "アドレス",
    col_nickname: "ニックネーム",
    delete_profile: "このプロファイルを削除",
    add_profile: "プロファイルを追加",
    api_hint: "StreamDeckから名前指定で接続: /api/connect/プロファイル名",
    input_device: "入力デバイス:",
    output_device: "出力デバイス:",
    default_device: "(既定のデバイス)",
    voice_mode: "送信モード:",
    mode_always: "常時送信",
    mode_vad: "ボイス検出",
    mode_ptt: "プッシュトゥトーク",
    vad_threshold: "閾値:",
    input_level: "入力レベル:",
    ptt_key: "キー:",
    ptt_note: "(ウィンドウ非フォーカスでも有効)",
    auto_start: "Windows起動時に自動起動(トレイに格納)",
    auto_start_failed: "自動起動の設定に失敗: {0}",
    api_label: "StreamDeck用API:",
    language_label: "言語:",
    show_log: "ログを表示",

    tray_open: "開く",
    tray_exit: "終了",

    connecting_to: "{0} に {1} として接続します",
    already_connected: "すでに接続中です。先に切断してください",
    disconnecting: "切断します",
    disconnected_done: "切断しました",
    server_closed: "サーバから切断されました",
    connection_lost: "接続が失われました",
    handshake_timeout_log: "接続タイムアウト: サーバからの応答がありません",
    handshake_timeout_status: "接続タイムアウト",
    send_audio_error: "音声送信エラー: {0}",
    capture_stopped: "音声キャプチャが停止しました",
    reconnect_giveup: "再接続を{0}回試みましたが失敗しました",
    reconnect_in: "{0}秒後に再接続します ({1}/{2})",
    reconnect_cancel: "再接続を中止します",

    playback_device: "再生デバイス: {0} ({1}Hz {2}ch)",
    capture_device: "録音デバイス: {0} ({1}Hz {2}ch)",
    device_fallback: "デバイス「{0}」が見つからないため既定のデバイスを使います",
    playback_init_failed: "再生デバイスの初期化に失敗: {0}",
    capture_init_failed: "録音デバイスの初期化に失敗: {0}",
    playback_switch_failed: "再生デバイスの切替に失敗: {0}",
    capture_switch_failed: "録音デバイスの切替に失敗: {0}",
    playback_rebuild: "再生ストリームのエラーを検出、作り直します",
    capture_rebuild: "録音ストリームのエラーを検出、作り直します",
    playback_rebuild_failed: "再生ストリームの再作成に失敗: {0} (音声設定からデバイスを選び直してください)",
    capture_rebuild_failed: "録音ストリームの再作成に失敗: {0} (音声設定からデバイスを選び直してください)",

    api_listening: "StreamDeck用API待受中: {0}",
    api_error: "APIサーバエラー: {0}",
    api_bind_failed: "APIサーバの起動に失敗 (ポート{0}): {1}",
    api_connect_req: "API: 接続要求 ({0})",
    api_disconnect_req: "API: 切断要求",
    api_mute: "API: ミュート {0}",
    api_unknown_profile: "API: 未登録のプロファイル名「{0}」への接続要求",
    api_profile_incomplete: "API: プロファイル「{0}」はアドレスまたはニックネームが未設定です",

    key_mouse_side1: "マウス サイド1",
    key_mouse_side2: "マウス サイド2",
    key_lshift: "左Shift",
    key_lctrl: "左Ctrl",
    key_lalt: "左Alt",
    key_muhenkan: "無変換",
    key_f12: "F12",
};
