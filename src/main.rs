#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod api;
mod audio;
mod client;
mod config;
mod i18n;
mod tray;

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui;

use audio::VoiceMode;
use client::{ChannelInfo, ClientHandle, Command, Status, Update};
use config::Config;
use i18n::{fmt, t};

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

/// 起動オプションから言語指定を読む: `--lang ja|en` または省略形 `--ja` `--en`
fn parse_lang_arg() -> Option<i18n::Lang> {
    let args: Vec<String> = std::env::args().collect();
    for (i, arg) in args.iter().enumerate() {
        match arg.as_str() {
            "--ja" => return Some(i18n::Lang::Ja),
            "--en" => return Some(i18n::Lang::En),
            "--lang" => match args.get(i + 1).map(String::as_str) {
                Some("ja") => return Some(i18n::Lang::Ja),
                Some("en") => return Some(i18n::Lang::En),
                other => tracing::warn!("--lang: unknown value {other:?} (expected ja or en)"),
            },
            _ => {}
        }
    }
    None
}

/// メインウィンドウの固定サイズ。ボタン文言の幅が言語で違うため幅も言語別
fn main_window_size(lang: i18n::Lang) -> [f32; 2] {
    match lang {
        i18n::Lang::Ja => [400.0, 640.0],
        i18n::Lang::En => [460.0, 640.0],
    }
}

