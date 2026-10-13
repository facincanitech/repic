use crate::config::{FPS_OPTIONS, HOTKEY_OPTIONS, SECONDS_OPTIONS};
use crate::platform;
use crate::recorder;
use crate::state::{Health, RecCmd, Shared};
use eframe::egui::{self, Color32, RichText, Vec2};
use global_hotkey::GlobalHotKeyManager;
use global_hotkey::hotkey::{Code, HotKey};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Instant, SystemTime};

const ACCENT: Color32 = Color32::from_rgb(235, 64, 64);
const THUMB: Vec2 = Vec2::new(128.0, 72.0);

pub fn hotkey_from_name(name: &str) -> HotKey {
    let code = match name {
        "F6" => Code::F6,
        "F7" => Code::F7,
        "F9" => Code::F9,
        "F10" => Code::F10,
        "F11" => Code::F11,
        "F12" => Code::F12,
        _ => Code::F8,
    };
    HotKey::new(None, code)
}

struct Clip {
    path: PathBuf,
    name: String,
    when: String,
    size_mb: f64,
}

pub struct ClipApp {
    shared: Arc<Shared>,
    hotkeys: Option<GlobalHotKeyManager>,
    current_hotkey: HotKey,
    hotkey_error: Option<String>,
    _tray: Option<tray_icon::TrayIcon>,
    clips: Vec<Clip>,
    last_scan: Instant,
    thumbs: HashMap<PathBuf, Option<egui::TextureHandle>>,
    confirm_delete: Option<PathBuf>,
}

impl ClipApp {
    pub fn new(
        shared: Arc<Shared>,
        hotkeys: Option<GlobalHotKeyManager>,
        current_hotkey: HotKey,
        hotkey_error: Option<String>,
        tray: Option<tray_icon::TrayIcon>,
    ) -> Self {
        Self {
            shared,
            hotkeys,
            current_hotkey,
            hotkey_error,
            _tray: tray,
            clips: Vec::new(),
            last_scan: Instant::now() - std::time::Duration::from_secs(60),
            thumbs: HashMap::new(),
            confirm_delete: None,
        }
    }

