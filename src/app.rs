//! Janela: barra de cima (status + clipar), grade com os últimos clipes,
//! e as configurações num modal.

use crate::config::{FPS_OPTIONS, SECONDS_OPTIONS};
use crate::hotkey;
use crate::lang::{fill, t};
use crate::platform::{self, Monitor};
use crate::recorder;
use crate::state::{Health, RecCmd, Shared};
use eframe::egui::{self, Color32, CornerRadius, RichText, Stroke, Vec2};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant, SystemTime};

const ACCENT: Color32 = Color32::from_rgb(235, 64, 64);
const OK_GREEN: Color32 = Color32::from_rgb(110, 200, 120);
const BG: Color32 = Color32::from_rgb(17, 17, 21);
const CARD: Color32 = Color32::from_rgb(27, 27, 33);
const CARD_HOVER: Color32 = Color32::from_rgb(36, 36, 44);
/// Na janela padrão cabem 3 por linha; maior/menor, ajusta sozinho.
const CARD_MIN_W: f32 = 280.0;
const GAP: f32 = 12.0;
/// Quanto tempo a mensagem ("Clipe salvo", "Exportado"…) fica na tela.
const EVENT_SECS: u64 = 6;

pub fn apply_style(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = Color32::from_rgb(24, 24, 30);
    v.extreme_bg_color = Color32::from_rgb(12, 12, 15);
    v.window_corner_radius = CornerRadius::same(12);
    v.selection.bg_fill = ACCENT.gamma_multiply(0.7);
    for w in [
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        // Mais que isso deixa a caixinha de marcar redonda.
        w.corner_radius = CornerRadius::same(4);
    }
    ctx.set_visuals(v);
    ctx.all_styles_mut(|s| {
        s.spacing.button_padding = Vec2::new(10.0, 5.0);
        s.spacing.item_spacing = Vec2::new(8.0, 6.0);
    });
}

struct Clip {
    path: PathBuf,
    name: String,
    when: String,
    size_mb: f64,
}

pub struct ClipApp {
    shared: Arc<Shared>,
    tray: Option<tray_icon::TrayIcon>,
    clips: Vec<Clip>,
    last_scan: Instant,
    thumbs: HashMap<PathBuf, Option<egui::TextureHandle>>,
    confirm_delete: Option<PathBuf>,
    settings_open: bool,
    /// Esperando a pessoa apertar o novo atalho.
    capturing: bool,
    monitors: Vec<Monitor>,
    autostart: bool,
}

