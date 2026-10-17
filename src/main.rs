// Sem janela de console no Windows.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod app;
mod audio;
mod config;
mod hotkey;
mod lang;
mod platform;
mod recorder;
mod state;

use eframe::egui;
use state::{RecCmd, Shared};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
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

pub fn tray_tooltip(hotkey: &str) -> String {
    lang::fill(lang::t().tray_tip, &[&hotkey])
}

/// Espera um clipe que esteja sendo salvo terminar (no máximo 15s),
/// pra não deixar um MP4 pela metade ao sair.
fn wait_for_save(shared: &Shared) {
    let start = Instant::now();
    while shared.saving.load(Ordering::SeqCst) && start.elapsed() < Duration::from_secs(15) {
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Fecha o RePic de vez (sem deixar clipe pela metade nem ffmpeg aberto).
pub fn quit(shared: &Shared) -> ! {
    wait_for_save(shared);
    shared.send(RecCmd::Quit);
    // Dá tempo do ffmpeg fechar e limpar o buffer.
    std::thread::sleep(Duration::from_millis(400));
    std::process::exit(0);
}

fn main() -> eframe::Result {
    let has_arg = |name: &str| std::env::args().any(|a| a == name);
    // Instalador atualizando/desinstalando: pede pro RePic aberto fechar
    // (ele pode ser admin, aí o taskkill do instalador não consegue).
    if has_arg("--quit") {
        if platform::signal_other(platform::Signal::Quit) {
            let deadline = Instant::now() + Duration::from_secs(20);
            while platform::already_running() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(200));
            }
        }
        return Ok(());
    }
    // Reaberto como administrador: o RePic de antes ainda está fechando.
    let restarting = has_arg("--restart");
    let deadline = Instant::now() + Duration::from_secs(20);
    while platform::already_running() {
        if !restarting || Instant::now() > deadline {
            // Abriu o .exe de novo: só traz a janela que já está rodando.
            platform::focus_other_instance();
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    // Aberto pelo "Iniciar com o Windows": começa escondido na bandeja.
    let start_hidden = has_arg("--tray");
    let cfg = config::Config::load();

    if cfg.admin && !platform::is_admin() {
        let args = if start_hidden { "--restart --tray" } else { "--restart" };
        if platform::relaunch_as_admin(args) {
            return Ok(());
        }
        platform::log("rodar como administrador: o Windows não confirmou");
    }
    // Se o .exe mudou de pasta, atualiza o caminho do iniciar com o Windows.
    // (Admin recusado agora não desfaz a tarefa de admin.)
    if platform::autostart_enabled()
        && cfg.admin == platform::is_admin()
        && let Err(e) = platform::set_autostart(true, cfg.admin)
    {
        platform::log(&format!("iniciar com o Windows: {e}"));
    }

    let shared = Shared::new(cfg.clone());
    audio::start(shared.audio.clone(), audio::Source::Loopback);
    audio::start(shared.mic.clone(), audio::Source::Mic);
    recorder::start(shared.clone());

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("RePic")
            .with_inner_size([980.0, 660.0])
            .with_min_inner_size([560.0, 440.0])
            .with_visible(!start_hidden)
            // Escondido: nem pega o foco no instante em que o eframe mostra a
            // janela (o jogo continua na frente).
            .with_active(!start_hidden)
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
            app::apply_style(&cc.egui_ctx);

            // Guarda o HWND pra conseguir reabrir a janela pela bandeja
            // (e por uma segunda abertura do .exe).
            #[cfg(windows)]
            {
                use raw_window_handle::{HasWindowHandle, RawWindowHandle};
                if let Ok(h) = cc.window_handle()
                    && let RawWindowHandle::Win32(w) = h.as_raw()
                {
                    shared.hwnd.store(w.hwnd.get(), Ordering::SeqCst);
                    platform::remember_window(w.hwnd.get());
                    hotkey::set_window(w.hwnd.get());
                }
            }

            let s = shared.clone();
            platform::listen_signals(move |sig| match sig {
                platform::Signal::Show => s.request_show(),
                platform::Signal::Quit => quit(&s),
            });

            // Atalho global (funciona com o jogo em foco e com a janela fechada).
            let s = shared.clone();
            hotkey::start(cfg.shortcut, move |ev| match ev {
                hotkey::Event::Pressed => recorder::hotkey_pressed(s.clone()),
                hotkey::Event::Captured(c) => {
                    *s.captured.lock().unwrap() = Some(c);
                    s.repaint();
                }
                hotkey::Event::AdminGame => {
                    recorder::notify(&s, lang::t().toast_admin_game, platform::Toast::Error);
                    s.set_event(false, lang::t().admin_game);
                }
            });

            // Bandeja.
            let open_item = MenuItem::new(lang::t().tray_open, true, None);
            let save_item = MenuItem::new(lang::t().tray_save, true, None);
            let quit_item = MenuItem::new(lang::t().tray_quit, true, None);
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
                        .with_tooltip(tray_tooltip(&hotkey::label(&cfg.shortcut)))
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
                    quit(&s);
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

            Ok(Box::new(app::ClipApp::new(shared, tray, start_hidden)))
        }),
    );

    wait_for_save(&shared);
    shared.send(RecCmd::Quit);
    std::thread::sleep(Duration::from_millis(300));
    result
}