    fn scan(&mut self) {
        let dir = self.shared.config().output_dir;
        let mut clips: Vec<(SystemTime, Clip)> = std::fs::read_dir(&dir)
            .map(|rd| {
                rd.flatten()
                    .filter(|e| {
                        e.path()
                            .extension()
                            .is_some_and(|x| x.eq_ignore_ascii_case("mp4"))
                    })
                    .filter_map(|e| {
                        let meta = e.metadata().ok()?;
                        let modified = meta.modified().ok()?;
                        let when = chrono::DateTime::<chrono::Local>::from(modified)
                            .format("%d/%m/%Y às %H:%M:%S")
                            .to_string();
                        Some((
                            modified,
                            Clip {
                                name: e.file_name().to_string_lossy().to_string(),
                                path: e.path(),
                                when,
                                size_mb: meta.len() as f64 / 1_048_576.0,
                            },
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        clips.sort_by(|a, b| b.0.cmp(&a.0));
        self.clips = clips.into_iter().map(|(_, c)| c).collect();
        self.thumbs.retain(|_, t| t.is_some());
        self.last_scan = Instant::now();
    }

    fn thumb(&mut self, ctx: &egui::Context, clip: &PathBuf) -> Option<egui::TextureHandle> {
        if let Some(t) = self.thumbs.get(clip) {
            return t.clone();
        }
        let path = recorder::thumb_path(&self.shared, clip);
        let tex = image::open(&path).ok().map(|img| {
            let rgba = img.to_rgba8();
            let size = [rgba.width() as usize, rgba.height() as usize];
            let color = egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
            ctx.load_texture(path.to_string_lossy(), color, egui::TextureOptions::LINEAR)
        });
        // Guarda até a falta (None) pra não ler o disco todo frame;
        // o próximo scan limpa as faltas e tenta de novo.
        self.thumbs.insert(clip.clone(), tex.clone());
        tex
    }

    fn change_hotkey(&mut self, name: &str) {
        let new = hotkey_from_name(name);
        if let Some(m) = &self.hotkeys {
            let _ = m.unregister(self.current_hotkey);
            match m.register(new) {
                Ok(()) => {
                    self.current_hotkey = new;
                    self.hotkey_error = None;
                }
                Err(e) => {
                    self.hotkey_error =
                        Some(format!("{name} já está em uso por outro programa ({e})"));
                    let _ = m.register(self.current_hotkey);
                    return;
                }
            }
        }
        let mut cfg = self.shared.config.lock().unwrap();
        cfg.hotkey = name.to_string();
        cfg.save();
    }

    fn header(&self, ui: &mut egui::Ui) {
        let status = self.shared.status.lock().unwrap().clone();
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(Vec2::splat(14.0), egui::Sense::hover());
            let color = match status.health {
                Health::Recording => {
                    // Bolinha "REC" pulsando.
                    let t = ui.input(|i| i.time);
                    let a = 0.55 + 0.45 * (t * 3.0).sin().abs() as f32;
                    ACCENT.gamma_multiply(a)
                }
                Health::Starting => Color32::from_rgb(230, 180, 60),
                Health::Error => Color32::GRAY,
            };
            ui.painter().circle_filled(rect.center(), 6.0, color);
            ui.label(RichText::new("ClipCraft").size(22.0).strong());
        });
        ui.label(RichText::new(&status.text).weak());
        if let Some((ok, msg)) = &status.last_event {
            let color = if *ok {
                Color32::from_rgb(110, 200, 120)
            } else {
                ACCENT
            };
            ui.label(RichText::new(msg).color(color));
        }
    }

    fn explanation(&self, ui: &mut egui::Ui) {
        let cfg = self.shared.config();
        egui::Frame::group(ui.style()).inner_margin(12.0).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                RichText::new(format!(
                    "Aperte {} a qualquer momento e os últimos {} segundos viram um clipe.",
                    cfg.hotkey, cfg.seconds
                ))
                .size(15.0)
                .strong(),
            );
            ui.add_space(4.0);
            ui.label("• O ClipCraft fica gravando em loop, só na memória/pasta temporária. Nada sai do seu PC.");
            ui.label("• Fechar esta janela não para a gravação: ele continua na bandeja, perto do relógio.");
            ui.label("• Pra sair de verdade: clique com o botão direito no ícone da bandeja → Sair.");
            ui.label("• Jogos em tela cheia exclusiva podem sair pretos. Use modo janela sem bordas.");
        });
    }

    fn settings(&mut self, ui: &mut egui::Ui) {
        let mut cfg = self.shared.config();
        let before = cfg.clone();
        let mut hotkey = cfg.hotkey.clone();

        ui.horizontal_wrapped(|ui| {
            egui::ComboBox::from_label("duração")
                .selected_text(format!("{}s", cfg.seconds))
                .show_ui(ui, |ui| {
                    for s in SECONDS_OPTIONS {
                        ui.selectable_value(&mut cfg.seconds, s, format!("{s}s"));
                    }
                });
            ui.add_space(8.0);
            egui::ComboBox::from_label("fps")
                .selected_text(cfg.fps.to_string())
                .show_ui(ui, |ui| {
                    for f in FPS_OPTIONS {
                        ui.selectable_value(&mut cfg.fps, f, f.to_string());
                    }
                });
            ui.add_space(8.0);
            egui::ComboBox::from_label("tecla")
                .selected_text(&hotkey)
                .show_ui(ui, |ui| {
                    for k in HOTKEY_OPTIONS {
                        ui.selectable_value(&mut hotkey, k.to_string(), k);
                    }
                });
            ui.add_space(8.0);
            ui.checkbox(&mut cfg.audio, "som do PC");
        });
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("Pasta: {}", cfg.output_dir.display())).weak());
            if ui.small_button("Abrir pasta").clicked() {
                let _ = std::fs::create_dir_all(&cfg.output_dir);
                platform::open_path(&cfg.output_dir);
            }
        });
        if let Some(err) = &self.hotkey_error {
            ui.label(RichText::new(err).color(ACCENT));
        }

        if hotkey != cfg.hotkey {
            self.change_hotkey(&hotkey);
        }
        if cfg != before {
            let needs_restart = cfg.fps != before.fps || cfg.seconds != before.seconds;
            self.shared
                .audio
                .enabled
                .store(cfg.audio, Ordering::Relaxed);
            self.shared.audio.set_keep_secs(cfg.seconds);
            cfg.hotkey = self.shared.config().hotkey; // já salvo em change_hotkey
            cfg.save();
            *self.shared.config.lock().unwrap() = cfg;
            if needs_restart {
                self.shared.send(RecCmd::Restart);
            }
        }
    }

    fn history(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Histórico").size(17.0).strong());
            ui.label(RichText::new(format!("({})", self.clips.len())).weak());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let saving = self.shared.saving.load(Ordering::SeqCst);
                let btn = egui::Button::new(RichText::new("● Salvar agora").color(Color32::WHITE))
                    .fill(ACCENT);
                if ui.add_enabled(!saving, btn).clicked() {
                    recorder::trigger_save(self.shared.clone());
                }
            });
        });
        ui.add_space(4.0);

        if self.clips.is_empty() {
            ui.add_space(30.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("Nenhum clipe ainda.").weak());
                let key = self.shared.config().hotkey;
                ui.label(RichText::new(format!("Joga um pouco e aperta {key} kkk")).weak());
            });
            return;
        }

        let ctx = ui.ctx().clone();
        let paths: Vec<PathBuf> = self.clips.iter().map(|c| c.path.clone()).collect();
        let thumbs: Vec<Option<egui::TextureHandle>> =
            paths.iter().map(|p| self.thumb(&ctx, p)).collect();
        let mut delete: Option<PathBuf> = None;

        egui::ScrollArea::vertical()
            .auto_shrink([false; 2])
            .show(ui, |ui| {
                for (clip, tex) in self.clips.iter().zip(thumbs) {
                    egui::Frame::group(ui.style())
                        .inner_margin(8.0)
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                match tex {
                                    Some(t) => {
                                        let r = ui.add(
                                            egui::Image::new((t.id(), THUMB))
                                                .corner_radius(4.0)
                                                .sense(egui::Sense::click()),
                                        );
                                        if r.on_hover_text("Assistir").clicked() {
                                            platform::open_path(&clip.path);
                                        }
                                    }
                                    None => {
                                        let (rect, _) =
                                            ui.allocate_exact_size(THUMB, egui::Sense::hover());
                                        ui.painter().rect_filled(rect, 4.0, Color32::from_gray(40));
                                        ui.painter().text(
                                            rect.center(),
                                            egui::Align2::CENTER_CENTER,
                                            "▶",
                                            egui::FontId::proportional(22.0),
                                            Color32::GRAY,
                                        );
                                    }
                                }
                                ui.vertical(|ui| {
                                    ui.label(RichText::new(&clip.name).strong());
                                    ui.label(
                                        RichText::new(format!(
                                            "{} · {:.1} MB",
                                            clip.when, clip.size_mb
                                        ))
                                        .weak(),
                                    );
                                    ui.horizontal(|ui| {
                                        if ui.button("▶ Assistir").clicked() {
                                            platform::open_path(&clip.path);
                                        }
                                        if ui.button("📁 Mostrar").clicked() {
                                            platform::reveal_path(&clip.path);
                                        }
                                        if self.confirm_delete.as_ref() == Some(&clip.path) {
                                            if ui
                                                .button(RichText::new("Confirmar").color(ACCENT))
                                                .clicked()
                                            {
                                                delete = Some(clip.path.clone());
                                            }
                                            if ui.button("Cancelar").clicked() {
                                                self.confirm_delete = None;
                                            }
                                        } else if ui.button("🗑 Apagar").clicked() {
                                            self.confirm_delete = Some(clip.path.clone());
                                        }
                                    });
                                });
                            });
                        });
                    ui.add_space(4.0);
                }
            });

        if let Some(path) = delete {
            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_file(recorder::thumb_path(&self.shared, &path));
            self.thumbs.remove(&path);
            self.confirm_delete = None;
            self.shared.history_dirty.store(true, Ordering::SeqCst);
        }
    }
}

impl eframe::App for ClipApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Pedido da bandeja pra mostrar a janela.
        if self.shared.show_requested.swap(false, Ordering::SeqCst) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            self.shared.history_dirty.store(true, Ordering::SeqCst);
        }
        // Fechar = esconder na bandeja.
        if ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if self.shared.history_dirty.swap(false, Ordering::SeqCst)
            || self.last_scan.elapsed().as_secs() >= 3
        {
            self.scan();
        }

        egui::CentralPanel::default().show(ui, |ui| {
            self.header(ui);
            ui.add_space(8.0);
            self.explanation(ui);
            ui.add_space(8.0);
            self.settings(ui);
            ui.separator();
            self.history(ui);
        });

        // Atualiza a bolinha e o status mesmo sem mexer o mouse.
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(250));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.shared.send(RecCmd::Quit);
    }
}
