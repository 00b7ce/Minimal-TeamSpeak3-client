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

/// Mutexのロック。パニックで毒化されていても続行する
/// (音声コールバックスレッドのパニックが他スレッドへ連鎖して全体が止まるのを防ぐ)
pub fn lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// ロックを試み、他スレッドが保持中ならNoneを返す(毒化は無視して取得)。
/// 音声コールバックスレッドがデバイス障害でロックを持ったままハングしても、
/// 呼び出し側(接続ワーカーやUI)を巻き込んで凍結させないために使う。
pub fn try_lock<T>(mutex: &std::sync::Mutex<T>) -> Option<std::sync::MutexGuard<'_, T>> {
    match mutex.try_lock() {
        Ok(guard) => Some(guard),
        Err(std::sync::TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
        Err(std::sync::TryLockError::WouldBlock) => None,
    }
}

/// コンソールとログファイル(%APPDATA%\ts3-client\ts3-client.log)の両方へ出力する
fn init_logging() {
    use std::io::Write;

    let file = (|| {
        let dir = dirs::config_dir()?.join("ts3-client");
        std::fs::create_dir_all(&dir).ok()?;
        let path = dir.join("ts3-client.log");
        // 肥大化したら作り直す
        if std::fs::metadata(&path).map(|m| m.len() > 5 * 1024 * 1024).unwrap_or(false) {
            let _ = std::fs::remove_file(&path);
        }
        std::fs::OpenOptions::new().create(true).append(true).open(path).ok()
    })()
    .map(|f| Arc::new(std::sync::Mutex::new(f)));

    struct Tee {
        file: Option<Arc<std::sync::Mutex<std::fs::File>>>,
    }
    impl Write for Tee {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let _ = std::io::stderr().write_all(buf);
            if let Some(file) = &self.file {
                let _ = lock(file).write_all(buf);
            }
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            if let Some(file) = &self.file {
                let _ = lock(file).flush();
            }
            Ok(())
        }
    }

    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(move || Tee { file: file.clone() })
        .init();

    // どのスレッドで落ちてもログファイルに残す
    std::panic::set_hook(Box::new(|info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        tracing::error!("パニック発生: {info}\n{backtrace}");
    }));
}