impl ClipApp {
    pub fn new(shared: Arc<Shared>, tray: Option<tray_icon::TrayIcon>) -> Self {
        Self {
            shared,
            tray,
            clips: Vec::new(),
            last_scan: Instant::now() - Duration::from_secs(60),
            thumbs: HashMap::new(),
            confirm_delete: None,
            settings_open: false,
            capturing: false,
            monitors: Vec::new(),
            autostart: platform::autostart_enabled(),
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
                            .format(t().date_fmt)
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
        // Esquece miniaturas que faltaram (tenta de novo) e de clipes que sumiram.
        let alive: std::collections::HashSet<&PathBuf> = self.clips.iter().map(|c| &c.path).collect();
        self.thumbs.retain(|p, t| t.is_some() && alive.contains(p));
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

    // --- Atalho ----------------------------------------------------------
    // Quem escuta a tecla nova é o gancho de teclado (hotkey.rs), não a
    // janela: assim pega a tecla física certa em qualquer layout.

    fn start_capture(&mut self) {
        *self.shared.captured.lock().unwrap() = None;
        hotkey::begin_capture();
        self.capturing = true;
    }

    fn cancel_capture(&mut self) {
        hotkey::cancel_capture();
        self.capturing = false;
    }

    /// Recebe o atalho escolhido (ou o Esc) do gancho.
    fn handle_capture(&mut self, ctx: &egui::Context) {
        let Some(result) = self.shared.captured.lock().unwrap().take() else {
            return;
        };
        self.capturing = false;
        // O Esc de cancelar não fecha as configurações junto.
        ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        let Some(combo) = result else {
            return;
        };
        let label = hotkey::label(&combo);
        if let Some(t) = &self.tray {
            let _ = t.set_tooltip(Some(crate::tray_tooltip(&label)));
        }
        let mut cfg = self.shared.config.lock().unwrap();
        cfg.shortcut = combo;
        cfg.save();
    }

    // --- Barra de cima ---------------------------------------------------

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        let status = self.shared.status.lock().unwrap().clone();
        let cfg = self.shared.config();
        let saving = self.shared.saving.load(Ordering::SeqCst);

        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(Vec2::splat(16.0), egui::Sense::hover());
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
            ui.painter().circle_filled(rect.center(), 7.0, color);
            ui.label(RichText::new("RePic").size(24.0).strong());

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(egui::Button::new(RichText::new("⚙").size(18.0)).min_size(Vec2::splat(34.0)))
                    .on_hover_text(t().settings)
                    .clicked()
                {
                    self.open_settings();
                }
                let label = if saving {
                    t().saving_btn.to_string()
                } else {
                    format!("{}  ·  {}", t().clip_now, hotkey::label(&cfg.shortcut))
                };
                let btn = egui::Button::new(RichText::new(label).color(Color32::WHITE).strong())
                    .fill(ACCENT)
                    .min_size(Vec2::new(0.0, 34.0));
                if ui.add_enabled(!saving, btn).clicked() {
                    recorder::trigger_save(self.shared.clone());
                }
            });
        });

        let color = if status.health == Health::Error {
            ACCENT
        } else {
            ui.visuals().weak_text_color()
        };
        ui.label(RichText::new(&status.text).color(color));

        if let Some((ok, msg, at)) = &status.last_event
            && (saving || at.elapsed() < Duration::from_secs(EVENT_SECS))
        {
            ui.label(RichText::new(msg).color(if *ok { OK_GREEN } else { ACCENT }));
        }
    }

    // --- Grade de clipes -------------------------------------------------

    fn clips_grid(&mut self, ui: &mut egui::Ui) {
        let cfg = self.shared.config();
        ui.horizontal(|ui| {
            ui.label(RichText::new(t().recent).size(18.0).strong());
            ui.label(RichText::new(format!("{}", self.clips.len())).weak());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(t().open_folder).clicked() {
                    let _ = std::fs::create_dir_all(&cfg.output_dir);
                    platform::open_path(&cfg.output_dir);
                }
            });
        });
        ui.add_space(6.0);

        if self.clips.is_empty() {
            ui.add_space(60.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("🎬").size(48.0));
                ui.add_space(8.0);
                let key = hotkey::label(&cfg.shortcut);
                let title = fill(t().empty_title, &[&key, &cfg.seconds]);
                ui.label(RichText::new(title).size(17.0).strong());
                ui.add_space(4.0);
                ui.label(RichText::new(t().empty_1).weak());
                ui.label(RichText::new(t().empty_2).weak());
            });
            return;
        }

        let ctx = ui.ctx().clone();
        let paths: Vec<PathBuf> = self.clips.iter().map(|c| c.path.clone()).collect();
        let thumbs: Vec<Option<egui::TextureHandle>> =
            paths.iter().map(|p| self.thumb(&ctx, p)).collect();
        let mut clicked: Option<(Action, usize)> = None;

        egui::ScrollArea::vertical()
            .auto_shrink([false; 2])
            .show(ui, |ui| {
                let avail = ui.available_width() - 4.0;
                let cols = (((avail + GAP) / (CARD_MIN_W + GAP)).floor() as usize).max(1);
                let card_w = (avail - GAP * (cols - 1) as f32) / cols as f32;
                for (r, (row, row_thumbs)) in
                    self.clips.chunks(cols).zip(thumbs.chunks(cols)).enumerate()
                {
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = GAP;
                        for (c, (clip, tex)) in row.iter().zip(row_thumbs).enumerate() {
                            let down = egui::Layout::top_down(egui::Align::Min);
                            ui.allocate_ui_with_layout(Vec2::new(card_w, 0.0), down, |ui| {
                                ui.set_width(card_w);
                                if let Some(action) = clip_card(ui, clip, tex.as_ref(), card_w) {
                                    clicked = Some((action, r * cols + c));
                                }
                            });
                        }
                    });
                    ui.add_space(GAP);
                }
            });

        if let Some((action, i)) = clicked {
            let path = self.clips[i].path.clone();
            let name = self.clips[i].name.clone();
            self.run_action(action, path, &name);
        }
    }

    fn run_action(&mut self, action: Action, path: PathBuf, name: &str) {
        match action {
            Action::Play => platform::open_path(&path),
            Action::Reveal => platform::reveal_path(&path),
            Action::Export => {
                if let Some(dest) = platform::save_dialog(name) {
                    match std::fs::copy(&path, &dest) {
                        Ok(_) => self
                            .shared
                            .set_event(true, fill(t().exported, &[&dest.display()])),
                        Err(e) => self.shared.set_event(false, fill(t().export_failed, &[&e])),
                    }
                }
            }
            Action::Delete => self.confirm_delete = Some(path),
        }
    }

    fn delete_modal(&mut self, ctx: &egui::Context) {
        let Some(path) = self.confirm_delete.clone() else {
            return;
        };
        let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
        let mut close = false;
        let resp = egui::Modal::new(egui::Id::new("confirm_delete")).show(ctx, |ui| {
            ui.set_width(340.0);
            ui.label(RichText::new(t().delete_title).size(18.0).strong());
            ui.add_space(4.0);
            ui.label(fill(t().delete_body, &[&name]));
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let del = egui::Button::new(RichText::new(t().delete).color(Color32::WHITE)).fill(ACCENT);
                if ui.add(del).clicked() {
                    match std::fs::remove_file(&path) {
                        Ok(()) => {
                            let _ = std::fs::remove_file(recorder::thumb_path(&self.shared, &path));
                            self.thumbs.remove(&path);
                            self.shared.history_dirty.store(true, Ordering::SeqCst);
                        }
                        Err(e) => self.shared.set_event(false, fill(t().delete_failed, &[&e])),
                    }
                    close = true;
                }
                if ui.button(t().cancel).clicked() {
                    close = true;
                }
            });
        });
        if close || resp.should_close() {
            self.confirm_delete = None;
        }
    }

    // --- Configurações ---------------------------------------------------

    fn open_settings(&mut self) {
        self.monitors = platform::list_monitors();
        self.autostart = platform::autostart_enabled();
        self.settings_open = true;
    }

    fn settings_modal(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let mut cfg = self.shared.config();
        let before = cfg.clone();
        let mut autostart = self.autostart;
        let mut change_key = false;
        let mut pick_dir = false;
        let mut close = false;

        let resp = egui::Modal::new(egui::Id::new("settings")).show(ctx, |ui| {
            ui.set_width(460.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(t().settings).size(20.0).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("✖").on_hover_text(t().close).clicked() {
                        close = true;
                    }
                });
            });
            ui.add_space(6.0);

            section(ui, t().sec_shortcut, |ui| {
                ui.horizontal(|ui| {
                    ui.label(t().key_to_clip);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if self.capturing {
                            if ui.button(t().cancel).clicked() {
                                change_key = true;
                            }
                            ui.label(RichText::new(t().press_key).color(ACCENT));
                        } else {
                            if ui.button(t().change).clicked() {
                                change_key = true;
                            }
                            key_chip(ui, &hotkey::label(&cfg.shortcut));
                        }
                    });
                });
                hint(ui, t().shortcut_hint);
            });

            section(ui, t().sec_recording, |ui| {
                egui::Grid::new("rec_grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                    ui.label(t().duration);
                    egui::ComboBox::from_id_salt("dur")
                        .selected_text(fill(t().last_secs, &[&cfg.seconds]))
                        .show_ui(ui, |ui| {
                            for s in SECONDS_OPTIONS {
                                ui.selectable_value(&mut cfg.seconds, s, fill(t().last_secs, &[&s]));
                            }
                        });
                    ui.end_row();

                    ui.label(t().quality);
                    egui::ComboBox::from_id_salt("fps")
                        .selected_text(format!("{} fps", cfg.fps))
                        .show_ui(ui, |ui| {
                            for f in FPS_OPTIONS {
                                ui.selectable_value(&mut cfg.fps, f, format!("{f} fps"));
                            }
                        });
                    ui.end_row();

                    ui.label(t().monitor);
                    let current = if cfg.monitor.is_empty() {
                        t().primary_windows.to_string()
                    } else {
                        self.monitors
                            .iter()
                            .find(|m| m.id == cfg.monitor)
                            .map(|m| m.label.clone())
                            .unwrap_or_else(|| t().disconnected.into())
                    };
                    egui::ComboBox::from_id_salt("mon")
                        .selected_text(current)
                        .width(220.0)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut cfg.monitor, String::new(), t().primary_windows);
                            for m in &self.monitors {
                                ui.selectable_value(&mut cfg.monitor, m.id.clone(), &m.label);
                            }
                        });
                    ui.end_row();
                });
                ui.add_space(4.0);
                ui.checkbox(&mut cfg.anywhere, t().anywhere);
                hint(ui, if cfg.anywhere { t().anywhere_on } else { t().anywhere_off });
            });

            section(ui, t().sec_audio, |ui| {
                ui.checkbox(&mut cfg.audio, t().pc_audio);
                ui.checkbox(&mut cfg.mic, t().mic);
                hint(ui, t().mic_hint);
            });

            section(ui, t().sec_system, |ui| {
                ui.checkbox(&mut autostart, t().autostart);
                hint(ui, t().autostart_hint);
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(t().clips_folder);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(t().change_folder).clicked() {
                            pick_dir = true;
                        }
                    });
                });
                ui.label(RichText::new(cfg.output_dir.display().to_string()).weak().small());
            });

            // Rodapé: versão e marca.
            ui.add_space(10.0);
            ui.vertical_centered(|ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    let weak = ui.visuals().weak_text_color();
                    ui.label(
                        RichText::new(concat!("RePic v", env!("CARGO_PKG_VERSION"), " · powered by"))
                            .small()
                            .color(weak),
                    );
                    ui.hyperlink_to(
                        RichText::new("FacincaniTech").small().strong(),
                        "https://facincanitech.github.io",
                    );
                });
            });
        });
        if resp.should_close() && !self.capturing {
            close = true;
        }

        // Aplica o que mudou.
        if change_key {
            if self.capturing {
                self.cancel_capture();
            } else {
                self.start_capture();
            }
        }
        if pick_dir && let Some(dir) = platform::pick_folder(&cfg.output_dir) {
            cfg.output_dir = dir;
        }
        if autostart != self.autostart {
            match platform::set_autostart(autostart) {
                Ok(()) => self.autostart = autostart,
                Err(e) => self.shared.set_event(false, fill(t().autostart_failed, &[&e])),
            }
        }
        if cfg != before {
            let needs_restart =
                cfg.fps != before.fps || cfg.seconds != before.seconds || cfg.monitor != before.monitor;
            self.shared.audio.enabled.store(cfg.audio, Ordering::Relaxed);
            self.shared.mic.enabled.store(cfg.mic, Ordering::Relaxed);
            self.shared.audio.set_keep_secs(cfg.seconds);
            self.shared.mic.set_keep_secs(cfg.seconds);
            if cfg.output_dir != before.output_dir {
                self.shared.history_dirty.store(true, Ordering::SeqCst);
            }
            cfg.shortcut = self.shared.config().shortcut; // salvo à parte em handle_capture
            cfg.save();
            *self.shared.config.lock().unwrap() = cfg;
            if needs_restart {
                self.shared.send(RecCmd::Restart);
            }
        }
        if close {
            if self.capturing {
                self.cancel_capture();
            }
            self.settings_open = false;
        }
    }
}

