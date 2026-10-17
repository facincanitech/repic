use crate::hotkey::Combo;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const SECONDS_OPTIONS: [u32; 5] = [15, 30, 60, 90, 120];
/// FPS pra reduzir (o padrão é o nativo do monitor). Do maior pro menor.
pub const FPS_OPTIONS: [u32; 6] = [240, 165, 144, 120, 60, 30];
/// Alturas pra reduzir a resolução (o padrão é a nativa). Do maior pro menor.
pub const RES_OPTIONS: [u32; 4] = [2160, 1440, 1080, 720];

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum Quality {
    /// Arquivos pequenos, bom pra mandar em chat.
    Light,
    Normal,
    /// Recomendado.
    #[default]
    High,
    /// Arquivo bem maior.
    Max,
}

impl Quality {
    pub const ALL: [Quality; 4] = [Quality::Light, Quality::Normal, Quality::High, Quality::Max];

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|q| *q == self).unwrap()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    /// Quantos segundos do passado vão pro clipe.
    pub seconds: u32,
    /// FPS da gravação. 0 = nativo do monitor (padrão).
    pub frame_rate: u32,
    /// Altura da gravação. 0 = resolução nativa do monitor (padrão).
    pub resolution: u32,
    pub quality: Quality,
    /// Atalho de clipar (qualquer tecla, com ou sem Ctrl/Alt/Shift).
    pub shortcut: Combo,
    /// Captura o som do PC (o que sai nos fones/caixas).
    pub audio: bool,
    /// Captura o microfone padrão do Windows.
    pub mic: bool,
    /// true: o atalho clipa em qualquer lugar. false: só com jogo/app em tela cheia.
    pub anywhere: bool,
    /// Monitor gravado (ver `platform::Monitor::id`). Vazio = o principal do Windows.
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
            frame_rate: 0,
            resolution: 0,
            quality: Quality::default(),
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
            // O Bloco de Notas às vezes salva com BOM no começo.
            .and_then(|s| serde_json::from_str(s.trim_start_matches('\u{feff}')).ok())
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
        if self.frame_rate != 0 && !FPS_OPTIONS.contains(&self.frame_rate) {
            self.frame_rate = 0;
        }
        if self.resolution != 0 && !RES_OPTIONS.contains(&self.resolution) {
            self.resolution = 0;
        }
        if self.shortcut.vk == 0 || self.shortcut.vk > 0xFE {
            self.shortcut = def.shortcut;
        }
        // v0.2 antigo guardava o nome do Windows (`\\.\DISPLAY5`), que muda
        // sozinho. Esse formato vira "principal".
        if self.monitor.starts_with("\\\\") {
            self.monitor.clear();
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
            frame_rate: 97,
            resolution: 333,
            monitor: "\\\\.\\DISPLAY8".into(),
            shortcut: Combo {
                vk: 0,
                ..Combo::default()
            },
            ..Config::default()
        };
        cfg.validate();
        assert_eq!(cfg.seconds, 30);
        assert_eq!(cfg.frame_rate, 0);
        assert_eq!(cfg.resolution, 0);
        assert_eq!(cfg.shortcut, Combo::default());
        assert!(cfg.monitor.is_empty());
    }

    #[test]
    fn config_with_bom_still_parses() {
        let s = "\u{feff}{\"seconds\":90}";
        let cfg: Config = serde_json::from_str(s.trim_start_matches('\u{feff}')).unwrap();
        assert_eq!(cfg.seconds, 90);
    }

    #[test]
    fn old_config_without_new_fields_still_loads() {
        // v0.1 guardava "hotkey" como texto: é ignorado e o atalho volta pro F8.
        let cfg: Config = serde_json::from_str(r#"{"seconds":60,"fps":30,"hotkey":"F9"}"#).unwrap();
        assert_eq!(cfg.seconds, 60);
        assert_eq!(cfg.shortcut, Combo::default());
        // O "fps" antigo (30/60 fixo) é ignorado: agora o padrão é o nativo.
        assert_eq!(cfg.frame_rate, 0);
        assert_eq!(cfg.quality, Quality::High);
        assert!(cfg.anywhere);
        assert!(!cfg.mic);
        assert!(cfg.monitor.is_empty());
    }
}