fn main() -> eframe::Result {
    init_logging();

    // 自動起動時はトレイに格納された状態(ウィンドウ非表示)で開始する
    let minimized = std::env::args().any(|a| a == "--minimized");
    // 言語の優先順位: 起動オプション > 設定ファイル > OSの言語設定
    let lang_override = parse_lang_arg();
    let lang = lang_override
        .or(Config::load().language)
        .unwrap_or_else(i18n::detect_os_lang);

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size(main_window_size(lang))
            .with_resizable(false)
            .with_maximize_button(false)
            .with_visible(!minimized),
        ..Default::default()
    };
    eframe::run_native(
        "TS3 Client",
        options,
        Box::new(move |cc| {
            setup_fonts(&cc.egui_ctx);
            Ok(Box::new(App::new(cc, minimized, lang_override)))
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

/// フォント設定。英数字はInter、日本語はNoto Sans JP(いずれもexeに埋め込み)。
/// システムフォントに依存しないため、どの環境でも同じ見た目になる
fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "inter".to_owned(),
        egui::FontData::from_static(include_bytes!("../assets/Inter.ttf")).into(),
    );
    fonts.font_data.insert(
        "noto_sans_jp".to_owned(),
        egui::FontData::from_static(include_bytes!("../assets/NotoSansJP.ttf")).into(),
    );
    // Interを最優先、日本語グリフはNoto Sans JPが受け持つ
    let proportional = fonts.families.entry(egui::FontFamily::Proportional).or_default();
    proportional.insert(0, "inter".to_owned());
    proportional.insert(1, "noto_sans_jp".to_owned());
    // 等幅系は既定フォントを優先しつつ、日本語だけNotoで補完する
    fonts
        .families
        .entry(egui::FontFamily::Monospace)
        .or_default()
        .push("noto_sans_jp".to_owned());
    ctx.set_fonts(fonts);

    // デバッグビルドでeguiが出す「ピクセル境界ずれ」の赤枠警告を無効化する
    // (パネルの出現アニメーション中に一瞬表示されて紛らわしいため)
    ctx.all_styles_mut(|style| style.debug.show_unaligned = false);
}

const MAX_LOG_LINES: usize = 200;

/// プッシュトゥトークに選べるキー (Windows仮想キーコード, 現在の言語での表示名)
fn ptt_keys() -> [(i32, &'static str); 7] {
    let t = t();
    [
        (0x05, t.key_mouse_side1),
        (0x06, t.key_mouse_side2),
        (0xA0, t.key_lshift),
        (0xA2, t.key_lctrl),
        (0xA4, t.key_lalt),
        (0x1D, t.key_muhenkan),
        (0x7B, t.key_f12),
    ]
}

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
    fn new(
        cc: &eframe::CreationContext<'_>,
        minimized: bool,
        lang_override: Option<i18n::Lang>,
    ) -> Self {
        let config = Config::load();
        // 言語を確定してから各スレッド(トレイ等)を起動する
        let lang = lang_override
            .or(config.language)
            .unwrap_or_else(i18n::detect_os_lang);
        i18n::set(lang);

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
            let current = selected.as_deref().unwrap_or(t().default_device);
            egui::ComboBox::from_id_salt(id).selected_text(current).width(240.0).show_ui(
                ui,
                |ui| {
                    if ui.selectable_label(selected.is_none(), t().default_device).clicked() {
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
            t().input_device,
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
            t().output_device,
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
            ui.label(t().voice_mode);
            for (mode, name) in [
                (VoiceMode::Always, t().mode_always),
                (VoiceMode::VoiceActivation, t().mode_vad),
                (VoiceMode::PushToTalk, t().mode_ptt),
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
                    ui.label(t().vad_threshold);
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
                    ui.label(t().input_level);
                    ui.add(
                        egui::ProgressBar::new(fraction)
                            .desired_width(200.0)
                            .text(format!("{level:.0} dB")),
                    );
                });
            }
            VoiceMode::PushToTalk => {
                ui.horizontal(|ui| {
                    ui.label(t().ptt_key);
                    let keys = ptt_keys();
                    let current = keys
                        .iter()
                        .find(|(vk, _)| *vk == self.config.ptt_vk)
                        .map(|(_, name)| *name)
                        .unwrap_or("?");
                    egui::ComboBox::from_id_salt("ptt_key").selected_text(current).show_ui(
                        ui,
                        |ui| {
                            for (vk, name) in &keys {
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
                    ui.weak(t().ptt_note);
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
            ui.strong(t().col_profile_name);
            ui.strong(t().col_address);
            ui.strong(t().col_nickname);
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
                    .on_hover_text(t().delete_profile)
                    .clicked()
                {
                    delete = Some(i);
                }
                ui.end_row();
            }
        });

        if ui.button("＋").on_hover_text(t().add_profile).clicked() {
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

        ui.weak(t().api_hint);

        if save {
            self.config.save();
        }
    }

    fn general_settings_ui(&mut self, ui: &mut egui::Ui) {
        // 言語選択。ユーザーが明示的に選んだ時点でconfigに固定される
        ui.horizontal(|ui| {
            ui.label(t().language_label);
            let current = match i18n::lang() {
                i18n::Lang::Ja => "日本語",
                i18n::Lang::En => "English",
            };
            egui::ComboBox::from_id_salt("language_select").selected_text(current).show_ui(
                ui,
                |ui| {
                    for (lang, name) in
                        [(i18n::Lang::Ja, "日本語"), (i18n::Lang::En, "English")]
                    {
                        if ui.selectable_label(i18n::lang() == lang, name).clicked()
                            && i18n::lang() != lang
                        {
                            i18n::set(lang);
                            self.config.language = Some(lang);
                            self.config.save();
                            // ボタン文言の幅が変わるためメインウィンドウの幅も追従させる
                            ui.ctx().send_viewport_cmd_to(
                                egui::ViewportId::ROOT,
                                egui::ViewportCommand::InnerSize(main_window_size(lang).into()),
                            );
                        }
                    }
                },
            );
        });

        let mut show_log = self.config.show_log;
        if ui.checkbox(&mut show_log, t().show_log).changed() {
            self.config.show_log = show_log;
            self.config.save();
        }

        let mut auto_start = self.config.auto_start;
        if ui.checkbox(&mut auto_start, t().auto_start).changed() {
            match build_auto_launch() {
                Ok(auto) => {
                    let result = if auto_start { auto.enable() } else { auto.disable() };
                    match result {
                        Ok(()) => {
                            self.config.auto_start = auto_start;
                            self.config.save();
                        }
                        Err(e) => {
                            self.push_log(fmt(t().auto_start_failed, &[&e.to_string()]))
                        }
                    }
                }
                Err(e) => self.push_log(fmt(t().auto_start_failed, &[&e.to_string()])),
            }
        }
        ui.horizontal(|ui| {
            ui.label(t().api_label);
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
                .with_title(t().settings_title)
                .with_inner_size([560.0, 440.0]),
            |ui, _class| {
                if ui.ctx().input(|i| i.viewport().close_requested()) {
                    open = false;
                }
                // プロファイルが増えても操作できるようスクロール可能にしておく
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.add_space(8.0);
                    ui.heading(t().profile_settings);
                    self.profile_settings_ui(ui);
                    ui.add_space(8.0);
                    ui.separator();
                    ui.heading(t().audio_settings);
                    self.voice_settings_ui(ui);
                    ui.add_space(8.0);
                    ui.separator();
                    ui.heading(t().general_settings);
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
            // 1行目: プロファイル選択 → 接続/切断、右端(ウィンドウ右上)に設定
            ui.horizontal(|ui| {
                ui.label(t().profile_label);
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
                if connected {
                    if ui.button(t().disconnect).clicked() {
                        let _ = self.handle.commands.send(Command::Disconnect);
                    }
                } else {
                    let profile = self.config.selected();
                    let ready = !profile.address.trim().is_empty()
                        && !profile.nickname.trim().is_empty();
                    let button = ui.add_enabled(ready, egui::Button::new(t().connect));
                    let button = if ready {
                        button
                    } else {
                        button.on_disabled_hover_text(t().connect_hint)
                    };
                    if button.clicked() {
                        self.config.save();
                        let _ = self.handle.commands.send(Command::Connect {
                            address: self.config.selected().address.trim().to_owned(),
                            nickname: self.config.selected().nickname.trim().to_owned(),
                        });
                    }
                }
                // 設定はオプション操作としてウィンドウ右上の端に置く
                // (ウィンドウ幅を内容に合わせているため接続ボタンとの間隔は小さい)
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(t().settings_button).clicked() {
                        self.settings_open = true;
                    }
                });
            });
            ui.add_space(4.0);
            // 2行目: ミュートと接続状態
            ui.horizontal(|ui| {
                let muted_flag = &self.handle.audio_controls.muted;
                let mut muted = muted_flag.load(Ordering::Relaxed);
                if ui.checkbox(&mut muted, t().mute).changed() {
                    muted_flag.store(muted, Ordering::Relaxed);
                }
                match &self.status {
                    Status::Disconnected => ui.label(t().status_disconnected),
                    Status::Connecting => ui.label(t().status_connecting),
                    Status::Connected { server_name } => ui.colored_label(
                        egui::Color32::from_rgb(0, 160, 60),
                        fmt(t().status_connected, &[server_name]),
                    ),
                    Status::Error(e) => ui.colored_label(
                        egui::Color32::from_rgb(200, 60, 60),
                        fmt(t().status_error, &[e]),
                    ),
                };
            });
            ui.add_space(4.0);
        });

        self.show_settings_window(ui.ctx().clone());

        if self.config.show_log {
            egui::Panel::bottom("log_panel")
                .resizable(true)
                .default_size(120.0)
                .show(ui, |ui| {
                    ui.add_space(4.0);
                    ui.label(t().log_heading);
                    egui::ScrollArea::vertical().stick_to_bottom(true).show(ui, |ui| {
                        for line in &self.log {
                            ui.small(line);
                        }
                    });
                });
        }

        // コンテキストメニュー内では&mut selfを取れないため、音量変更は集めてから適用する
        let mut volume_changes: Vec<VolumeChange> = Vec::new();
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                if self.channels.is_empty() {
                    ui.weak(t().no_channels);
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
                            let response = ui.label(label).on_hover_text(t().volume_hover);
                            response.context_menu(|ui| {
                                ui.set_min_width(220.0);
                                ui.label(fmt(t().volume_of, &[&client.name]));
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
                                if ui.button(t().reset_100).clicked() {
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
