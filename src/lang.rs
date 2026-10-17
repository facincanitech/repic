//! Textos da interface no idioma do Windows: português, espanhol ou inglês
//! (inglês pra qualquer outro idioma). `REPIC_LANG=en|es|pt` força um idioma.

use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Pt,
    En,
    Es,
}

fn from_code(code: &str) -> Lang {
    let code = code.to_ascii_lowercase();
    if code.starts_with("pt") {
        Lang::Pt
    } else if code.starts_with("es") {
        Lang::Es
    } else {
        Lang::En
    }
}

fn detect() -> Lang {
    if let Ok(l) = std::env::var("REPIC_LANG") {
        return from_code(&l);
    }
    #[cfg(windows)]
    {
        // Idioma da interface do Windows; o "primário" fica nos 10 bits de baixo.
        let id = unsafe { windows_sys::Win32::Globalization::GetUserDefaultUILanguage() };
        match id & 0x3FF {
            0x16 => Lang::Pt,
            0x0A => Lang::Es,
            _ => Lang::En,
        }
    }
    #[cfg(not(windows))]
    {
        from_code(&std::env::var("LANG").unwrap_or_default())
    }
}

pub fn lang() -> Lang {
    static LANG: OnceLock<Lang> = OnceLock::new();
    *LANG.get_or_init(detect)
}

pub fn t() -> &'static Texts {
    match lang() {
        Lang::Pt => &PT,
        Lang::En => &EN,
        Lang::Es => &ES,
    }
}

/// Troca cada `{}` do texto pelos valores, em ordem.
pub fn fill(template: &str, args: &[&dyn std::fmt::Display]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut args = args.iter();
    let mut rest = template;
    while let Some(i) = rest.find("{}") {
        out.push_str(&rest[..i]);
        if let Some(a) = args.next() {
            out.push_str(&a.to_string());
        }
        rest = &rest[i + 2..];
    }
    out.push_str(rest);
    out
}