enum Action {
    Play,
    Reveal,
    Export,
    Delete,
}

fn clip_card(ui: &mut egui::Ui, clip: &Clip, tex: Option<&egui::TextureHandle>, w: f32) -> Option<Action> {
    let mut action = None;
    let thumb_size = Vec2::new(w, w * 9.0 / 16.0);
    let hovered = ui.rect_contains_pointer(ui.max_rect());
    egui::Frame::new()
        .fill(if hovered { CARD_HOVER } else { CARD })
        .corner_radius(10)
        .show(ui, |ui| {
            ui.set_width(w);
            // Miniatura: clique assiste.
            let (rect, resp) = ui.allocate_exact_size(thumb_size, egui::Sense::click());
            let radius = CornerRadius { nw: 10, ne: 10, sw: 0, se: 0 };
            match tex {
                Some(t) => {
                    egui::Image::new((t.id(), thumb_size))
                        .corner_radius(radius)
                        .paint_at(ui, rect);
                }
                None => {
                    ui.painter().rect_filled(rect, radius, Color32::from_gray(38));
                }
            }
            let painter = ui.painter();
            if resp.hovered() || tex.is_none() {
                if resp.hovered() {
                    painter.rect_filled(rect, radius, Color32::from_black_alpha(90));
                }
                painter.circle(rect.center(), 22.0, Color32::from_black_alpha(150), Stroke::new(2.0, Color32::WHITE));
                painter.text(
                    rect.center() + Vec2::new(2.0, 0.0),
                    egui::Align2::CENTER_CENTER,
                    "▶",
                    egui::FontId::proportional(20.0),
                    Color32::WHITE,
                );
            }
            // Tamanho no canto da miniatura.
            let badge = format!("{:.1} MB", clip.size_mb);
            let galley = painter.layout_no_wrap(badge, egui::FontId::proportional(12.0), Color32::WHITE);
            let pos = rect.right_bottom() - galley.size() - Vec2::new(10.0, 8.0);
            painter.rect_filled(
                egui::Rect::from_min_size(pos - Vec2::new(5.0, 2.0), galley.size() + Vec2::new(10.0, 4.0)),
                4.0,
                Color32::from_black_alpha(170),
            );
            painter.galley(pos, galley, Color32::WHITE);
            if resp.on_hover_text(t().watch).clicked() {
                action = Some(Action::Play);
            }

            // Embaixo da miniatura: nome, data e hora, e os botões.
            egui::Frame::new().inner_margin(egui::Margin::symmetric(10, 8)).show(ui, |ui| {
                let stem = clip.name.trim_end_matches(".mp4");
                ui.add(egui::Label::new(RichText::new(stem).strong()).truncate());
                ui.label(RichText::new(&clip.when).weak());
                ui.add_space(4.0);
                let gap = 4.0;
                let bw = (ui.available_width() - gap * 2.0) / 3.0;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = gap;
                    let mut btn = |ui: &mut egui::Ui, text: RichText, tip: &str, a: Action| {
                        let b = egui::Button::new(text).min_size(Vec2::new(bw, 26.0));
                        if ui.add(b).on_hover_text(tip).clicked() {
                            action = Some(a);
                        }
                    };
                    btn(ui, t().folder.into(), t().folder_tip, Action::Reveal);
                    btn(ui, t().export.into(), t().export_tip, Action::Export);
                    btn(ui, RichText::new(t().delete).color(ACCENT), t().delete_tip, Action::Delete);
                });
            });
        });
    action
}

