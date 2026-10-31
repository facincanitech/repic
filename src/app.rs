//! Janela: barra de cima (status + clipar), grade com os últimos clipes,
//! e as configurações num modal.

use crate::config::{
    FPS_OPTIONS, MAX_SECONDS, Quality, RES_OPTIONS, UNITS, best_unit, duration_from_input, format_size,
    record_limit_from_input,
    fit_resolution,
    format_duration,
};
use crate::hotkey::{self, Target};
use crate::lang::{fill, t};
use crate::platform::{self, Monitor};
use crate::recorder;
use crate::state::{Health, RecCmd, Shared};
use crate::update::{self, UpdateState};
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
/// Som estéreo de 48 kHz em f32, que é o caso comum.
const AUDIO_BYTES_PER_SEC: f64 = 384_000.0;

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
    capturing: Option<Target>,
    monitors: Vec<Monitor>,
    autostart: bool,
    /// Bytes de vídeo por segundo, medido ao abrir as configurações.
    video_rate: f64,
    /// Maior duração que cabe no disco agora.
    max_seconds: u32,
    /// O que está no campo de duração, e a unidade dele em segundos.
    duration_text: String,
    duration_unit: u32,
    /// O que está no campo de limite da gravação, e a unidade (1 = MB, 1000 = GB).
    limit_text: String,
    limit_unit: u32,
    /// Quando reiniciar a gravação por causa de uma configuração nova.
    restart_at: Option<Instant>,
    /// Aberto com --tray: quadros que faltam pra esconder a janela (o eframe
    /// mostra ela sozinho depois do primeiro quadro, mesmo pedindo escondida).
    hide_in: Option<u8>,
}

