//! 設定ファイルの読み書き。
//!
//! 保存先: `%APPDATA%\ts3-client\config.toml`
//! 読み込み失敗時(初回起動・壊れたファイル)は既定値で開始する。

use serde::{Deserialize, Serialize};

use crate::audio::VoiceMode;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerEntry {
    /// UI・APIで指定する表示名
    pub name: String,
    pub address: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub servers: Vec<ServerEntry>,
    pub selected_server: usize,
    /// 旧形式(単一アドレス)からの移行用。読み込み時のみ使用し、保存しない
    #[serde(skip_serializing)]
    address: Option<String>,
    pub nickname: String,
    /// Noneは「既定のデバイス」
    pub input_device: Option<String>,
    pub output_device: Option<String>,
    pub voice_mode: VoiceMode,
    /// ボイスアクティベーションの閾値(dBFS)
    pub vad_threshold_db: f32,
    /// プッシュトゥトークのキー(Windows仮想キーコード)
    pub ptt_vk: i32,
    /// StreamDeck連携用ローカルHTTP APIのポート
    pub api_port: u16,
    /// Windows起動時に自動起動する(トレイ最小化状態で開始)
    pub auto_start: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            servers: vec![ServerEntry {
                name: "自宅サーバ".to_owned(),
                address: "192.168.10.8".to_owned(),
            }],
            selected_server: 0,
            address: None,
            nickname: "mekabu".to_owned(),
            input_device: None,
            output_device: None,
            voice_mode: VoiceMode::Always,
            vad_threshold_db: -40.0,
            ptt_vk: 0x05, // マウス サイド1 (XButton1)
            api_port: 9871,
            auto_start: false,
        }
    }
}

fn config_path() -> Option<std::path::PathBuf> {
    Some(dirs::config_dir()?.join("ts3-client").join("config.toml"))
}

impl Config {
    pub fn load() -> Self {
        let Some(path) = config_path() else { return Self::default() };
        let config = match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
                tracing::warn!("設定ファイルの解析に失敗、既定値を使用: {e}");
                Self::default()
            }),
            Err(_) => Self::default(), // 初回起動など
        };
        config.normalize()
    }

    /// 旧形式からの移行と不正値の補正
    fn normalize(mut self) -> Self {
        if self.servers.is_empty() {
            let address =
                self.address.take().unwrap_or_else(|| "192.168.10.8".to_owned());
            self.servers.push(ServerEntry { name: "サーバ1".to_owned(), address });
        }
        if self.selected_server >= self.servers.len() {
            self.selected_server = 0;
        }
        self
    }

    /// 現在選択中のサーバ(serversは常に1件以上ある)
    pub fn selected(&self) -> &ServerEntry {
        &self.servers[self.selected_server.min(self.servers.len() - 1)]
    }

    pub fn save(&self) {
        let Some(path) = config_path() else { return };
        let Ok(text) = toml::to_string_pretty(self) else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = std::fs::write(&path, text) {
            tracing::warn!("設定ファイルの保存に失敗: {e}");
        }
    }
}
