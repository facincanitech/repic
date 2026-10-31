use crate::hotkey::{Combo, same_keys};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// FPS pra reduzir (o padrão é o nativo do monitor). Do maior pro menor.
pub const FPS_OPTIONS: [u32; 6] = [240, 165, 144, 120, 60, 30];
/// Alturas pra reduzir a resolução (o padrão é a nativa). Do maior pro menor.
pub const RES_OPTIONS: [u32; 4] = [2160, 1440, 1080, 720];
pub const MIN_SECONDS: u32 = 15;
/// Até 2 horas: mais que isso o buffer ocuparia disco demais o tempo todo.
pub const MAX_SECONDS: u32 = 2 * 60 * 60;

/// Unidades do campo de duração, em segundos.
pub const UNITS: [u32; 3] = [1, 60, 3600];

/// Unidade em que o tempo aparece inteiro: 7200 = 2 horas, 90 = 90 segundos.
pub fn best_unit(seconds: u32) -> u32 {
    match seconds {
        s if s >= 3600 && s % 3600 == 0 => 3600,
        s if s >= 60 && s % 60 == 0 => 60,
        _ => 1,
    }
}

/// Tempo digitado -> segundos, dentro do limite e sempre inteiro na unidade:
/// 2 horas nunca vira 2h e 1 minuto. Só quebra a unidade se o disco não
/// comportar nem 1 dela (aí fica no maior tempo que cabe).
pub fn duration_from_input(value: u32, unit: u32, max: u32) -> u32 {
    // "0 minutos" vira 1 minuto, não 15 segundos numa unidade que ninguém pediu.
    let clamped = value
        .max(1)
        .saturating_mul(unit)
        .clamp(MIN_SECONDS, max.max(MIN_SECONDS));
    let whole = clamped / unit * unit;
    if whole >= MIN_SECONDS { whole } else { clamped }
}

/// Limite de tamanho digitado -> MB. Menos que um pedaço de gravação não daria
/// nem pra começar, então o mínimo é 10 MB. 0 = sem limite.
pub fn record_limit_from_input(value: u32, unit: u32) -> u32 {
    const MIN_MB: u32 = 10;
    const MAX_MB: u32 = 1_000_000; // 1 TB
    match value {
        0 => 0,
        v => v.saturating_mul(unit).clamp(MIN_MB, MAX_MB),
    }
}

/// "850 MB", "1.2 GB".
pub fn format_size(bytes: u64) -> String {
    let mb = bytes as f64 / 1_000_000.0;
    if mb >= 1000.0 {
        format!("{:.1} GB", mb / 1000.0)
    } else {
        format!("{mb:.0} MB")
    }
}

/// "90s", "2min 10s", "1h 30min".
pub fn format_duration(seconds: u32) -> String {
    if seconds < 120 {
        return format!("{seconds}s");
    }
    let (h, m, s) = (seconds / 3600, seconds % 3600 / 60, seconds % 60);
    let mut parts = Vec::new();
    if h > 0 {
        parts.push(format!("{h}h"));
    }
    if m > 0 {
        parts.push(format!("{m}min"));
    }
    if s > 0 {
        parts.push(format!("{s}s"));
    }
    parts.join(" ")
}

