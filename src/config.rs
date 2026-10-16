use crate::hotkey::Combo;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const SECONDS_OPTIONS: [u32; 5] = [15, 30, 60, 90, 120];
pub const FPS_OPTIONS: [u32; 2] = [30, 60];

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    /// Quantos segundos do passado vão pro clipe.
    pub seconds: u32,
    pub fps: u32,
    /// Atalho de clipar (qualquer tecla, com ou sem Ctrl/Alt/Shift).
    pub shortcut: Combo,
    /// Captura o som do PC (o que sai nos fones/caixas).
    pub audio: bool,
    /// Captura o microfone padrão do Windows.
    pub mic: bool,
    /// true: o atalho clipa em qualquer lugar. false: só com jogo/app em tela cheia.
    pub anywhere: bool,
    /// Monitor gravado (`\\.\DISPLAY2`). Vazio = o principal do Windows.
    pub monitor: String,
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
            shortcut: Combo::default(),
            audio: true,
            mic: false,
            anywhere: true,
            monitor: String::new(),
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
        let mut cfg: Self = std::fs::read_to_string(config_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        cfg.validate();
        cfg
    }

    /// Conserta valores inválidos (ex.: config.json editado na mão).
    pub fn validate(&mut self) {
        let def = Self::default();
        if !SECONDS_OPTIONS.contains(&self.seconds) {
            self.seconds = def.seconds;
        }
        if !FPS_OPTIONS.contains(&self.fps) {
            self.fps = def.fps;
        }
        if self.shortcut.vk == 0 || self.shortcut.vk > 0xFE {
            self.shortcut = def.shortcut;
        }
        if self.output_dir.as_os_str().is_empty() {
            self.output_dir = def.output_dir;
        }
    }

    pub fn save(&self) {
        let _ = std::fs::create_dir_all(app_dir());
        if let Ok(s) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(config_path(), s);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_fixes_hand_edited_values() {
        let mut cfg = Config {
            seconds: 7,
            fps: 144,
            shortcut: Combo {
                vk: 0,
                ..Combo::default()
            },
            ..Config::default()
        };
        cfg.validate();
        assert_eq!(cfg.seconds, 30);
        assert_eq!(cfg.fps, 60);
        assert_eq!(cfg.shortcut, Combo::default());
    }

    #[test]
    fn old_config_without_new_fields_still_loads() {
        // v0.1 guardava "hotkey" como texto: é ignorado e o atalho volta pro F8.
        let cfg: Config = serde_json::from_str(r#"{"seconds":60,"fps":30,"hotkey":"F9"}"#).unwrap();
        assert_eq!(cfg.seconds, 60);
        assert_eq!(cfg.shortcut, Combo::default());
        assert!(cfg.anywhere);
        assert!(!cfg.mic);
        assert!(cfg.monitor.is_empty());
    }
}
