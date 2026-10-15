// Sem janela de console no Windows.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod app;
mod audio;
mod config;
mod platform;
mod recorder;
mod state;

use eframe::egui;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use state::{RecCmd, Shared};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

/// Ícone gerado no código: bolinha vermelha de "REC" com anel branco.
fn icon_rgba(size: u32) -> Vec<u8> {
    let mut px = vec![0u8; (size * size * 4) as usize];
    let c = (size as f32 - 1.0) / 2.0;
    let r_out = size as f32 * 0.48;
    let r_ring = size as f32 * 0.40;
    let r_dot = size as f32 * 0.30;
    for y in 0..size {
        for x in 0..size {
            let d = ((x as f32 - c).powi(2) + (y as f32 - c).powi(2)).sqrt();
            let i = ((y * size + x) * 4) as usize;
            let (rgb, a) = if d <= r_dot {
                ([235, 64, 64], 1.0)
            } else if d <= r_ring {
                ([24, 24, 28], 1.0)
            } else if d <= r_out {
                ([245, 245, 245], 1.0)
            } else {
                ([0, 0, 0], 0.0)
            };
            // Borda suave.
            let edge = (r_out + 0.5 - d).clamp(0.0, 1.0);
            px[i..i + 3].copy_from_slice(&rgb);
            px[i + 3] = (a * edge * 255.0) as u8;
        }
    }
    px
}

fn main() -> eframe::Result {
    if platform::already_running() {
        return Ok(());
    }

    let cfg = config::Config::load();
    let shared = Shared::new(cfg.clone());
    audio::start(shared.audio.clone());
    recorder::start(shared.clone());

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("RePic")
            .with_inner_size([560.0, 640.0])
            .with_min_inner_size([420.0, 420.0])
            .with_icon(egui::IconData {
                rgba: icon_rgba(64),
                width: 64,
                height: 64,
            }),
        ..Default::default()
    };

    let shared_for_app = shared.clone();
    let result = eframe::run_native(
        "RePic",
        options,
        Box::new(move |cc| {
            let shared = shared_for_app;
            let _ = shared.ctx.set(cc.egui_ctx.clone());
            cc.egui_ctx.set_visuals(egui::Visuals::dark());

            // Guarda o HWND pra conseguir reabrir a janela pela bandeja.
            #[cfg(windows)]
            {
                use raw_window_handle::{HasWindowHandle, RawWindowHandle};
                if let Ok(h) = cc.window_handle() {
                    if let RawWindowHandle::Win32(w) = h.as_raw() {
                        shared
                            .hwnd
                            .store(w.hwnd.get(), std::sync::atomic::Ordering::SeqCst);
                    }
                }
            }

            // Atalho global (funciona com o jogo em foco).
            let hotkey = app::hotkey_from_name(&cfg.hotkey);
            let mut hotkey_error = None;
            let manager = match GlobalHotKeyManager::new() {
                Ok(m) => {
                    if let Err(e) = m.register(hotkey) {
                        hotkey_error = Some(format!(
                            "{} já está em uso por outro programa ({e})",
                            cfg.hotkey
                        ));
                    }
                    Some(m)
                }
                Err(e) => {
                    hotkey_error = Some(format!("Atalho global indisponível: {e}"));
                    None
                }
            };
            let s = shared.clone();
            GlobalHotKeyEvent::set_event_handler(Some(move |ev: GlobalHotKeyEvent| {
                if ev.state == HotKeyState::Pressed {
                    recorder::trigger_save(s.clone());
                }
            }));

            // Bandeja.
            let open_item = MenuItem::new("Abrir RePic", true, None);
            let save_item = MenuItem::new("Salvar clipe agora", true, None);
            let quit_item = MenuItem::new("Sair", true, None);
            let menu = Menu::new();
            let _ = menu.append_items(&[
                &open_item,
                &save_item,
                &PredefinedMenuItem::separator(),
                &quit_item,
            ]);
            let tray = tray_icon::Icon::from_rgba(icon_rgba(32), 32, 32)
                .ok()
                .and_then(|icon| {
                    TrayIconBuilder::new()
                        .with_icon(icon)
                        .with_tooltip(format!("RePic — aperte {} pra clipar", cfg.hotkey))
                        .with_menu(Box::new(menu))
                        .with_menu_on_left_click(false)
                        .build()
                        .ok()
                });

            let (open_id, save_id, quit_id) = (
                open_item.id().clone(),
                save_item.id().clone(),
                quit_item.id().clone(),
            );
            let s = shared.clone();
            MenuEvent::set_event_handler(Some(move |ev: MenuEvent| {
                if ev.id == open_id {
                    s.request_show();
                } else if ev.id == save_id {
                    recorder::trigger_save(s.clone());
                } else if ev.id == quit_id {
                    s.send(RecCmd::Quit);
                    // Dá tempo do ffmpeg fechar e limpar o buffer.
                    std::thread::sleep(std::time::Duration::from_millis(400));
                    std::process::exit(0);
                }
            }));
            let s = shared.clone();
            TrayIconEvent::set_event_handler(Some(move |ev: TrayIconEvent| match ev {
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }
                | TrayIconEvent::DoubleClick { .. } => s.request_show(),
                _ => {}
            }));

            Ok(Box::new(app::ClipApp::new(
                shared,
                manager,
                hotkey,
                hotkey_error,
                tray,
            )))
        }),
    );

    shared.send(RecCmd::Quit);
    std::thread::sleep(std::time::Duration::from_millis(300));
    result
}