impl ClipApp {
    pub fn new(shared: Arc<Shared>, tray: Option<tray_icon::TrayIcon>, start_hidden: bool) -> Self {
        Self {
            hide_in: start_hidden.then_some(1),
            shared,
            tray,
            clips: Vec::new(),
            last_scan: Instant::now() - Duration::from_secs(60),
            thumbs: HashMap::new(),
            confirm_delete: None,
            settings_open: false,
            capturing: None,
            monitors: Vec::new(),
            autostart: platform::autostart_enabled(),
            video_rate: 0.0,
            max_seconds: MAX_SECONDS,
            duration_text: String::new(),
            duration_unit: 1,
            limit_text: String::new(),
            limit_unit: 1,
            restart_at: None,
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

    fn start_capture(&mut self, target: Target) {
        *self.shared.captured.lock().unwrap() = None;
        hotkey::begin_capture(target);
        self.capturing = Some(target);
    }

    fn cancel_capture(&mut self) {
        hotkey::cancel_capture();
        self.capturing = None;
    }

    /// Recebe o atalho escolhido (ou o Esc) do gancho.
    fn handle_capture(&mut self, ctx: &egui::Context) {
        let Some((target, result)) = self.shared.captured.lock().unwrap().take() else {
            return;
        };
        self.capturing = None;
        // O Esc de cancelar não fecha as configurações junto.
        ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        let Some(combo) = result else {
            return;
        };
        if target == Target::Clip
            && let Some(t) = &self.tray
        {
            let _ = t.set_tooltip(Some(crate::tray_tooltip(&hotkey::label(&combo))));
        }
        let mut cfg = self.shared.config.lock().unwrap();
        match target {
            Target::Clip => cfg.shortcut = combo,
            Target::Record => cfg.record_shortcut = combo,
        }
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
                if ui.add_enabled(!saving, btn).on_hover_text(t().clip_tip).clicked() {
                    recorder::trigger_save(self.shared.clone());
                }

                let recording = self.shared.recording.load(Ordering::SeqCst);
                let stopping = self.shared.rec_stop.load(Ordering::SeqCst);
                let (label, fill) = if recording && stopping {
                    (t().saving_btn.to_string(), Color32::from_gray(60))
                } else if recording {
                    let secs = self
                        .shared
                        .rec_started
                        .lock()
                        .unwrap()
                        .map_or(0, |at| at.elapsed().as_secs());
                    let clock = format!("{:02}:{:02}", secs / 60, secs % 60);
                    let size = format_size(self.shared.rec_bytes.load(Ordering::SeqCst));
                    (format!("{}  ·  {clock}  ·  {size}", t().stop_btn), ACCENT)
                } else {
                    let key = hotkey::label(&cfg.record_shortcut);
                    (format!("{}  ·  {key}", t().record_btn), Color32::from_gray(60))
                };
                let btn = egui::Button::new(RichText::new(label).color(Color32::WHITE).strong())
                    .fill(fill)
                    .min_size(Vec2::new(0.0, 34.0));
                if ui
                    .add_enabled(!(recording && stopping), btn)
                    .on_hover_text(t().record_tip)
                    .clicked()
                {
                    recorder::toggle_recording(self.shared.clone());
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
        self.update_banner(ui);
    }

    /// Avisa de versão nova, com o botão de atualizar.
    fn update_banner(&self, ui: &mut egui::Ui) {
        let state = self.shared.update.lock().unwrap().clone();
        if state == UpdateState::None {
            return;
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| match state {
            UpdateState::Available(release) => {
                let text = fill(t().update_available, &[&release.version]);
                ui.label(RichText::new(text).color(OK_GREEN).strong());
                // Instalar fecha o RePic: não pode no meio de uma gravação.
                let idle = !self.shared.recording.load(Ordering::SeqCst)
                    && !self.shared.saving.load(Ordering::SeqCst);
                let btn = ui
                    .add_enabled(idle, egui::Button::new(t().update_btn))
                    .on_disabled_hover_text(t().update_busy);
                if btn.clicked() {
                    update::install(self.shared.clone());
                }
            }
            UpdateState::Working => {
                ui.spinner();
                ui.label(t().update_working);
            }
            UpdateState::Failed => {
                ui.label(RichText::new(t().update_failed).color(ACCENT));
                if ui.button(t().update_site).clicked() {
                    platform::open_url(update::SITE_URL);
                }
            }
            UpdateState::None => {}
        });
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
                let title = fill(t().empty_title, &[&key, &format_duration(cfg.seconds)]);
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

    /// Põe a duração no campo: mantém a unidade escolhida se der número inteiro.
    fn show_duration(&mut self, seconds: u32) {
        if !UNITS.contains(&self.duration_unit)
            || seconds % self.duration_unit != 0
            || self.duration_text.is_empty()
        {
            self.duration_unit = best_unit(seconds);
        }
        self.duration_text = (seconds / self.duration_unit).to_string();
    }

    /// Põe o limite no campo: 2000 MB aparece como 2 GB, e 0 como vazio.
    fn show_limit(&mut self, limit_mb: u32) {
        if limit_mb == 0 {
            self.limit_text.clear();
            return;
        }
        self.limit_unit = if limit_mb % 1000 == 0 { 1000 } else { 1 };
        self.limit_text = (limit_mb / self.limit_unit).to_string();
    }

    fn open_settings(&mut self) {
        // Ao abrir, sempre a forma mais simples: 7200 aparece como 2 horas.
        self.duration_text.clear();
        self.monitors = platform::list_monitors();
        self.autostart = platform::autostart_enabled();
        // O buffer fica na pasta temporária: é o disco dela que limita o tempo.
        self.video_rate = recorder::video_bytes_per_sec(&self.shared);
        self.max_seconds = platform::free_disk(&self.shared.paths.buffer_dir)
            .map(|free| max_seconds_for_disk(free, self.video_rate))
            .unwrap_or(MAX_SECONDS);
        self.show_duration(self.shared.config().seconds);
        self.show_limit(self.shared.config().record_limit_mb);
        self.settings_open = true;
    }

    /// Ligou/desligou "Rodar como administrador" (já salvo no config).
    fn apply_admin(&mut self) {
        let admin = self.shared.config().admin;
        if admin && !platform::is_admin() {
            // Reabre como admin; o novo espera este fechar (--restart).
            if platform::relaunch_as_admin("--restart") {
                crate::quit(&self.shared);
            }
            let mut cfg = self.shared.config();
            cfg.admin = false;
            cfg.save();
            *self.shared.config.lock().unwrap() = cfg;
            self.shared.set_event(false, t().admin_declined);
            return;
        }
        // Troca o "Iniciar com o Windows" entre tarefa de admin e registro.
        if self.autostart
            && let Err(e) = platform::set_autostart(true, admin && platform::is_admin())
        {
            self.shared.set_event(false, fill(t().autostart_failed, &[&e]));
            return;
        }
        let msg = if admin { t().admin_on_now } else { t().admin_off_later };
        self.shared.set_event(true, msg);
    }

    fn settings_modal(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let mut cfg = self.shared.config();
        let before = cfg.clone();
        let mut autostart = self.autostart;
        let mut change_key: Option<Target> = None;
        let mut commit_duration = false;
        let mut commit_limit = false;
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

            // Em janela baixa, as configurações rolam em vez de cortar o topo.
            let max_h = (ctx.content_rect().height() - 150.0).max(200.0);
            egui::ScrollArea::vertical().max_height(max_h).show(ui, |ui| {
                section(ui, t().sec_shortcut, |ui| {
                    for (target, label, combo) in [
                        (Target::Clip, t().key_to_clip, cfg.shortcut),
                        (Target::Record, t().key_to_record, cfg.record_shortcut),
                    ] {
                        ui.horizontal(|ui| {
                            ui.label(label);
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if self.capturing == Some(target) {
                                    if ui.button(t().cancel).clicked() {
                                        change_key = Some(target);
                                    }
                                    ui.label(RichText::new(t().press_key).color(ACCENT));
                                } else {
                                    if ui.button(t().change).clicked() {
                                        change_key = Some(target);
                                    }
                                    key_chip(ui, &hotkey::label(&combo));
                                }
                            });
                        });
                    }
                    hint(ui, t().record_hint);
                    hint(ui, t().shortcut_hint);
                });

                section(ui, t().sec_recording, |ui| {
                    egui::Grid::new("rec_grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
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

                        ui.label(t().duration);
                        ui.horizontal(|ui| {
                            let field = egui::TextEdit::singleline(&mut self.duration_text)
                                .desired_width(56.0)
                                .char_limit(5);
                            let typed = ui.add(field);
                            // Só número inteiro: "2:01" nem tem como entrar.
                            self.duration_text.retain(|c| c.is_ascii_digit());
                            commit_duration |= typed.lost_focus();
                            let number = self.duration_text.parse::<u32>().unwrap_or(0);
                            let name = |unit: u32| {
                                let forms = t().units[UNITS.iter().position(|u| *u == unit).unwrap_or(0)];
                                forms[usize::from(number != 1)]
                            };
                            let before_unit = self.duration_unit;
                            egui::ComboBox::from_id_salt("dur_unit")
                                .selected_text(name(self.duration_unit))
                                .show_ui(ui, |ui| {
                                    for unit in UNITS {
                                        ui.selectable_value(&mut self.duration_unit, unit, name(unit));
                                    }
                                });
                            commit_duration |= self.duration_unit != before_unit;
                        });
                        ui.end_row();

                        // O "nativo" vem do monitor escolhido; dá pra reduzir.
                        let mon = platform::pick_monitor(&self.monitors, &cfg.monitor);
                        let native_fps = mon.as_ref().map(|m| m.refresh).unwrap_or(60);
                        let native_label = fill(t().native_fps, &[&native_fps]);
                        ui.label("FPS");
                        let fps_text = if cfg.frame_rate == 0 || cfg.frame_rate >= native_fps {
                            native_label.clone()
                        } else {
                            format!("{} fps", cfg.frame_rate)
                        };
                        egui::ComboBox::from_id_salt("fps")
                            .selected_text(fps_text)
                            .width(220.0)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut cfg.frame_rate, 0, native_label);
                                for f in FPS_OPTIONS.into_iter().filter(|&f| f < native_fps) {
                                    ui.selectable_value(&mut cfg.frame_rate, f, format!("{f} fps"));
                                }
                            });
                        ui.end_row();

                        let (nw, nh) = mon.as_ref().map(|m| (m.width, m.height)).unwrap_or((1920, 1080));
                        let native_label = fill(t().native_res, &[&nw, &nh]);
                        ui.label(t().resolution);
                        // Mostra o tamanho real: num ultrawide, 1080p fica 1920×810.
                        let res_label = |h: u32| {
                            fit_resolution(nw, nh, h).map(|(fw, fh)| format!("{h}p · {fw}×{fh}"))
                        };
                        let res_text =
                            res_label(cfg.resolution).unwrap_or_else(|| native_label.clone());
                        egui::ComboBox::from_id_salt("res")
                            .selected_text(res_text)
                            .width(220.0)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut cfg.resolution, 0, native_label);
                                for h in RES_OPTIONS {
                                    if let Some(text) = res_label(h) {
                                        ui.selectable_value(&mut cfg.resolution, h, text);
                                    }
                                }
                            });
                        ui.end_row();

                        let q_label = |q: Quality| {
                            let name = t().q_names[q.index()];
                            if q == Quality::default() {
                                format!("{name}{}", t().recommended)
                            } else {
                                name.to_string()
                            }
                        };
                        ui.label(t().quality);
                        egui::ComboBox::from_id_salt("quality")
                            .selected_text(q_label(cfg.quality))
                            .width(220.0)
                            .show_ui(ui, |ui| {
                                for q in Quality::ALL {
                                    ui.selectable_value(&mut cfg.quality, q, q_label(q));
                                }
                            });
                        ui.end_row();

                        ui.label(t().rec_limit);
                        ui.horizontal(|ui| {
                            let field = egui::TextEdit::singleline(&mut self.limit_text)
                                .desired_width(56.0)
                                .char_limit(7);
                            let typed = ui.add(field);
                            self.limit_text.retain(|c| c.is_ascii_digit());
                            commit_limit |= typed.lost_focus();
                            let unit_name = |u: u32| if u == 1000 { "GB" } else { "MB" };
                            let before_unit = self.limit_unit;
                            egui::ComboBox::from_id_salt("limit_unit")
                                .selected_text(unit_name(self.limit_unit))
                                .width(56.0)
                                .show_ui(ui, |ui| {
                                    for u in [1, 1000] {
                                        ui.selectable_value(&mut self.limit_unit, u, unit_name(u));
                                    }
                                });
                            commit_limit |= self.limit_unit != before_unit;
                        });
                        ui.end_row();
                    });
                    hint(ui, &fill(t().duration_limit, &[&format_duration(self.max_seconds)]));
                    if cfg.seconds > 120 {
                        let (recording, saving) = disk_use(cfg.seconds, self.video_rate);
                        hint(ui, &fill(t().disk_use, &[&recording, &saving]));
                    }
                    if cfg.seconds > self.max_seconds {
                        ui.label(RichText::new(t().disk_low).small().color(ACCENT));
                    }
                    hint(ui, t().q_hints[cfg.quality.index()]);
                    hint(ui, t().rec_limit_hint);
                    let mon = platform::pick_monitor(&self.monitors, &cfg.monitor);
                    if mon.is_some_and(|m| fit_resolution(m.width, m.height, cfg.resolution).is_some()) {
                        hint(ui, t().scale_hint);
                    }
                    ui.add_space(4.0);
                    ui.checkbox(&mut cfg.anywhere, t().anywhere);
                    hint(ui, if cfg.anywhere { t().anywhere_on } else { t().anywhere_off });
                    ui.add_space(4.0);
                    ui.checkbox(&mut cfg.notify, t().notify);
                    hint(ui, t().notify_hint);
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
                    ui.checkbox(&mut cfg.check_updates, t().update_check);
                    hint(ui, t().update_check_hint);
                    ui.add_space(4.0);
                    ui.checkbox(&mut cfg.admin, t().admin);
                    hint(ui, t().admin_hint);
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
        if resp.should_close() && self.capturing.is_none() {
            close = true;
        }

