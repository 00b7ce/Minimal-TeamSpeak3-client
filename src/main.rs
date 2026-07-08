#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod client;

use std::collections::VecDeque;

use eframe::egui;

use client::{ChannelInfo, ClientHandle, Command, Status, Update};

fn main() -> eframe::Result {
    tracing_subscriber::fmt::init();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([420.0, 560.0]),
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

struct App {
    handle: ClientHandle,
    address: String,
    nickname: String,
    status: Status,
    channels: Vec<ChannelInfo>,
    log: VecDeque<String>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let app = Self {
            handle: client::spawn(cc.egui_ctx.clone()),
            address: "192.168.10.8".to_owned(),
            nickname: "mekabu".to_owned(),
            status: Status::Disconnected,
            channels: Vec::new(),
            log: VecDeque::new(),
        };
        // 動作確認用: 起動と同時に接続する
        if std::env::args().any(|a| a == "--autoconnect") {
            let _ = app.handle.commands.send(Command::Connect {
                address: app.address.clone(),
                nickname: app.nickname.clone(),
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
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.apply_updates();

        egui::Panel::top("connection_panel").show(ui, |ui| {
            ui.add_space(4.0);
            let connected = !matches!(self.status, Status::Disconnected | Status::Error(_));
            ui.horizontal(|ui| {
                ui.label("サーバ:");
                ui.add_enabled(!connected, egui::TextEdit::singleline(&mut self.address).desired_width(140.0));
                ui.label("名前:");
                ui.add_enabled(!connected, egui::TextEdit::singleline(&mut self.nickname).desired_width(80.0));
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if connected {
                    if ui.button("切断").clicked() {
                        let _ = self.handle.commands.send(Command::Disconnect);
                    }
                } else if ui.button("接続").clicked() {
                    let _ = self.handle.commands.send(Command::Connect {
                        address: self.address.trim().to_owned(),
                        nickname: self.nickname.trim().to_owned(),
                    });
                }
                match &self.status {
                    Status::Disconnected => ui.label("未接続"),
                    Status::Connecting => ui.label("接続中..."),
                    Status::Connected { server_name } => {
                        ui.colored_label(egui::Color32::from_rgb(0, 160, 60), format!("接続済: {server_name}"))
                    }
                    Status::Error(e) => ui.colored_label(egui::Color32::from_rgb(200, 60, 60), format!("エラー: {e}")),
                };
            });
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
        // ウィンドウを閉じたときも正規の切断処理を試みる
        let _ = self.handle.commands.send(Command::Disconnect);
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
}
