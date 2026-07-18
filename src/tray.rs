//! システムトレイアイコン。
//!
//! Windowsではトレイアイコンを作ったスレッドでWin32メッセージポンプを
//! 回し続ける必要があるため、専用スレッドで作成・保持する。
//! メニュー操作はeguiのビューポートコマンドで直接ウィンドウへ反映する
//! (ウィンドウ非表示中はUIフレームが回らないため、チャネル経由のポーリングでは拾えない)。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui;
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

/// トレイアイコンと関連イベント転送スレッドを起動する。
/// `exiting`をtrueにしてからCloseを送ると、main.rs側の「閉じる=トレイへ格納」を迂回して本当に終了する。
pub fn spawn(ctx: egui::Context, exiting: Arc<AtomicBool>) {
    // メニューイベント(開く/終了)の転送
    {
        let ctx = ctx.clone();
        let exiting = exiting.clone();
        std::thread::spawn(move || {
            while let Ok(event) = MenuEvent::receiver().recv() {
                match event.id.as_ref() {
                    "open" => show_window(&ctx),
                    "exit" => {
                        exiting.store(true, Ordering::Relaxed);
                        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        ctx.request_repaint();
                    }
                    _ => {}
                }
            }
        });
    }

    // アイコン左クリックでもウィンドウを開く
    {
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            while let Ok(event) = TrayIconEvent::receiver().recv() {
                let clicked = matches!(
                    event,
                    TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } | TrayIconEvent::DoubleClick { button: MouseButton::Left, .. }
                );
                if clicked {
                    show_window(&ctx);
                }
            }
        });
    }

    // トレイアイコン本体+メッセージポンプ
    std::thread::spawn(move || {
        let menu = Menu::new();
        let t = crate::i18n::t();
        let _ = menu.append(&MenuItem::with_id("open", t.tray_open, true, None));
        let _ = menu.append(&MenuItem::with_id("exit", t.tray_exit, true, None));

        let _tray = match TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("TS3 Client")
            .with_icon(make_icon())
            .build()
        {
            Ok(tray) => tray,
            Err(e) => {
                tracing::error!("トレイアイコンの作成に失敗: {e}");
                return;
            }
        };

        // SAFETY: 標準的なWin32メッセージループ。このスレッドのメッセージのみ処理する
        unsafe {
            use windows_sys::Win32::UI::WindowsAndMessaging::{
                DispatchMessageW, GetMessageW, MSG, TranslateMessage,
            };
            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    });
}

fn show_window(ctx: &egui::Context) {
    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    ctx.request_repaint();
}

/// 32x32の単色円アイコンを生成する(画像ファイル不要)
fn make_icon() -> tray_icon::Icon {
    const SIZE: usize = 32;
    let mut rgba = vec![0u8; SIZE * SIZE * 4];
    let center = (SIZE as f32 - 1.0) / 2.0;
    let radius = SIZE as f32 / 2.0 - 1.0;
    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx = x as f32 - center;
            let dy = y as f32 - center;
            if (dx * dx + dy * dy).sqrt() <= radius {
                let i = (y * SIZE + x) * 4;
                rgba[i..i + 4].copy_from_slice(&[30, 120, 220, 255]);
            }
        }
    }
    tray_icon::Icon::from_rgba(rgba, SIZE as u32, SIZE as u32).expect("アイコン生成に失敗")
}