        // Aplica o que mudou.
        if let Some(target) = change_key {
            if self.capturing == Some(target) {
                self.cancel_capture();
            } else {
                self.start_capture(target);
            }
        }
        if pick_dir && let Some(dir) = platform::pick_folder(&cfg.output_dir) {
            cfg.output_dir = dir;
        }
        if autostart != self.autostart {
            match platform::set_autostart(autostart, cfg.admin && platform::is_admin()) {
                Ok(()) => self.autostart = autostart,
                Err(e) => self.shared.set_event(false, fill(t().autostart_failed, &[&e])),
            }
        }
        // A duração só vale ao sair do campo: digitar "120" não pode reiniciar
        // a gravação em 1, 12 e 120.
        if commit_limit || close {
            let value = self.limit_text.parse::<u32>().unwrap_or(0);
            cfg.record_limit_mb = record_limit_from_input(value, self.limit_unit);
            self.show_limit(cfg.record_limit_mb);
        }
        if commit_duration || close {
            if let Ok(value) = self.duration_text.parse::<u32>()
                && value.saturating_mul(self.duration_unit) != cfg.seconds
            {
                cfg.seconds = duration_from_input(value, self.duration_unit, self.max_seconds);
            }
            self.show_duration(cfg.seconds);
        }
        if cfg != before {
            let needs_restart =
                cfg.frame_rate != before.frame_rate
                    || cfg.resolution != before.resolution
                    || cfg.quality != before.quality
                    || cfg.seconds != before.seconds
                    || cfg.monitor != before.monitor;
            self.shared.audio.enabled.store(cfg.audio, Ordering::Relaxed);
            self.shared.mic.enabled.store(cfg.mic, Ordering::Relaxed);
            self.shared.audio.set_keep_secs(cfg.seconds);
            self.shared.mic.set_keep_secs(cfg.seconds);
            if cfg.output_dir != before.output_dir {
                self.shared.history_dirty.store(true, Ordering::SeqCst);
            }
            cfg.shortcut = self.shared.config().shortcut; // salvo à parte em handle_capture
            let admin_changed = cfg.admin != before.admin;
            cfg.save();
            *self.shared.config.lock().unwrap() = cfg;
            if needs_restart {
                // Espera parar de clicar no − e no +: reiniciar apaga o buffer.
                self.restart_at = Some(Instant::now() + Duration::from_secs(1));
            }
            if admin_changed {
                self.apply_admin();
            }
        }
        if close {
            if self.capturing.is_some() {
                self.cancel_capture();
            }
            self.settings_open = false;
            self.flush_restart();
        }
    }

    /// Aplica um reinício pendente da gravação.
    fn flush_restart(&mut self) {
        if self.restart_at.take().is_some() {
            self.shared.send(RecCmd::Restart);
        }
    }
}

