#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod api;
mod audio;
mod client;
mod config;
mod tray;

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui;

use audio::VoiceMode;
use client::{ChannelInfo, ClientHandle, Command, Status, Update};
use config::Config;

fn main() -> eframe::Result {
    tracing_subscriber::fmt::init();

    // 自動起動時はトレイに格納された状態(ウィンドウ非表示)で開始する
    let minimized = std::env::args().any(|a| a == "--minimized");

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([420.0, 640.0])
            .with_visible(!minimized),
        ..Default::default()
    };
    eframe::run_native(
        "TS3 Client",
        options,
        Box::new(|cc| {
            setup_japanese_font(&cc.egui_ctx);
            Ok(Box::new(App::new(cc)))
        }),
    )
}

/// Windows起動時の自動起動(レジストリRunキー)を設定する
fn build_auto_launch() -> anyhow::Result<auto_launch::AutoLaunch> {
    let exe = std::env::current_exe()?;
    Ok(auto_launch::AutoLaunchBuilder::new()
        .set_app_name("TS3Client")
        .set_app_path(&exe.to_string_lossy())
        .set_args(&["--minimized"])
        .build()?)
}

/// eguiの標準フォントはCJK非対応のため、Windowsのシステムフォントを追加する
fn setup_japanese_font(ctx: &egui::Context) {
    let candidates = [
        "C:/Windows/Fonts/YuGothM.ttc",  // 游ゴシック Medium
        "C:/Windows/Fonts/meiryo.ttc",   // メイリオ
        "C:/Windows/Fonts/msgothic.ttc", // MSゴシック
    ];
    for path in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            let mut fonts = egui::FontDefinitions::default();
            fonts
                .font_data
                .insert("japanese".to_owned(), egui::FontData::from_owned(bytes).into());
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts
                    .families
                    .entry(family)
                    .or_default()
                    .push("japanese".to_owned());
            }
            ctx.set_fonts(fonts);
            return;
        }
    }
    eprintln!("警告: 日本語フォントが見つかりませんでした。日本語が□で表示されます");
}

const MAX_LOG_LINES: usize = 200;

/// プッシュトゥトークに選べるキー (Windows仮想キーコード, 表示名)
const PTT_KEYS: &[(i32, &str)] = &[
    (0x05, "マウス サイド1"),
    (0x06, "マウス サイド2"),
    (0xA0, "左Shift"),
    (0xA2, "左Ctrl"),
    (0xA4, "左Alt"),
    (0x1D, "無変換"),
    (0x7B, "F12"),
];

struct App {
    handle: ClientHandle,
    config: Config,
    status: Status,
    channels: Vec<ChannelInfo>,
    log: VecDeque<String>,
    input_devices: Vec<String>,
    output_devices: Vec<String>,
    /// trueのときのみウィンドウの閉じる操作で本当に終了する(通常はトレイへ格納)
    exiting: Arc<AtomicBool>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let config = Config::load();
        let (input_devices, output_devices) = audio::list_devices();

        let exiting = Arc::new(AtomicBool::new(false));
        tray::spawn(cc.egui_ctx.clone(), exiting.clone());

        // 自動起動が有効なら登録を更新する(exeの移動に追従するため毎回上書き)
        if config.auto_start {
            if let Ok(auto) = build_auto_launch() {
                let _ = auto.enable();
            }
        }