fn main() -> eframe::Result {
    init_logging();

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
        Box::new(move |cc| {
            setup_japanese_font(&cc.egui_ctx);
            Ok(Box::new(App::new(cc, minimized)))
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

struct VolumeChange {
    name: String,
    client_id: u16,
    volume: f32,
    save: bool,
}

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
    /// 設定ウィンドウ(別ビューポート)を表示中か
    settings_open: bool,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, minimized: bool) -> Self {
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
            settings_open: false,
        };

        // 最小化起動: eframeは初回フレーム描画後に必ずウィンドウを表示してしまう
        // (with_visible(false)は白フラッシュ対策で上書きされる)ため、
        // 少し遅らせて非表示コマンドを送り、トレイ格納状態にする
        if minimized {
            let ctx = cc.egui_ctx.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(700));
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                ctx.request_repaint();
            });
        }
        // 動作確認用: 起動と同時に接続する
        if std::env::args().any(|a| a == "--autoconnect") {
            let _ = app.handle.commands.send(Command::Connect {
                address: app.config.selected().address.clone(),
                nickname: app.config.selected().nickname.clone(),
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

    /// 全プロファイルを一覧表示し、その場で編集/追加/削除する
    /// (メインウィンドウの選択状態とは独立して動作する)
    fn profile_settings_ui(&mut self, ui: &mut egui::Ui) {
        let mut save = false;
        let mut delete: Option<usize> = None;

        egui::Grid::new("profiles_grid").num_columns(4).spacing([8.0, 4.0]).show(ui, |ui| {
            ui.strong("プロファイル名");
            ui.strong("アドレス");
            ui.strong("ニックネーム");
            ui.label("");
            ui.end_row();

            let count = self.config.profiles.len();
            let row_height = ui.spacing().interact_size.y;
            for (i, profile) in self.config.profiles.iter_mut().enumerate() {
                // Gridセル内ではdesired_widthが無視されるため、add_sizedで幅を確保する
                if ui
                    .add_sized([130.0, row_height], egui::TextEdit::singleline(&mut profile.name))
                    .lost_focus()
                {
                    save = true;
                }
                if ui
                    .add_sized(
                        [220.0, row_height],
                        egui::TextEdit::singleline(&mut profile.address),
                    )
                    .lost_focus()
                {
                    save = true;
                }
                if ui
                    .add_sized(
                        [110.0, row_height],
                        egui::TextEdit::singleline(&mut profile.nickname),
                    )
                    .lost_focus()
                {
                    save = true;
                }
                // 最後の1件は消せない(接続先が空になるのを防ぐ)
                if ui
                    .add_enabled(count > 1, egui::Button::new("－"))
                    .on_hover_text("このプロファイルを削除")
                    .clicked()
                {
                    delete = Some(i);
                }
                ui.end_row();
            }
        });

        if ui.button("＋").on_hover_text("プロファイルを追加").clicked() {
            // ニックネームは使い回すことが多いので直前の値を引き継ぐ
            let nickname =
                self.config.profiles.last().map(|p| p.nickname.clone()).unwrap_or_default();
            self.config.profiles.push(config::Profile {
                name: format!("Profile{}", self.config.profiles.len() + 1),
                address: String::new(),
                nickname,
            });
            save = true;
        }

        if let Some(i) = delete {
            self.config.profiles.remove(i);
            // メインウィンドウの選択位置がずれないよう補正する
            if i < self.config.selected_profile {
                self.config.selected_profile -= 1;
            } else if self.config.selected_profile >= self.config.profiles.len() {
                self.config.selected_profile = self.config.profiles.len() - 1;
            }
            save = true;
        }

        ui.weak("StreamDeckから名前指定で接続: /api/connect/プロファイル名");

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

    /// 音量変更を設定・共有マップ・再生中キューの3か所へ反映する
    fn apply_volume_change(&mut self, change: VolumeChange) {
        let is_default = (change.volume - 1.0).abs() < 0.005;
        if is_default {
            self.config.volumes.remove(&change.name);
            lock(&self.handle.volumes).remove(&change.name);
        } else {
            self.config.volumes.insert(change.name.clone(), change.volume);
            lock(&self.handle.volumes).insert(change.name.clone(), change.volume);
        }
        // 再生中(発話中)なら即時反映。音声スレッドが混み合っていたら待たない
        // (保存済み音量は次の発話開始時に適用される)
        if let Some(mut handler) = try_lock(&self.handle.audio_handler) {
            if let Some(queue) =
                handler.get_mut_queues().get_mut(&tsclientlib::ClientId(change.client_id))
            {
                queue.volume = change.volume;
            }
        }
        if change.save {
            self.config.save();
        }
    }

    /// 設定ウィンドウ(OSレベルの別ウィンドウ)。開いている間は毎フレーム描画する
    fn show_settings_window(&mut self, ctx: egui::Context) {
        if !self.settings_open {
            return;
        }
        let mut open = true;
        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("settings_window"),
            egui::ViewportBuilder::default()
                .with_title("設定 - TS3 Client")
                .with_inner_size([560.0, 330.0]),
            |ui, _class| {
                if ui.ctx().input(|i| i.viewport().close_requested()) {
                    open = false;
                }
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.add_space(8.0);
                    ui.heading("プロファイル設定");
                    self.profile_settings_ui(ui);
                    ui.add_space(8.0);
                    ui.separator();
                    ui.heading("音声設定");
                    self.voice_settings_ui(ui);
                    ui.add_space(8.0);
                    ui.separator();
                    ui.heading("一般設定");
                    self.general_settings_ui(ui);
                    ui.add_space(8.0);
                });
            },
        );
        self.settings_open = open;
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
                ui.label("プロファイル:");
                ui.add_enabled_ui(!connected, |ui| {
                    let selected_name = self.config.selected().name.clone();
                    egui::ComboBox::from_id_salt("profile_select")
                        .selected_text(selected_name)
                        .width(180.0)
                        .show_ui(ui, |ui| {
                            for i in 0..self.config.profiles.len() {
                                let is_selected = i == self.config.selected_profile;
                                if ui
                                    .selectable_label(is_selected, &self.config.profiles[i].name)
                                    .clicked()
                                    && !is_selected
                                {
                                    self.config.selected_profile = i;
                                    self.config.save();
                                }
                            }
                        });
                });
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if connected {
                    if ui.button("切断").clicked() {
                        let _ = self.handle.commands.send(Command::Disconnect);
                    }
                } else {
                    let profile = self.config.selected();
                    let ready = !profile.address.trim().is_empty()
                        && !profile.nickname.trim().is_empty();
                    let button = ui.add_enabled(ready, egui::Button::new("接続"));
                    let button = if ready {
                        button
                    } else {
                        button.on_disabled_hover_text(
                            "設定でプロファイルのアドレスとニックネームを入力してください",
                        )
                    };
                    if button.clicked() {
                        self.config.save();
                        let _ = self.handle.commands.send(Command::Connect {
                            address: self.config.selected().address.trim().to_owned(),
                            nickname: self.config.selected().nickname.trim().to_owned(),
                        });
                    }
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
                // 設定ボタンは行の右端に寄せる
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("⚙ 設定").clicked() {
                        self.settings_open = true;
                    }
                });
            });
            ui.add_space(4.0);
        });

        self.show_settings_window(ui.ctx().clone());

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

        // コンテキストメニュー内では&mut selfを取れないため、音量変更は集めてから適用する
        let mut volume_changes: Vec<VolumeChange> = Vec::new();
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
                            let volume = self
                                .config
                                .volumes
                                .get(&client.name)
                                .copied()
                                .unwrap_or(1.0);
                            let label = if (volume - 1.0).abs() < 0.005 {
                                format!("👤 {}", client.name)
                            } else {
                                format!("👤 {} 🔊{:.0}%", client.name, volume * 100.0)
                            };
                            let response =
                                ui.label(label).on_hover_text("右クリックで音量調整");
                            response.context_menu(|ui| {
                                ui.set_min_width(220.0);
                                ui.label(format!("{} の音量", client.name));
                                let mut percent = volume * 100.0;
                                let slider = ui.add(
                                    egui::Slider::new(&mut percent, 0.0..=200.0).suffix("%"),
                                );
                                if slider.changed() || slider.drag_stopped() {
                                    volume_changes.push(VolumeChange {
                                        name: client.name.clone(),
                                        client_id: client.id,
                                        volume: percent / 100.0,
                                        // ドラッグ中は保存せず、離したときにファイルへ書く
                                        save: slider.drag_stopped(),
                                    });
                                }
                                if ui.button("100%に戻す").clicked() {
                                    volume_changes.push(VolumeChange {
                                        name: client.name.clone(),
                                        client_id: client.id,
                                        volume: 1.0,
                                        save: true,
                                    });
                                    ui.close();
                                }
                            });
                        });
                    }
                }
            });
        });
        for change in volume_changes {
            self.apply_volume_change(change);
        }
    }

    fn on_exit(&mut self) {
        self.config.save();
        // ウィンドウを閉じたときも正規の切断処理を試みる
        let _ = self.handle.commands.send(Command::Disconnect);
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
}