/// (ocupado enquanto grava, pico ao salvar), em GB, já com o som. Ao salvar,
/// o buffer, a cópia intermediária e o clipe final existem ao mesmo tempo.
fn disk_use(seconds: u32, video_rate: f64) -> (String, String) {
    const GB: f64 = 1_000_000_000.0;
    let recording = seconds as f64 * (video_rate + AUDIO_BYTES_PER_SEC);
    let saving = seconds as f64 * (3.0 * video_rate + 2.0 * AUDIO_BYTES_PER_SEC);
    (format!("{:.1}", recording / GB), format!("{:.1}", saving / GB))
}

/// Maior tempo que cabe em `free` bytes, considerando o pico ao salvar.
fn max_seconds_for_disk(free: u64, video_rate: f64) -> u32 {
    let per_sec = 3.0 * video_rate + 2.0 * AUDIO_BYTES_PER_SEC;
    // 2 minutos sempre pode: é o que já existia antes das durações longas.
    ((free as f64 / per_sec) as u32).clamp(120, MAX_SECONDS)
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
        if let Some(n) = self.hide_in {
            if n == 0 {
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                self.hide_in = None;
            } else {
                self.hide_in = Some(n - 1);
                ctx.request_repaint();
            }
        }
        // Pedido da bandeja pra mostrar a janela.
        if self.shared.show_requested.swap(false, Ordering::SeqCst) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            self.shared.history_dirty.store(true, Ordering::SeqCst);
        }
        if self.restart_at.is_some_and(|at| Instant::now() >= at) {
            self.flush_restart();
        }
        // Fechar = esconder na bandeja.
        if ctx.input(|i| i.viewport().close_requested()) {
            // Escondida, a janela para de rodar: aplica o que estava pendente.
            self.flush_restart();
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