        let app = Self {
            handle: client::spawn(cc.egui_ctx.clone(), &config),
            config,
            status: Status::Disconnected,
            channels: Vec::new(),
            log: VecDeque::new(),
            input_devices,
            output_devices,
            exiting,
        };
        // 動作確認用: 起動と同時に接続する
        if std::env::args().any(|a| a == "--autoconnect") {
            let _ = app.handle.commands.send(Command::Connect {
                address: app.config.selected().address.clone(),
                nickname: app.config.nickname.clone(),
            });
        }
        app
    }

    fn apply_updates(&mut self) {
        while let Ok(update) = self.handle.updates.try_recv() {
            match update {
                Update::Status(status) => self.status = status,
                Update::Snapshot(channels) => self.channels = channels,
                Update::Log(line) => {
                    if self.log.len() >= MAX_LOG_LINES {
                        self.log.pop_front();
                    }
                    self.log.push_back(line);
                }
            }
        }
    }

    /// デバイス選択コンボボックス。変更されたらtrueを返す
    fn device_combo(
        ui: &mut egui::Ui,
        id: &str,
        label: &str,
        selected: &mut Option<String>,
        devices: &[String],
    ) -> bool {
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.label(label);
            let current = selected.as_deref().unwrap_or("(既定のデバイス)");
            egui::ComboBox::from_id_salt(id).selected_text(current).width(240.0).show_ui(
                ui,
                |ui| {
                    if ui.selectable_label(selected.is_none(), "(既定のデバイス)").clicked() {
                        if selected.is_some() {
                            *selected = None;
                            changed = true;
                        }
                    }
                    for name in devices {
                        let is_selected = selected.as_deref() == Some(name);
                        if ui.selectable_label(is_selected, name).clicked() && !is_selected {
                            *selected = Some(name.clone());
                            changed = true;
                        }
                    }
                },
            );
        });
        changed
    }

    fn voice_settings_ui(&mut self, ui: &mut egui::Ui) {
        let controls = &self.handle.audio_controls;
        let mut save = false;

        if Self::device_combo(
            ui,
            "input_device",
            "入力デバイス:",
            &mut self.config.input_device,
            &self.input_devices,
        ) {
            let _ = self
                .handle
                .audio_commands
                .send(audio::AudioCommand::SetInputDevice(self.config.input_device.clone()));
            save = true;
        }
        if Self::device_combo(
            ui,
            "output_device",
            "出力デバイス:",
            &mut self.config.output_device,
            &self.output_devices,
        ) {
            let _ = self
                .handle
                .audio_commands
                .send(audio::AudioCommand::SetOutputDevice(self.config.output_device.clone()));
            save = true;
        }

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label("送信モード:");
            for (mode, name) in [
                (VoiceMode::Always, "常時送信"),
                (VoiceMode::VoiceActivation, "ボイス検出"),
                (VoiceMode::PushToTalk, "プッシュトゥトーク"),
            ] {
                if ui.radio(self.config.voice_mode == mode, name).clicked()
                    && self.config.voice_mode != mode
                {
                    self.config.voice_mode = mode;
                    controls.set_mode(mode);
                    save = true;
                }
            }
        });

        match self.config.voice_mode {
            VoiceMode::VoiceActivation => {
                ui.horizontal(|ui| {
                    ui.label("閾値:");
                    if ui
                        .add(
                            egui::Slider::new(&mut self.config.vad_threshold_db, -70.0..=0.0)
                                .suffix(" dB"),
                        )
                        .changed()
                    {
                        controls.set_vad_threshold_db(self.config.vad_threshold_db);
                        save = true;
                    }
                });
                // 入力レベルメーター(閾値調整用)
                let level = controls.input_level_db();
                let fraction = ((level + 70.0) / 70.0).clamp(0.0, 1.0);
                ui.horizontal(|ui| {
                    ui.label("入力レベル:");
                    ui.add(
                        egui::ProgressBar::new(fraction)
                            .desired_width(200.0)
                            .text(format!("{level:.0} dB")),
                    );
                });
            }
            VoiceMode::PushToTalk => {
                ui.horizontal(|ui| {
                    ui.label("キー:");
                    let current = PTT_KEYS
                        .iter()
                        .find(|(vk, _)| *vk == self.config.ptt_vk)
                        .map(|(_, name)| *name)
                        .unwrap_or("?");
                    egui::ComboBox::from_id_salt("ptt_key").selected_text(current).show_ui(
                        ui,
                        |ui| {
                            for (vk, name) in PTT_KEYS {
                                if ui
                                    .selectable_label(self.config.ptt_vk == *vk, *name)
                                    .clicked()
                                    && self.config.ptt_vk != *vk
                                {
                                    self.config.ptt_vk = *vk;
                                    controls.set_ptt_vk(*vk);
                                    save = true;
                                }
                            }
                        },
                    );
                    ui.weak("(ウィンドウ非フォーカスでも有効)");
                });
            }
            VoiceMode::Always => {}
        }

        if save {
            self.config.save();
        }
    }

    fn server_settings_ui(&mut self, ui: &mut egui::Ui) {
        let mut save = false;
        let i = self.config.selected_server;

        ui.horizontal(|ui| {
            ui.label("表示名:");
            if ui
                .add(
                    egui::TextEdit::singleline(&mut self.config.servers[i].name)
                        .desired_width(140.0),
                )
                .lost_focus()
            {
                save = true;
            }
            ui.label("アドレス:");
            if ui
                .add(
                    egui::TextEdit::singleline(&mut self.config.servers[i].address)
                        .desired_width(140.0),
                )
                .lost_focus()
            {
                save = true;
            }
        });
        ui.horizontal(|ui| {
            if ui.button("サーバを追加").clicked() {
                self.config
                    .servers
                    .push(config::ServerEntry {
                        name: format!("サーバ{}", self.config.servers.len() + 1),
                        address: String::new(),
                    });
                self.config.selected_server = self.config.servers.len() - 1;
                save = true;
            }
            // 最後の1件は消せない(接続先が空になるのを防ぐ)
            if ui
                .add_enabled(self.config.servers.len() > 1, egui::Button::new("このサーバを削除"))
                .clicked()
            {
                self.config.servers.remove(i);
                if self.config.selected_server >= self.config.servers.len() {
                    self.config.selected_server = self.config.servers.len() - 1;
                }
                save = true;
            }
        });
        ui.weak("StreamDeckから名前指定で接続: /api/connect/表示名");

        if save {
            self.config.save();
        }
    }

    fn general_settings_ui(&mut self, ui: &mut egui::Ui) {
        let mut auto_start = self.config.auto_start;
        if ui.checkbox(&mut auto_start, "Windows起動時に自動起動(トレイに格納)").changed() {
            match build_auto_launch() {
                Ok(auto) => {
                    let result = if auto_start { auto.enable() } else { auto.disable() };
                    match result {
                        Ok(()) => {
                            self.config.auto_start = auto_start;
                            self.config.save();
                        }
                        Err(e) => self.push_log(format!("自動起動の設定に失敗: {e}")),
                    }
                }
                Err(e) => self.push_log(format!("自動起動の設定に失敗: {e}")),
            }
        }
        ui.horizontal(|ui| {
            ui.label("StreamDeck用API:");
            ui.monospace(format!("http://127.0.0.1:{}/api/", self.config.api_port));
        });
        ui.weak("connect / disconnect / mute/on / mute/off / mute/toggle / status");
    }

    fn push_log(&mut self, line: String) {
        if self.log.len() >= MAX_LOG_LINES {
            self.log.pop_front();
        }
        self.log.push_back(line);
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.apply_updates();
        // レベルメーターやPTT状態を映すため、控えめな頻度で再描画し続ける
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));

        // 閉じるボタンは終了ではなくトレイへの格納にする(トレイの「終了」からのみ終了)
        if ui.ctx().input(|i| i.viewport().close_requested())
            && !self.exiting.load(Ordering::Relaxed)
        {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }

        egui::Panel::top("connection_panel").show(ui, |ui| {
            ui.add_space(4.0);
            let connected = !matches!(self.status, Status::Disconnected | Status::Error(_));
            ui.horizontal(|ui| {
                ui.label("サーバ:");
                ui.add_enabled_ui(!connected, |ui| {
                    let selected_name = self.config.selected().name.clone();
                    egui::ComboBox::from_id_salt("server_select")
                        .selected_text(selected_name)
                        .width(160.0)
                        .show_ui(ui, |ui| {
                            for i in 0..self.config.servers.len() {
                                let is_selected = i == self.config.selected_server;
                                if ui
                                    .selectable_label(is_selected, &self.config.servers[i].name)
                                    .clicked()
                                    && !is_selected
                                {
                                    self.config.selected_server = i;
                                    self.config.save();
                                }
                            }
                        });
                });
                ui.label("名前:");
                if ui
                    .add_enabled(
                        !connected,
                        egui::TextEdit::singleline(&mut self.config.nickname).desired_width(80.0),
                    )
                    .lost_focus()
                {
                    self.config.save();
                }
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if connected {
                    if ui.button("切断").clicked() {
                        let _ = self.handle.commands.send(Command::Disconnect);
                    }
                } else if ui.button("接続").clicked() {
                    self.config.save();
                    let _ = self.handle.commands.send(Command::Connect {
                        address: self.config.selected().address.trim().to_owned(),
                        nickname: self.config.nickname.trim().to_owned(),
                    });
                }
                let muted_flag = &self.handle.audio_controls.muted;
                let mut muted = muted_flag.load(Ordering::Relaxed);
                if ui.checkbox(&mut muted, "ミュート").changed() {
                    muted_flag.store(muted, Ordering::Relaxed);
                }
                match &self.status {
                    Status::Disconnected => ui.label("未接続"),
                    Status::Connecting => ui.label("接続中..."),
                    Status::Connected { server_name } => ui.colored_label(
                        egui::Color32::from_rgb(0, 160, 60),
                        format!("接続済: {server_name}"),
                    ),
                    Status::Error(e) => ui.colored_label(
                        egui::Color32::from_rgb(200, 60, 60),
                        format!("エラー: {e}"),
                    ),
                };
            });
            ui.add_space(4.0);
            egui::CollapsingHeader::new("サーバ設定").show(ui, |ui| self.server_settings_ui(ui));
            egui::CollapsingHeader::new("音声設定").show(ui, |ui| self.voice_settings_ui(ui));
            egui::CollapsingHeader::new("一般設定").show(ui, |ui| self.general_settings_ui(ui));
            ui.add_space(4.0);
        });

        egui::Panel::bottom("log_panel")
            .resizable(true)
            .default_size(120.0)
            .show(ui, |ui| {
                ui.add_space(4.0);
                ui.label("ログ");
                egui::ScrollArea::vertical().stick_to_bottom(true).show(ui, |ui| {
                    for line in &self.log {
                        ui.small(line);
                    }
                });
            });

        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                if self.channels.is_empty() {
                    ui.weak("(チャンネル情報なし)");
                }
                for channel in &self.channels {
                    ui.label(egui::RichText::new(format!("📁 {}", channel.name)).strong());
                    for client in &channel.clients {
                        ui.horizontal(|ui| {
                            ui.add_space(20.0);
                            ui.label(format!("👤 {client}"));
                        });
                    }
                }
            });
        });
    }

    fn on_exit(&mut self) {
        self.config.save();
        // ウィンドウを閉じたときも正規の切断処理を試みる
        let _ = self.handle.commands.send(Command::Disconnect);
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
}
