//! 設定ファイルの読み書き。
//!
//! 保存先: `%APPDATA%\ts3-client\config.toml`
//! 読み込み失敗時(初回起動・壊れたファイル)は既定値で開始する。

use serde::{Deserialize, Serialize};

use crate::audio::VoiceMode;

/// 接続先プロファイル(サーバとニックネームの組)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    /// UI・APIで指定する表示名
    pub name: String,
    pub address: String,
    pub nickname: String,
}

/// 旧形式(servers + グローバルnickname)の読み込み用
#[derive(Debug, Clone, Deserialize)]
struct LegacyServerEntry {
    #[serde(default)]
    name: String,
    #[serde(default)]
    address: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub profiles: Vec<Profile>,
    pub selected_profile: usize,
    // ---- 以下3つは旧形式からの移行用。読み込み時のみ使用し、保存しない ----
    #[serde(skip_serializing)]
    servers: Vec<LegacyServerEntry>,
    #[serde(skip_serializing)]
    selected_server: usize,
    #[serde(skip_serializing)]
    nickname: String,
    #[serde(skip_serializing)]
    address: Option<String>,
    // ----
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
    /// 相手ごとの再生音量(ニックネーム → 倍率1.0=100%)。IDはセッションごとに変わるため名前で持つ
    pub volumes: std::collections::HashMap<String, f32>,
    /// UI言語。NoneはOSの言語設定に従う(ユーザーが明示的に選ぶとSomeになる)
    pub language: Option<crate::i18n::Lang>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            // 空のままにしておき、normalize()が移行または初期プロファイル作成を行う
            profiles: Vec::new(),
            selected_profile: 0,
            servers: Vec::new(),
            selected_server: 0,
            nickname: String::new(),
            address: None,
            input_device: None,
            output_device: None,
            voice_mode: VoiceMode::Always,
            vad_threshold_db: -40.0,
            ptt_vk: 0x05, // マウス サイド1 (XButton1)
            api_port: 9871,
            auto_start: false,
            volumes: std::collections::HashMap::new(),
            language: None,
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
        if self.profiles.is_empty() {
            let nickname = std::mem::take(&mut self.nickname);
            if !self.servers.is_empty() {
                // 旧形式: [[servers]] + グローバルnickname
                self.profiles = self
                    .servers
                    .drain(..)
                    .map(|s| Profile { name: s.name, address: s.address, nickname: nickname.clone() })
                    .collect();
                self.selected_profile = self.selected_server;
            } else {
                // さらに旧い形式(単一address)または初回起動。
                // 初回は空のプロファイルを作り、ユーザーが設定画面で埋める
                let address = self.address.take().unwrap_or_default();
                self.profiles.push(Profile { name: "Profile1".to_owned(), address, nickname });
            }
        }
        if self.selected_profile >= self.profiles.len() {
            self.selected_profile = 0;
        }
        self
    }

    /// 現在選択中のプロファイル(profilesは常に1件以上ある)
    pub fn selected(&self) -> &Profile {
        &self.profiles[self.selected_profile.min(self.profiles.len() - 1)]
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