pub struct Texts {
    // Barra de cima e lista
    pub settings: &'static str,
    pub saving_btn: &'static str,
    pub clip_now: &'static str,
    pub recent: &'static str,
    pub open_folder: &'static str,
    pub empty_title: &'static str,
    pub empty_1: &'static str,
    pub empty_2: &'static str,
    pub date_fmt: &'static str,
    // Card do clipe
    pub watch: &'static str,
    pub folder: &'static str,
    pub folder_tip: &'static str,
    pub export: &'static str,
    pub export_tip: &'static str,
    pub delete: &'static str,
    pub delete_tip: &'static str,
    pub exported: &'static str,
    pub export_failed: &'static str,
    pub export_dialog: &'static str,
    pub video_filter: &'static str,
    // Excluir
    pub delete_title: &'static str,
    pub delete_body: &'static str,
    pub delete_failed: &'static str,
    pub cancel: &'static str,
    pub close: &'static str,
    // Configurações
    pub sec_shortcut: &'static str,
    pub key_to_clip: &'static str,
    pub press_key: &'static str,
    pub change: &'static str,
    pub shortcut_hint: &'static str,
    pub key_n: &'static str,
    pub sec_recording: &'static str,
    pub duration: &'static str,
    pub last_secs: &'static str,
    pub quality: &'static str,
    pub resolution: &'static str,
    pub native_fps: &'static str,
    pub native_res: &'static str,
    pub recommended: &'static str,
    pub q_names: [&'static str; 4],
    pub q_hints: [&'static str; 4],
    pub scale_hint: &'static str,
    pub monitor: &'static str,
    pub primary_windows: &'static str,
    pub disconnected: &'static str,
    pub monitor_label: &'static str,
    pub primary_suffix: &'static str,
    pub anywhere: &'static str,
    pub anywhere_on: &'static str,
    pub anywhere_off: &'static str,
    pub sec_audio: &'static str,
    pub pc_audio: &'static str,
    pub mic: &'static str,
    pub mic_hint: &'static str,
    pub sec_system: &'static str,
    pub autostart: &'static str,
    pub autostart_hint: &'static str,
    pub autostart_failed: &'static str,
    pub windows_refused: &'static str,
    pub clips_folder: &'static str,
    pub change_folder: &'static str,
    pub folder_dialog: &'static str,
    // Gravador
    pub looking: &'static str,
    pub recording: &'static str,
    pub record_failed: &'static str,
    pub no_game: &'static str,
    pub saving: &'static str,
    pub saved: &'static str,
    pub save_failed: &'static str,
    pub buffer_empty: &'static str,
    pub output_folder: &'static str,
    pub ffmpeg_missing: &'static str,
    pub ffmpeg_open: &'static str,
    pub ffmpeg_closed: &'static str,
    // Bandeja
    pub tray_tip: &'static str,
    pub tray_open: &'static str,
    pub tray_save: &'static str,
    pub tray_quit: &'static str,
}

pub static PT: Texts = Texts {
    settings: "Configurações",
    saving_btn: "Salvando…",
    clip_now: "Clipar agora",
    recent: "Últimos clipes",
    open_folder: "Abrir pasta",
    empty_title: "Aperte {} e os últimos {} segundos viram um clipe.",
    empty_1: "O RePic grava em loop e só guarda quando você manda. Nada sai do seu PC.",
    empty_2: "Fechar a janela deixa ele na bandeja, perto do relógio.",
    date_fmt: "%d/%m/%Y · %H:%M",
    watch: "Assistir",
    folder: "Pasta",
    folder_tip: "Mostrar na pasta",
    export: "Exportar",
    export_tip: "Salvar uma cópia em outro lugar",
    delete: "Excluir",
    delete_tip: "Apagar o clipe",
    exported: "Exportado pra {}",
    export_failed: "Não exportou: {}",
    export_dialog: "Exportar clipe",
    video_filter: "Vídeo MP4",
    delete_title: "Excluir clipe?",
    delete_body: "{} vai ser apagado do PC. Não dá pra desfazer.",
    delete_failed: "Não apagou (o clipe está aberto em algum programa?): {}",
    cancel: "Cancelar",
    close: "Fechar",
    sec_shortcut: "Atalho",
    key_to_clip: "Tecla pra clipar",
    press_key: "Aperte a tecla… (Esc cancela)",
    change: "Alterar",
    shortcut_hint: "Qualquer tecla, sozinha ou com Ctrl/Alt/Shift. A tecla continua funcionando \
                    normal no jogo; só lembra que apertar ela também clipa.",
    key_n: "Tecla {}",
    sec_recording: "Gravação",
    duration: "Duração do clipe",
    last_secs: "últimos {}s",
    quality: "Qualidade",
    resolution: "Resolução",
    native_fps: "Nativo · {} fps (recomendado)",
    native_res: "Nativa · {}×{} (recomendado)",
    recommended: " (recomendado)",
    q_names: ["Leve", "Normal", "Alta", "Máxima"],
    q_hints: [
        "Arquivos pequenos, bons pra mandar no Discord e no WhatsApp.",
        "Equilíbrio entre qualidade e tamanho do arquivo.",
        "Boa nitidez até em movimento rápido.",
        "O melhor que a placa faz. Arquivos bem maiores.",
    ],
    scale_hint: "Reduzir a resolução usa um pouco do processador (~7% em 720p).",
    monitor: "Monitor",
    primary_windows: "Principal do Windows",
    disconnected: "Desconectado (usando o principal)",
    monitor_label: "Monitor {} · {}×{}",
    primary_suffix: " (principal)",
    anywhere: "Clipar em qualquer lugar",
    anywhere_on: "O atalho funciona sempre: jogo, navegador, área de trabalho…",
    anywhere_off: "O atalho só funciona com um jogo (ou vídeo) em tela cheia na frente.",
    sec_audio: "Áudio",
    pc_audio: "Som do PC (o que sai nos fones/caixas)",
    mic: "Microfone",
    mic_hint: "Usa o microfone padrão do Windows. Troca sozinho se você mudar lá.",
    sec_system: "Sistema",
    autostart: "Iniciar com o Windows",
    autostart_hint: "Abre escondido na bandeja, já gravando.",
    autostart_failed: "Iniciar com o Windows: {}",
    windows_refused: "o Windows recusou (erro {})",
    clips_folder: "Pasta dos clipes",
    change_folder: "Trocar…",
    folder_dialog: "Onde salvar os clipes",
    looking: "Procurando o melhor jeito de gravar…",
    recording: "Gravando em loop · {}{} · {} fps · últimos {}s",
    record_failed: "Não deu pra gravar: {}",
    no_game: "Nenhum jogo em tela cheia. Pra clipar em qualquer lugar, ative nas configurações.",
    saving: "Salvando clipe…",
    saved: "Clipe salvo: {}",
    save_failed: "Não salvou: {}",
    buffer_empty: "o buffer ainda está vazio, espera uns segundos",
    output_folder: "pasta de saída: {}",
    ffmpeg_missing: "ffmpeg.exe não encontrado. Ele precisa ficar na mesma pasta do RePic.exe",
    ffmpeg_open: "não consegui abrir o ffmpeg: {}",
    ffmpeg_closed: "o ffmpeg fechou sozinho",
    tray_tip: "RePic — aperte {} pra clipar",
    tray_open: "Abrir RePic",
    tray_save: "Salvar clipe agora",
    tray_quit: "Sair",
};

pub static EN: Texts = Texts {
    settings: "Settings",
    saving_btn: "Saving…",
    clip_now: "Clip now",
    recent: "Recent clips",
    open_folder: "Open folder",
    empty_title: "Press {} and the last {} seconds become a clip.",
    empty_1: "RePic records in a loop and only keeps what you tell it to. Nothing leaves your PC.",
    empty_2: "Closing the window keeps it running in the tray, next to the clock.",
    date_fmt: "%Y-%m-%d · %H:%M",
    watch: "Watch",
    folder: "Folder",
    folder_tip: "Show in folder",
    export: "Export",
    export_tip: "Save a copy somewhere else",
    delete: "Delete",
    delete_tip: "Delete the clip",
    exported: "Exported to {}",
    export_failed: "Export failed: {}",
    export_dialog: "Export clip",
    video_filter: "MP4 video",
    delete_title: "Delete clip?",
    delete_body: "{} will be deleted from your PC. This can't be undone.",
    delete_failed: "Couldn't delete (is the clip open in another program?): {}",
    cancel: "Cancel",
    close: "Close",
    sec_shortcut: "Shortcut",
    key_to_clip: "Key to clip",
    press_key: "Press a key… (Esc cancels)",
    change: "Change",
    shortcut_hint: "Any key, alone or with Ctrl/Alt/Shift. The key still works normally in \
                    your game; just remember that pressing it also clips.",
    key_n: "Key {}",
    sec_recording: "Recording",
    duration: "Clip length",
    last_secs: "last {}s",
    quality: "Quality",
    resolution: "Resolution",
    native_fps: "Native · {} fps (recommended)",
    native_res: "Native · {}×{} (recommended)",
    recommended: " (recommended)",
    q_names: ["Light", "Normal", "High", "Maximum"],
    q_hints: [
        "Small files, good for sending on Discord or WhatsApp.",
        "Balance between quality and file size.",
        "Sharp even in fast motion.",
        "The best your GPU can do. Much bigger files.",
    ],
    scale_hint: "Lowering the resolution uses a bit of CPU (~7% at 720p).",
    monitor: "Monitor",
    primary_windows: "Windows main display",
    disconnected: "Disconnected (using the main display)",
    monitor_label: "Monitor {} · {}×{}",
    primary_suffix: " (main)",
    anywhere: "Clip anywhere",
    anywhere_on: "The shortcut always works: games, browser, desktop…",
    anywhere_off: "The shortcut only works with a fullscreen game (or video) in front.",
    sec_audio: "Audio",
    pc_audio: "PC sound (what plays on your headphones/speakers)",
    mic: "Microphone",
    mic_hint: "Uses the Windows default microphone, and follows it if you change it there.",
    sec_system: "System",
    autostart: "Start with Windows",
    autostart_hint: "Opens hidden in the tray, already recording.",
    autostart_failed: "Start with Windows: {}",
    windows_refused: "Windows refused (error {})",
    clips_folder: "Clips folder",
    change_folder: "Change…",
    folder_dialog: "Where to save clips",
    looking: "Finding the best way to record…",
    recording: "Recording in a loop · {}{} · {} fps · last {}s",
    record_failed: "Can't record: {}",
    no_game: "No fullscreen game. To clip anywhere, turn it on in Settings.",
    saving: "Saving clip…",
    saved: "Clip saved: {}",
    save_failed: "Couldn't save: {}",
    buffer_empty: "the buffer is still empty, wait a few seconds",
    output_folder: "output folder: {}",
    ffmpeg_missing: "ffmpeg.exe not found. It must be in the same folder as RePic.exe",
    ffmpeg_open: "couldn't start ffmpeg: {}",
    ffmpeg_closed: "ffmpeg closed by itself",
    tray_tip: "RePic — press {} to clip",
    tray_open: "Open RePic",
    tray_save: "Save clip now",
    tray_quit: "Quit",
};

pub static ES: Texts = Texts {
    settings: "Configuración",
    saving_btn: "Guardando…",
    clip_now: "Clipear ahora",
    recent: "Clips recientes",
    open_folder: "Abrir carpeta",
    empty_title: "Pulsa {} y los últimos {} segundos se convierten en un clip.",
    empty_1: "RePic graba en bucle y solo guarda cuando tú lo pides. Nada sale de tu PC.",
    empty_2: "Cerrar la ventana lo deja en la bandeja, junto al reloj.",
    date_fmt: "%d/%m/%Y · %H:%M",
    watch: "Ver",
    folder: "Carpeta",
    folder_tip: "Mostrar en la carpeta",
    export: "Exportar",
    export_tip: "Guardar una copia en otro lugar",
    delete: "Eliminar",
    delete_tip: "Borrar el clip",
    exported: "Exportado a {}",
    export_failed: "No se pudo exportar: {}",
    export_dialog: "Exportar clip",
    video_filter: "Video MP4",
    delete_title: "¿Eliminar clip?",
    delete_body: "{} se borrará del PC. No se puede deshacer.",
    delete_failed: "No se pudo borrar (¿el clip está abierto en otro programa?): {}",
    cancel: "Cancelar",
    close: "Cerrar",
    sec_shortcut: "Atajo",
    key_to_clip: "Tecla para clipear",
    press_key: "Pulsa una tecla… (Esc cancela)",
    change: "Cambiar",
    shortcut_hint: "Cualquier tecla, sola o con Ctrl/Alt/Shift. La tecla sigue funcionando \
                    normal en el juego; solo recuerda que pulsarla también clipea.",
    key_n: "Tecla {}",
    sec_recording: "Grabación",
    duration: "Duración del clip",
    last_secs: "últimos {}s",
    quality: "Calidad",
    resolution: "Resolución",
    native_fps: "Nativo · {} fps (recomendado)",
    native_res: "Nativa · {}×{} (recomendado)",
    recommended: " (recomendado)",
    q_names: ["Ligera", "Normal", "Alta", "Máxima"],
    q_hints: [
        "Archivos pequeños, buenos para mandar por Discord o WhatsApp.",
        "Equilibrio entre calidad y tamaño del archivo.",
        "Buena nitidez incluso en movimiento rápido.",
        "Lo mejor que hace la tarjeta. Archivos mucho más grandes.",
    ],
    scale_hint: "Bajar la resolución usa un poco del procesador (~7% en 720p).",
    monitor: "Monitor",
    primary_windows: "Principal de Windows",
    disconnected: "Desconectado (usando el principal)",
    monitor_label: "Monitor {} · {}×{}",
    primary_suffix: " (principal)",
    anywhere: "Clipear en cualquier lugar",
    anywhere_on: "El atajo funciona siempre: juegos, navegador, escritorio…",
    anywhere_off: "El atajo solo funciona con un juego (o video) en pantalla completa delante.",
    sec_audio: "Audio",
    pc_audio: "Sonido del PC (lo que suena en tus auriculares/altavoces)",
    mic: "Micrófono",
    mic_hint: "Usa el micrófono predeterminado de Windows y lo sigue si lo cambias allí.",
    sec_system: "Sistema",
    autostart: "Iniciar con Windows",
    autostart_hint: "Se abre oculto en la bandeja, ya grabando.",
    autostart_failed: "Iniciar con Windows: {}",
    windows_refused: "Windows lo rechazó (error {})",
    clips_folder: "Carpeta de clips",
    change_folder: "Cambiar…",
    folder_dialog: "Dónde guardar los clips",
    looking: "Buscando la mejor forma de grabar…",
    recording: "Grabando en bucle · {}{} · {} fps · últimos {}s",
    record_failed: "No se puede grabar: {}",
    no_game: "Ningún juego en pantalla completa. Para clipear en cualquier lugar, actívalo en Configuración.",
    saving: "Guardando clip…",
    saved: "Clip guardado: {}",
    save_failed: "No se pudo guardar: {}",
    buffer_empty: "el búfer aún está vacío, espera unos segundos",
    output_folder: "carpeta de salida: {}",
    ffmpeg_missing: "No se encontró ffmpeg.exe. Debe estar en la misma carpeta que RePic.exe",
    ffmpeg_open: "no se pudo abrir ffmpeg: {}",
    ffmpeg_closed: "ffmpeg se cerró solo",
    tray_tip: "RePic — pulsa {} para clipear",
    tray_open: "Abrir RePic",
    tray_save: "Guardar clip ahora",
    tray_quit: "Salir",
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_replaces_in_order() {
        assert_eq!(fill(PT.empty_title, &[&"F8", &30]), "Aperte F8 e os últimos 30 segundos viram um clipe.");
        assert_eq!(fill("a {} b", &[]), "a  b");
    }

    #[test]
    fn language_codes() {
        assert_eq!(from_code("pt-BR"), Lang::Pt);
        assert_eq!(from_code("es_MX.UTF-8"), Lang::Es);
        assert_eq!(from_code("de-DE"), Lang::En);
    }

    #[test]
    fn every_language_has_the_same_placeholders() {
        // Se um idioma tiver {} a mais ou a menos, o texto sai errado.
        let count = |s: &str| s.matches("{}").count();
        for other in [&EN, &ES] {
            for (a, b) in [
                (PT.empty_title, other.empty_title),
                (PT.recording, other.recording),
                (PT.delete_body, other.delete_body),
                (PT.monitor_label, other.monitor_label),
                (PT.last_secs, other.last_secs),
                (PT.saved, other.saved),
                (PT.tray_tip, other.tray_tip),
            ] {
                assert_eq!(count(a), count(b), "{a} / {b}");
            }
        }
    }
}