/// Tamanho final pra uma opção como "1080p": cabe em 1920×1080 sem esticar
/// nem cortar (num ultrawide de 2560×1080 fica 1920×810). None = o monitor já
/// cabe, então essa opção não reduz nada.
pub fn fit_resolution(width: i32, height: i32, target: u32) -> Option<(u32, u32)> {
    let box_w = target as f64 * 16.0 / 9.0;
    let box_h = target as f64;
    let (w, h) = (width as f64, height as f64);
    if target == 0 || width <= 0 || height <= 0 || (w <= box_w && h <= box_h) {
        return None;
    }
    let scale = (box_w / w).min(box_h / h);
    // Largura e altura pares: o H.264 exige.
    let even = |v: f64| (v.round() as u32) / 2 * 2;
    Some((even(w * scale), even(h * scale)))
}

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
    /// Atalho de começar/parar a gravação daqui pra frente.
    pub record_shortcut: Combo,
    /// Limite de tamanho da gravação, em MB. 0 = sem limite.
    pub record_limit_mb: u32,
    /// Captura o som do PC (o que sai nos fones/caixas).
    pub audio: bool,
    /// Captura o microfone padrão do Windows.
    pub mic: bool,
    /// true: o atalho clipa em qualquer lugar. false: só com jogo/app em tela cheia.
    pub anywhere: bool,
    /// Aviso no canto da tela ao clipar ("Clipe salvo").
    pub notify: bool,
    /// Abre como administrador: necessário pra clipar jogos que rodam como
    /// admin (o Windows esconde o teclado deles dos programas comuns).
    pub admin: bool,
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
            record_shortcut: Combo::record_default(),
            record_limit_mb: 0,
            audio: true,
            mic: false,
            anywhere: true,
            notify: true,
            admin: true,
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
        if !(MIN_SECONDS..=MAX_SECONDS).contains(&self.seconds) {
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
        // Dois atalhos na mesma tecla: a segunda função nunca dispararia.
        let invalid = self.record_shortcut.vk == 0 || self.record_shortcut.vk > 0xFE;
        if invalid || same_keys(&self.record_shortcut, &self.shortcut) {
            self.record_shortcut = def.record_shortcut;
            if same_keys(&self.record_shortcut, &self.shortcut) {
                self.record_shortcut.vk = 0x79; // F10
                self.record_shortcut.scan = 0x44;
            }
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
    fn record_limit_and_size_text() {
        assert_eq!(record_limit_from_input(0, 1000), 0);
        assert_eq!(record_limit_from_input(5, 1), 10);
        assert_eq!(record_limit_from_input(500, 1), 500);
        assert_eq!(record_limit_from_input(2, 1000), 2000);
        assert_eq!(format_size(412_000_000), "412 MB");
        assert_eq!(format_size(1_250_000_000), "1.2 GB");
    }

    #[test]
    fn the_two_shortcuts_never_share_a_key() {
        let mut cfg = Config::default();
        cfg.shortcut = Combo::record_default();
        cfg.validate();
        assert_eq!(cfg.shortcut, Combo::record_default());
        assert_eq!(cfg.record_shortcut.vk, 0x79);
        // Config antigo, sem o atalho de gravar, ganha o F9.
        let cfg: Config = serde_json::from_str(r#"{"seconds":60}"#).unwrap();
        assert_eq!(cfg.record_shortcut, Combo::record_default());
    }

    #[test]
    fn typed_duration_stays_whole_and_in_range() {
        assert_eq!(duration_from_input(2, 3600, MAX_SECONDS), 7200);
        assert_eq!(duration_from_input(3, 3600, MAX_SECONDS), 7200);
        assert_eq!(duration_from_input(20, 60, MAX_SECONDS), 1200);
        assert_eq!(duration_from_input(150, 60, MAX_SECONDS), 7200);
        assert_eq!(duration_from_input(45, 1, MAX_SECONDS), 45);
        assert_eq!(duration_from_input(5, 1, MAX_SECONDS), 15);
        assert_eq!(duration_from_input(0, 60, MAX_SECONDS), 60);
        // Limite do disco de 5min30s: em minutos, fica nos 5 inteiros.
        assert_eq!(duration_from_input(20, 60, 330), 300);
        // Não cabe nem 1 hora: fica no maior tempo que cabe.
        assert_eq!(duration_from_input(1, 3600, 330), 330);
    }

    #[test]
    fn shows_each_duration_in_its_whole_unit() {
        assert_eq!(best_unit(30), 1);
        assert_eq!(best_unit(90), 1);
        assert_eq!(best_unit(60), 60);
        assert_eq!(best_unit(1200), 60);
        assert_eq!(best_unit(7200), 3600);
    }

    #[test]
    fn duration_text() {
        assert_eq!(format_duration(90), "90s");
        assert_eq!(format_duration(120), "2min");
        assert_eq!(format_duration(130), "2min 10s");
        assert_eq!(format_duration(3600), "1h");
        assert_eq!(format_duration(5400), "1h 30min");
    }

    #[test]
    fn long_durations_are_kept_when_loading() {
        let mut cfg = Config {
            seconds: 1230,
            ..Config::default()
        };
        cfg.validate();
        assert_eq!(cfg.seconds, 1230);
        cfg.seconds = MAX_SECONDS + 1;
        cfg.validate();
        assert_eq!(cfg.seconds, 30);
    }

    #[test]
    fn resolution_fits_without_stretching() {
        // Ultrawide: 1080p e 720p reduzem mantendo a proporção.
        assert_eq!(fit_resolution(2560, 1080, 1080), Some((1920, 810)));
        assert_eq!(fit_resolution(2560, 1080, 720), Some((1280, 540)));
        assert_eq!(fit_resolution(2560, 1080, 1440), None);
        // 16:9 normal.
        assert_eq!(fit_resolution(3840, 2160, 1080), Some((1920, 1080)));
        assert_eq!(fit_resolution(1920, 1080, 720), Some((1280, 720)));
        assert_eq!(fit_resolution(1920, 1080, 1080), None);
        // Nativo.
        assert_eq!(fit_resolution(1920, 1080, 0), None);
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
        assert!(cfg.notify);
        assert!(cfg.admin);
        assert!(!cfg.mic);
        assert!(cfg.monitor.is_empty());
    }
}
