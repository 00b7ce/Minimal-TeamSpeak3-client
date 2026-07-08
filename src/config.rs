//! 設定ファイルの読み書き。
//!
//! 保存先: `%APPDATA%\ts3-client\config.toml`
//! 読み込み失敗時(初回起動・壊れたファイル)は既定値で開始する。

use serde::{Deserialize, Serialize};

use crate::audio::VoiceMode;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub address: String,
    pub nickname: String,
    /// Noneは「既定のデバイス」
    pub input_device: Option<String>,
    pub output_device: Option<String>,
    pub voice_mode: VoiceMode,
    /// ボイスアクティベーションの閾値(dBFS)
    pub vad_threshold_db: f32,
    /// プッシュトゥトークのキー(Windows仮想キーコード)
    pub ptt_vk: i32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            address: "192.168.10.8".to_owned(),
            nickname: "mekabu".to_owned(),
            input_device: None,
            output_device: None,
            voice_mode: VoiceMode::Always,
            vad_threshold_db: -40.0,
            ptt_vk: 0x05, // マウス サイド1 (XButton1)
        }
    }
}

fn config_path() -> Option<std::path::PathBuf> {
    Some(dirs::config_dir()?.join("ts3-client").join("config.toml"))
}

impl Config {
    pub fn load() -> Self {
        let Some(path) = config_path() else { return Self::default() };
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
                tracing::warn!("設定ファイルの解析に失敗、既定値を使用: {e}");
                Self::default()
            }),
            Err(_) => Self::default(), // 初回起動など
        }
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
