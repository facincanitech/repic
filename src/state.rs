use crate::audio::AudioRing;
use crate::config::{self, Config};
use crate::hotkey::{Combo, Target};
use crate::update::UpdateState;
use eframe::egui;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

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
    pub last_event: Option<(bool, String, Instant)>,
}

pub enum RecCmd {
    Restart,
    Quit,
}

pub struct Paths {
    pub ffmpeg: PathBuf,
    pub buffer_dir: PathBuf,
    pub work_dir: PathBuf,
    pub record_dir: PathBuf,
    pub thumbs_dir: PathBuf,
}

pub struct Shared {
    pub config: Mutex<Config>,
    pub status: Mutex<Status>,
    pub paths: Paths,
    /// Som do PC.
    pub audio: Arc<AudioRing>,
    /// Microfone padrão do Windows.
    pub mic: Arc<AudioRing>,
    pub saving: AtomicBool,
    /// Resultado de "escolher atalho" vindo do teclado (hotkey.rs)
    /// (Some(None) = cancelou).
    pub captured: Mutex<Option<(Target, Option<Combo>)>>,
    /// Gravando daqui pra frente (o botão de gravar).
    pub recording: AtomicBool,
    /// Pediu pra parar: a gravação está terminando de salvar.
    pub rec_stop: AtomicBool,
    /// Quando a gravação começou (pra mostrar o tempo).
    pub rec_started: Mutex<Option<Instant>>,
    /// Tamanho previsto da gravação até agora, pro medidor.
    pub rec_bytes: AtomicU64,
    pub update: Mutex<UpdateState>,
    /// Até quando outro aviso fica na tela sem o medidor passar por cima.
    pub toast_until: Mutex<Option<Instant>>,
    pub history_dirty: AtomicBool,
    pub show_requested: AtomicBool,
    pub hwnd: AtomicIsize,
    pub ctx: OnceLock<egui::Context>,
    pub recorder: OnceLock<Sender<RecCmd>>,
}

impl Shared {
    pub fn new(config: Config) -> Arc<Self> {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()))
            .unwrap_or_else(|| PathBuf::from("."));
        let ext = if cfg!(windows) { ".exe" } else { "" };
        let local = exe_dir.join(format!("ffmpeg{ext}"));
        let ffmpeg = if local.exists() {
            local
        } else {
            PathBuf::from("ffmpeg")
        };
        let temp = std::env::temp_dir().join("RePic");
        Self::with_paths(config, temp, config::app_dir(), ffmpeg)
    }

    pub fn with_paths(config: Config, temp: PathBuf, app: PathBuf, ffmpeg: PathBuf) -> Arc<Self> {
        let paths = Paths {
            ffmpeg,
            buffer_dir: temp.join("buffer"),
            work_dir: temp.join("work"),
            record_dir: temp.join("record"),
            thumbs_dir: app.join("thumbs"),
        };
        let audio = AudioRing::new(config.audio, config.seconds, temp.join("audio-pc"));
        let mic = AudioRing::new(config.mic, config.seconds, temp.join("audio-mic"));
        Arc::new(Self {
            config: Mutex::new(config),
            status: Mutex::new(Status {
                health: Health::Starting,
                text: "Iniciando…".into(),
                last_event: None,
            }),
            paths,
            audio,
            mic,
            saving: AtomicBool::new(false),
            captured: Mutex::new(None),
            recording: AtomicBool::new(false),
            rec_stop: AtomicBool::new(false),
            rec_started: Mutex::new(None),
            rec_bytes: AtomicU64::new(0),
            update: Mutex::new(UpdateState::None),
            toast_until: Mutex::new(None),
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
        self.status.lock().unwrap().last_event = Some((ok, text.into(), Instant::now()));
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
