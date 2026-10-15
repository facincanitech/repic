use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const SECONDS_OPTIONS: [u32; 5] = [15, 30, 60, 90, 120];
pub const FPS_OPTIONS: [u32; 2] = [30, 60];
pub const HOTKEY_OPTIONS: [&str; 7] = ["F6", "F7", "F8", "F9", "F10", "F11", "F12"];

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    /// Quantos segundos do passado vão pro clipe.
    pub seconds: u32,
    pub fps: u32,
    pub hotkey: String,
    /// Captura o som do PC (o que sai nos fones/caixas).
    pub audio: bool,
    pub output_dir: PathBuf,
}

impl Default for Config {
    fn default() -> Self {
        let videos = dirs::video_dir()
            .or_else(|| dirs::home_dir().map(|h| h.join("Videos")))
            .unwrap_or_else(|| PathBuf::from("."));
        Self {
            seconds: 30,
            fps: 60,
            hotkey: "F8".into(),
            audio: true,
            output_dir: videos.join("RePic"),
        }
    }
}

pub fn app_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("RePic")
}

fn config_path() -> PathBuf {
    app_dir().join("config.json")
}

impl Config {
    pub fn load() -> Self {
        std::fs::read_to_string(config_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let _ = std::fs::create_dir_all(app_dir());
        if let Ok(s) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(config_path(), s);
        }
    }
}