fn section(ui: &mut egui::Ui, title: &str, body: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(6.0);
    ui.label(RichText::new(title).strong().color(ACCENT));
    egui::Frame::new()
        .fill(CARD)
        .corner_radius(8)
        .inner_margin(10)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            body(ui);
        });
}

fn hint(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).weak().small());
}

fn key_chip(ui: &mut egui::Ui, key: &str) {
    egui::Frame::new()
        .fill(Color32::from_gray(50))
        .stroke(Stroke::new(1.0, Color32::from_gray(80)))
        .corner_radius(5)
        .inner_margin(egui::Margin::symmetric(8, 3))
        .show(ui, |ui| {
            ui.label(RichText::new(key).monospace().strong());
        });
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
        self.handle_capture(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if self.shared.history_dirty.swap(false, Ordering::SeqCst)
            || self.last_scan.elapsed().as_secs() >= 3
        {
            self.scan();
        }

        egui::Panel::top("top")
            .frame(egui::Frame::new().fill(BG).inner_margin(egui::Margin::symmetric(18, 14)))
            .show(ui, |ui| self.top_bar(ui));
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BG).inner_margin(egui::Margin { left: 18, right: 14, top: 4, bottom: 8 }))
            .show(ui, |ui| self.clips_grid(ui));

        let ctx = ui.ctx().clone();
        self.settings_modal(&ctx);
        self.delete_modal(&ctx);

        // Atualiza a bolinha e o status mesmo sem mexer o mouse.
        ctx.request_repaint_after(Duration::from_millis(250));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.shared.send(RecCmd::Quit);
    }
}
