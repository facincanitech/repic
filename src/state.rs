use crate::audio::AudioRing;
use crate::config::{self, Config};
use eframe::egui;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, OnceLock};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Health {
    Starting,
    Recording,
    Error,
}

#[derive(Clone)]
pub struct Status {
    pub health: Health,
    pub text: String,
    /// Última mensagem de salvamento ("Clipe salvo", erro, etc).
    pub last_event: Option<(bool, String)>,
}

pub enum RecCmd {
    Restart,
    Quit,
}

pub struct Paths {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
    pub buffer_dir: PathBuf,
    pub work_dir: PathBuf,
    pub thumbs_dir: PathBuf,
}

pub struct Shared {
    pub config: Mutex<Config>,
    pub status: Mutex<Status>,
    pub paths: Paths,
    pub audio: Arc<AudioRing>,
    pub saving: AtomicBool,
    pub history_dirty: AtomicBool,
    pub show_requested: AtomicBool,
    pub hwnd: AtomicIsize,
    pub ctx: OnceLock<egui::Context>,
    pub recorder: OnceLock<Sender<RecCmd>>,
}

impl Shared {
    pub fn new(config: Config) -> Arc<Self> {
        let app = config::app_dir();
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()))
            .unwrap_or_else(|| PathBuf::from("."));
        let ext = if cfg!(windows) { ".exe" } else { "" };
        let find = |name: &str| {
            let local = exe_dir.join(format!("{name}{ext}"));
            if local.exists() {
                local
            } else {
                PathBuf::from(name)
            }
        };
        let temp = std::env::temp_dir().join("ClipCraft");
        let paths = Paths {
            ffmpeg: find("ffmpeg"),
            ffprobe: find("ffprobe"),
            buffer_dir: temp.join("buffer"),
            work_dir: temp.join("work"),
            thumbs_dir: app.join("thumbs"),
        };
        let audio = AudioRing::new(config.audio, config.seconds);
        Arc::new(Self {
            config: Mutex::new(config),
            status: Mutex::new(Status {
                health: Health::Starting,
                text: "Iniciando…".into(),
                last_event: None,
            }),
            paths,
            audio,
            saving: AtomicBool::new(false),
            history_dirty: AtomicBool::new(true),
            show_requested: AtomicBool::new(false),
            hwnd: AtomicIsize::new(0),
            ctx: OnceLock::new(),
            recorder: OnceLock::new(),
        })
    }

    pub fn config(&self) -> Config {
        self.config.lock().unwrap().clone()
    }

    pub fn set_status(&self, health: Health, text: impl Into<String>) {
        {
            let mut s = self.status.lock().unwrap();
            s.health = health;
            s.text = text.into();
        }
        self.repaint();
    }

    pub fn set_event(&self, ok: bool, text: impl Into<String>) {
        self.status.lock().unwrap().last_event = Some((ok, text.into()));
        self.repaint();
    }

    pub fn repaint(&self) {
        if let Some(ctx) = self.ctx.get() {
            ctx.request_repaint();
        }
    }

    pub fn send(&self, cmd: RecCmd) {
        if let Some(tx) = self.recorder.get() {
            let _ = tx.send(cmd);
        }
    }

    /// Traz a janela de volta (chamado pela bandeja).
    pub fn request_show(&self) {
        self.show_requested.store(true, Ordering::SeqCst);
        crate::platform::show_window(self.hwnd.load(Ordering::SeqCst));
        self.repaint();
    }
}
