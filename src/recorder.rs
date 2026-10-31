//! Gravação contínua em loop + salvar clipe.
//!
//! Como funciona:
//! - Um ffmpeg fica gravando a tela em pedaços de 2s (`seg_000.ts`, `seg_001.ts`, …).
//!   Com `-segment_wrap` ele reaproveita os nomes, então o disco nunca enche.
//! - Usamos o encoder da placa de vídeo (NVENC/AMF/QSV) pra quase não pesar no jogo.
//!   Se nenhum funcionar, cai pro x264 na CPU.
//! - No atalho, juntamos os últimos pedaços prontos sem reprocessar (`-c copy`),
//!   colamos o áudio (PC + microfone) da mesma janela de tempo e pronto.

use crate::config::{Config, Quality, fit_resolution, format_duration};
use crate::lang::{fill, t};
use crate::platform::{self, Monitor, Toast, hidden_command};
use crate::state::{Health, RecCmd, Shared};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

const SEGMENT_SECS: u32 = 2;

/// Quanto tempo depois da captura o quadro chega no arquivo (codificar +
/// gravar). O fim do vídeo vem da hora do arquivo, então o áudio é recuado
/// esse tanto pra ficar junto da imagem. Medido com pisca + bipe: sem isso
/// o som ficava ~140 ms adiantado.
const VIDEO_LATENCY: Duration = Duration::from_millis(140);

const NVIDIA: u32 = 0x10de;
const AMD: u32 = 0x1002;
const INTEL: u32 = 0x8086;

/// Como gravar: FPS, tamanho e qualidade já resolvidos pro monitor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Capture {
    pub fps: u32,
    /// Reduzir pra (largura, altura). None = resolução nativa.
    pub scale: Option<(u32, u32)>,
    pub quality: Quality,
}

impl Capture {
    /// "Nativo" vira a taxa e a resolução do monitor; uma escolha acima do
    /// que o monitor faz cai pro nativo (gravar mais só repetiria quadros).
    pub fn resolve(cfg: &Config, mon: Option<&Monitor>) -> Self {
        let native_fps = mon.map(|m| m.refresh).unwrap_or(60);
        let fps = match cfg.frame_rate {
            0 => native_fps,
            f => f.min(native_fps),
        };
        let scale = mon.and_then(|m| fit_resolution(m.width, m.height, cfg.resolution));
        Self {
            fps,
            scale,
            quality: cfg.quality,
        }
    }

    /// Escolhe o valor do nível de qualidade (Leve, Normal, Alta, Máxima).
    fn q(&self, levels: [u32; 4]) -> String {
        levels[self.quality.index()].to_string()
    }
}

struct Profile {
    name: &'static str,
    /// Fabricante da placa que esse encoder usa (None = CPU).
    vendor: Option<u32>,
    args: fn(c: &Capture, mon: Option<&Monitor>) -> Vec<String>,
}

/// Fonte de vídeo via Desktop Duplication. Com o monitor conhecido, cria o
/// dispositivo D3D11 na placa onde ele está ligado; sem isso o ddagrab só
/// enxerga os monitores da primeira placa (em notebook com duas placas,
/// a tela do notebook costuma ficar de fora).
///
/// Reduzir a resolução copia o quadro pra memória e redimensiona na CPU
/// (~7% do processador em 720p): o `scale_d3d11` do FFmpeg ainda não aceita
/// as texturas do ddagrab.
fn dda(c: &Capture, download: bool, mon: Option<&Monitor>) -> Vec<String> {
    let fps = c.fps;
    let mut chain = String::new();
    if download || c.scale.is_some() {
        chain.push_str(",hwdownload,format=bgra");
    }
    if let Some((w, h)) = c.scale {
        chain.push_str(&format!(",scale={w}:{h}:flags=bilinear"));
    }
    match mon {
        Some(m) => {
            let device = if m.unique_vendor {
                format!("d3d11va=rp:,vendor_id=0x{:04x}", m.vendor_id)
            } else {
                format!("d3d11va=rp:{}", m.adapter)
            };
            vec![
                "-init_hw_device".into(),
                device,
                "-filter_hw_device".into(),
                "rp".into(),
                "-filter_complex".into(),
                format!(
                    "ddagrab=output_idx={}:framerate={fps}:draw_mouse=1{chain}[v]",
                    m.output
                ),
                "-map".into(),
                "[v]".into(),
            ]
        }
        None => vec![
            "-f".into(),
            "lavfi".into(),
            "-i".into(),
            format!("ddagrab=output_idx=0:framerate={fps}:draw_mouse=1{chain}"),
        ],
    }
}

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

/// Ordem padrão: placa de vídeo primeiro, CPU por último. Na hora de gravar,
/// o encoder da placa onde o monitor está ligado vai pra frente da fila.
///
/// Níveis de qualidade em cada encoder (Leve, Normal, Alta, Máxima): número
/// menor = mais qualidade e arquivo maior. Todos em qualidade constante, sem
/// limite de bitrate.
const PROFILES: &[Profile] = &[
    Profile {
        name: "NVIDIA (NVENC)",
        vendor: Some(NVIDIA),
        args: |c, mon| {
            let mut a = dda(c, false, mon);
            a.extend(s(&["-c:v", "h264_nvenc", "-preset", "p4", "-rc", "vbr", "-cq"]));
            a.push(c.q([28, 23, 20, 16]));
            a.extend(s(&["-b:v", "0"]));
            a
        },
    },
    Profile {
        name: "AMD (AMF)",
        vendor: Some(AMD),
        args: |c, mon| {
            let mut a = dda(c, false, mon);
            a.extend(s(&["-c:v", "h264_amf", "-quality", "speed", "-rc", "cqp", "-qp_i"]));
            a.push(c.q([28, 22, 19, 16]));
            a.push("-qp_p".into());
            a.push(c.q([30, 24, 21, 18]));
            a
        },
    },
    Profile {
        name: "Intel (QuickSync)",
        vendor: Some(INTEL),
        args: |c, mon| {
            let mut a = dda(c, true, mon);
            a.extend(s(&["-c:v", "h264_qsv", "-preset", "veryfast", "-global_quality"]));
            a.push(c.q([30, 24, 21, 18]));
            a.extend(s(&["-pix_fmt", "nv12"]));
            a
        },
    },
    Profile {
        name: "CPU (x264)",
        vendor: None,
        args: |c, mon| {
            let mut a = dda(c, true, mon);
            a.extend(s(&["-c:v", "libx264", "-preset", "ultrafast", "-crf"]));
            a.push(c.q([28, 23, 20, 17]));
            a.extend(s(&["-pix_fmt", "yuv420p"]));
            a
        },
    },
    Profile {
        name: "CPU (x264, GDI)",
        vendor: None,
        args: |c, mon| {
            let fps = c.fps.min(30).to_string();
            let mut a = s(&["-f", "gdigrab", "-framerate", &fps]);
            if let Some(m) = mon {
                a.extend([
                    "-offset_x".into(),
                    m.x.to_string(),
                    "-offset_y".into(),
                    m.y.to_string(),
                    "-video_size".into(),
                    format!("{}x{}", m.width, m.height),
                ]);
            }
            a.extend(s(&["-i", "desktop"]));
            if let Some((w, h)) = c.scale {
                a.extend(["-vf".into(), format!("scale={w}:{h}")]);
            }
            a.extend(s(&["-c:v", "libx264", "-preset", "ultrafast", "-crf"]));
            a.push(c.q([28, 23, 20, 17]));
            a.extend(s(&["-pix_fmt", "yuv420p"]));
            a
        },
    },
];

/// Índices de PROFILES na ordem de tentativa pra esse monitor.
fn profile_order(mon: Option<&Monitor>) -> Vec<usize> {
    let mut order: Vec<usize> = (0..PROFILES.len()).collect();
    if let Some(m) = mon {
        // sort estável: só puxa pra frente o encoder da placa do monitor.
        order.sort_by_key(|&i| PROFILES[i].vendor != Some(m.vendor_id));
        order.sort_by_key(|&i| PROFILES[i].vendor.is_none());
    }
    order
}

struct Running {
    child: Child,
    stderr: Arc<Mutex<VecDeque<String>>>,
}

impl Running {
    fn kill(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
    fn last_error(&self) -> String {
        let lines = self.stderr.lock().unwrap();
        lines
            .iter()
            .rev()
            .take(2)
            .cloned()
            .collect::<Vec<_>>()
            .join(" | ")
    }
}

fn clean_dir(dir: &Path) {
    let _ = std::fs::create_dir_all(dir);
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

fn spawn_profile(
    shared: &Shared,
    profile: &Profile,
    mon: Option<&Monitor>,
) -> Result<Running, String> {
    let cfg = shared.config();
    let capture = Capture::resolve(&cfg, mon);
    let dir = &shared.paths.buffer_dir;
    clean_dir(dir);
    let wrap = cfg.seconds / SEGMENT_SECS + 6;
    let gop = (capture.fps * SEGMENT_SECS).to_string();

    let mut cmd = hidden_command(&shared.paths.ffmpeg);
    cmd.args(["-hide_banner", "-loglevel", "error", "-y"]);
    cmd.args((profile.args)(&capture, mon));
    cmd.args(["-g", &gop, "-an"]);
    cmd.args([
        "-f",
        "segment",
        "-segment_time",
        &SEGMENT_SECS.to_string(),
        "-segment_wrap",
        &wrap.to_string(),
        "-segment_format",
        "mpegts",
        "-reset_timestamps",
        "1",
    ]);
    cmd.arg(dir.join("seg_%03d.ts"));
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            t().ffmpeg_missing.to_string()
        } else {
            fill(t().ffmpeg_open, &[&e])
        }
    })?;
    platform::tie_to_app(&child);

    let stderr = Arc::new(Mutex::new(VecDeque::new()));
    if let Some(pipe) = child.stderr.take() {
        let buf = stderr.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                let mut b = buf.lock().unwrap();
                b.push_back(line);
                if b.len() > 30 {
                    b.pop_front();
                }
            }
        });
    }
    let mut running = Running { child, stderr };

    // Dá um tempinho pra ver se o encoder aceita a configuração.
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(3500) {
        if let Ok(Some(_)) = running.child.try_wait() {
            std::thread::sleep(Duration::from_millis(100));
            let err = running.last_error();
            return Err(if err.is_empty() {
                t().ffmpeg_closed.into()
            } else {
                err
            });
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(running)
}

/// Supervisor: mantém o ffmpeg vivo, troca de encoder se precisar,
/// reinicia quando a configuração muda.
pub fn start(shared: Arc<Shared>) {
    let (tx, rx) = mpsc::channel();
    let _ = shared.recorder.set(tx);

    std::thread::Builder::new()
        .name("recorder".into())
        .spawn(move || {
            let mut current: Option<Running> = None;
            let mut monitor: Option<Monitor> = None;
            let mut order: Vec<usize> = Vec::new();
            let mut pos = 0usize; // posição em `order`
            let mut crashes = 0u32;

            loop {
                if current.is_none() {
                    shared.set_status(Health::Starting, t().looking);
                    if order.is_empty() {
                        let cfg = shared.config();
                        monitor = platform::pick_monitor(&platform::list_monitors(), &cfg.monitor);
                        order = profile_order(monitor.as_ref());
                        pos = 0;
                    }
                    let mut last_err = String::new();
                    for (p, &i) in order.iter().enumerate().skip(pos) {
                        match spawn_profile(&shared, &PROFILES[i], monitor.as_ref()) {
                            Ok(r) => {
                                pos = p;
                                current = Some(r);
                                break;
                            }
                            Err(e) => {
                                let missing = e == t().ffmpeg_missing;
                                last_err = e;
                                if missing {
                                    break;
                                }
                            }
                        }
                    }
                    match &current {
                        Some(_) => {
                            let cfg = shared.config();
                            let tela = monitor
                                .as_ref()
                                .map(|m| format!("{} · ", m.label))
                                .unwrap_or_default();
                            let capture = Capture::resolve(&cfg, monitor.as_ref());
                            let mut encoder = PROFILES[order[pos]].name.to_string();
                            if let Some((w, h)) = capture.scale {
                                encoder = format!("{encoder} · {w}×{h}");
                            }
                            shared.set_status(
                                Health::Recording,
                                fill(
                                    t().recording,
                                    &[&tela, &encoder, &capture.fps, &format_duration(cfg.seconds)],
                                ),
                            );
                        }
                        None => {
                            shared.set_status(Health::Error, fill(t().record_failed, &[&last_err]));
                            // Na próxima tentativa, procura o monitor de novo.
                            order.clear();
                        }
                    }
                }

                let wait = if current.is_some() { 500 } else { 10_000 };
                match rx.recv_timeout(Duration::from_millis(wait)) {
                    Ok(RecCmd::Quit) | Err(RecvTimeoutError::Disconnected) => {
                        if let Some(r) = current.take() {
                            r.kill();
                        }
                        clean_dir(&shared.paths.buffer_dir);
                        break;
                    }
                    Ok(RecCmd::Restart) => {
                        finish_recording(&shared);
                        if let Some(r) = current.take() {
                            r.kill();
                        }
                        crashes = 0;
                        order.clear(); // monitor pode ter mudado
                        shared.audio.clear();
                        shared.mic.clear();
                    }
                    Err(RecvTimeoutError::Timeout) => {
                        let died = match current.as_mut() {
                            Some(r) => matches!(r.child.try_wait(), Ok(Some(_))),
                            None => false,
                        };
                        if died {
                            // Depois de cair não dá pra emendar: salva o que tem.
                            finish_recording(&shared);
                            let r = current.take().unwrap();
                            eprintln!("ffmpeg caiu: {}", r.last_error());
                            crashes += 1;
                            // Caiu várias vezes seguidas? Tenta o próximo encoder.
                            if crashes >= 3 && pos + 1 < order.len() {
                                pos += 1;
                                crashes = 0;
                            }
                            std::thread::sleep(Duration::from_secs(1));
                        }
                    }
                }
            }
        })
        .expect("thread do gravador");
}

// ---------------------------------------------------------------------------
// Salvar clipe

/// Chamado pelo atalho global. Com "clipar em qualquer lugar" desligado,
/// só salva se tiver um jogo (ou outro app) em tela cheia em foco.
pub fn hotkey_pressed(shared: Arc<Shared>) {
    if !shared.config().anywhere && !platform::foreground_is_fullscreen() {
        notify(&shared, t().toast_no_game, Toast::Error);
        shared.set_event(false, t().no_game);
        return;
    }
    trigger_save(shared);
}

/// Aviso no canto da tela. Com o aviso desligado (ou se ele não abrir),
/// avisa só com o som.
pub fn notify(shared: &Shared, text: &str, kind: Toast) {
    if !(shared.config().notify && platform::toast(text, kind)) {
        match kind {
            Toast::Busy => {}
            Toast::Ok => platform::beep_ok(),
            Toast::Error => platform::beep_error(),
        }
    }
}

/// Salva o clipe numa thread pra não travar nada.
pub fn trigger_save(shared: Arc<Shared>) {
    if shared.saving.swap(true, Ordering::SeqCst) {
        platform::log("clipe: já está salvando outro, ignorado");
        return;
    }
    let pressed = SystemTime::now();
    std::thread::spawn(move || {
        platform::log("clipe: salvando");
        notify(&shared, t().toast_clipping, Toast::Busy);
        shared.set_event(true, t().saving);
        shared.audio.set_hold(true);
        shared.mic.set_hold(true);
        let saved = save_clip(&shared, pressed);
        shared.audio.set_hold(false);
        shared.mic.set_hold(false);
        match saved {
            Ok(path) => {
                let name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                platform::log(&format!("clipe: salvo {name}"));
                notify(&shared, t().toast_saved, Toast::Ok);
                shared.set_event(true, fill(t().saved, &[&name]));
                shared.history_dirty.store(true, Ordering::SeqCst);
            }
            Err(e) => {
                platform::log(&format!("clipe: falhou: {e}"));
                let msg = fill(t().save_failed, &[&e]);
                notify(&shared, &msg, Toast::Error);
                shared.set_event(false, msg);
            }
        }
        shared.saving.store(false, Ordering::SeqCst);
        shared.repaint();
    });
}

fn completed_segments(dir: &Path) -> Vec<(PathBuf, SystemTime)> {
    let mut segs: Vec<(PathBuf, SystemTime)> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().extension().is_some_and(|x| x == "ts"))
                .filter_map(|e| {
                    let m = e.metadata().ok()?;
                    if m.len() > 0 {
                        Some((e.path(), m.modified().ok()?))
                    } else {
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    segs.sort_by_key(|(_, t)| *t);
    segs.pop(); // o mais novo ainda está sendo escrito
    segs
}

fn run_ffmpeg(shared: &Shared, args: &[&str]) -> Result<(), String> {
    let out = hidden_command(&shared.paths.ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("ffmpeg: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(err.lines().last().unwrap_or("ffmpeg falhou").to_string())
    }
}

/// Duração do arquivo, lida do `Duration: 00:00:30.02` que o ffmpeg imprime
/// ao abrir um arquivo (assim não precisa do ffprobe junto).
fn probe_duration(shared: &Shared, file: &Path) -> Option<f64> {
    let out = hidden_command(&shared.paths.ffmpeg)
        .args(["-hide_banner", "-i"])
        .arg(file)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    parse_duration(&String::from_utf8_lossy(&out.stderr))
}

fn parse_duration(ffmpeg_output: &str) -> Option<f64> {
    let rest = ffmpeg_output.split("Duration: ").nth(1)?;
    let hms = rest.split(',').next()?.trim();
    let mut parts = hms.split(':');
    let h: f64 = parts.next()?.parse().ok()?;
    let m: f64 = parts.next()?.parse().ok()?;
    let s: f64 = parts.next()?.parse().ok()?;
    Some(h * 3600.0 + m * 60.0 + s)
}

fn p(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

/// `RePic_2026-10-16_21-30-05.mp4`, ou `…_2.mp4` se já existir um nesse segundo.
fn unique_clip_path(dir: &Path, stamp: &str) -> PathBuf {
    let first = dir.join(format!("RePic_{stamp}.mp4"));
    if !first.exists() {
        return first;
    }
    (2..)
        .map(|n| dir.join(format!("RePic_{stamp}_{n}.mp4")))
        .find(|p| !p.exists())
        .unwrap()
}

/// Faixa de som bruto. Bruto porque WAV para em 4 GB, e som em 8 canais passa
/// disso num clipe de 2 horas.
struct AudioTrack {
    path: String,
    sample_rate: u32,
    channels: u16,
}

/// Quanto o vídeo gasta no disco por segundo, medido nos pedaços do buffer.
/// Nunca menos que `MIN_BYTES_PER_SEC`: tela parada gasta pouco, mas um jogo
/// logo em seguida gasta bem mais.
pub fn video_bytes_per_sec(shared: &Shared) -> f64 {
    /// ~14 Mbps, o maior medido em clipes reais na qualidade Alta.
    const MIN_BYTES_PER_SEC: f64 = 1_750_000.0;
    let segs = completed_segments(&shared.paths.buffer_dir);
    let total: u64 = segs
        .iter()
        .filter_map(|(f, _)| std::fs::metadata(f).ok())
        .map(|m| m.len())
        .sum();
    let secs = (segs.len() as u32 * SEGMENT_SECS).max(1) as f64;
    (total as f64 / secs).max(MIN_BYTES_PER_SEC)
}

/// Argumentos do ffmpeg pra juntar vídeo + faixas de áudio num MP4.
fn mux_args(video: &str, tracks: &[AudioTrack], out: &str) -> Vec<String> {
    let mut a = vec!["-i".to_string(), video.to_string()];
    for t in tracks {
        a.extend(s(&["-f", "f32le", "-ar"]));
        a.push(t.sample_rate.to_string());
        a.push("-ac".into());
        a.push(t.channels.to_string());
        a.extend(["-i".to_string(), t.path.clone()]);
    }
    match tracks.len() {
        0 => a.extend(s(&["-c", "copy"])),
        n => {
            a.extend(s(&["-map", "0:v"]));
            if n == 1 {
                a.extend(s(&["-map", "1:a"]));
            } else {
                // Som do PC + microfone na mesma faixa, sem abaixar o volume.
                let inputs: String = (1..=n).map(|i| format!("[{i}:a]")).collect();
                a.extend([
                    "-filter_complex".into(),
                    format!("{inputs}amix=inputs={n}:duration=first:normalize=0[a]"),
                    "-map".into(),
                    "[a]".into(),
                ]);
            }
            a.extend(s(&["-c:v", "copy", "-c:a", "aac", "-ac", "2", "-b:a", "192k", "-shortest"]));
        }
    }
    a.extend(s(&["-movflags", "+faststart", out]));
    a
}

/// Quantos pedaços pegar pra cobrir `seconds` até o aperto, sabendo que o
/// último pedaço termina `after_press` depois do aperto.
fn segments_needed(seconds: u32, after_press: Duration) -> usize {
    ((seconds as f64 + after_press.as_secs_f64()) / SEGMENT_SECS as f64).ceil() as usize
}

fn save_clip(shared: &Shared, pressed: SystemTime) -> Result<PathBuf, String> {
    let cfg = shared.config();
    // O pedaço que estava sendo gravado no aperto é justamente o momento que
    // a pessoa quer: espera ele fechar (no máximo um pedaço e pouco).
    let deadline = Instant::now() + Duration::from_millis(SEGMENT_SECS as u64 * 1000 + 1500);
    let segs = loop {
        let segs = completed_segments(&shared.paths.buffer_dir);
        let covers_press = segs.last().is_some_and(|(_, end)| *end >= pressed);
        if covers_press || Instant::now() >= deadline {
            break segs;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    if segs.is_empty() {
        return Err(t().buffer_empty.into());
    }
    let video_end = segs.last().unwrap().1;
    let after_press = video_end.duration_since(pressed).unwrap_or_default();
    let wanted = segments_needed(cfg.seconds, after_press);
    let take: Vec<PathBuf> = segs[segs.len().saturating_sub(wanted)..]
        .iter()
        .map(|(f, _)| f.clone())
        .collect();
    assemble(shared, &take, video_end)
}

// ---------------------------------------------------------------------------
// Gravar daqui pra frente (o botão de gravar, tipo OBS)
//
// Não abre outro ffmpeg: guarda uma cópia dos mesmos pedaços de 2s enquanto
// estiver gravando. Assim não pesa mais na placa, e clipar o passado continua
// funcionando junto. Como corta nos pedaços, pode começar e terminar até 2s a mais.

/// Começa a gravar ou, se já está gravando, pede pra parar e salvar.
pub fn toggle_recording(shared: Arc<Shared>) {
    if shared.recording.load(Ordering::SeqCst) {
        if !shared.rec_stop.swap(true, Ordering::SeqCst) {
            platform::log("gravação: parando");
            notify(&shared, t().toast_rec_saving, Toast::Busy);
            shared.set_event(true, t().toast_rec_saving);
        }
        shared.repaint();
        return;
    }
    if shared.recording.swap(true, Ordering::SeqCst) {
        return;
    }
    shared.rec_stop.store(false, Ordering::SeqCst);
    *shared.rec_started.lock().unwrap() = Some(Instant::now());
    // O som guardado não pode vencer enquanto a gravação durar.
    shared.audio.set_hold(true);
    shared.mic.set_hold(true);
    let started = SystemTime::now();
    platform::log("gravação: começou");
    notify(&shared, t().toast_rec_started, Toast::Ok);
    shared.repaint();
    std::thread::spawn(move || {
        let result = record(&shared, started);
        shared.audio.set_hold(false);
        shared.mic.set_hold(false);
        match result {
            Ok(path) => {
                let name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                platform::log(&format!("gravação: salva {name}"));
                notify(&shared, t().toast_rec_saved, Toast::Ok);
                shared.set_event(true, fill(t().saved, &[&name]));
                shared.history_dirty.store(true, Ordering::SeqCst);
            }
            Err(e) => {
                platform::log(&format!("gravação: falhou: {e}"));
                let msg = fill(t().save_failed, &[&e]);
                notify(&shared, &msg, Toast::Error);
                shared.set_event(false, msg);
            }
        }
        *shared.rec_started.lock().unwrap() = None;
        shared.rec_stop.store(false, Ordering::SeqCst);
        shared.recording.store(false, Ordering::SeqCst);
        shared.repaint();
    });
}

/// Termina a gravação em andamento e espera salvar. A gravação em loop vai ser
/// reiniciada ou fechada, e os pedaços novos não poderiam se misturar com ela.
pub fn finish_recording(shared: &Shared) {
    if !shared.recording.load(Ordering::SeqCst) {
        return;
    }
    shared.rec_stop.store(true, Ordering::SeqCst);
    while shared.recording.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Pedaços copiados até agora.
#[derive(Default)]
struct Recorded {
    files: Vec<PathBuf>,
    last_end: Option<SystemTime>,
    bytes: u64,
}

/// Copia os pedaços que fecharam depois do começo da gravação. O buffer vai
/// reaproveitar esses arquivos, então precisa ser cópia de verdade.
fn collect_segments(shared: &Shared, started: SystemTime, rec: &mut Recorded) -> Result<(), String> {
    for (seg, end) in completed_segments(&shared.paths.buffer_dir) {
        if end < started || rec.last_end.is_some_and(|last| end <= last) {
            continue;
        }
        let copy = shared
            .paths
            .record_dir
            .join(format!("rec_{:06}.ts", rec.files.len()));
        rec.bytes += std::fs::copy(&seg, &copy).map_err(|e| e.to_string())?;
        rec.files.push(copy);
        rec.last_end = Some(end);
    }
    Ok(())
}

/// Disco quase cheio? Precisa sobrar pro que falta: montar o vídeo e o som.
fn disk_nearly_full(free: u64, recorded: u64) -> bool {
    const RESERVE: u64 = 2_000_000_000;
    free < recorded.saturating_mul(2).saturating_add(RESERVE)
}

fn record(shared: &Shared, started: SystemTime) -> Result<PathBuf, String> {
    let dir = &shared.paths.record_dir;
    clean_dir(dir);
    let mut rec = Recorded::default();
    let mut last_disk_check = Instant::now();
    loop {
        if let Err(e) = collect_segments(shared, started, &mut rec) {
            // Disco cheio, por exemplo: salva o que já tem.
            platform::log(&format!("gravação: não consegui copiar um pedaço: {e}"));
            break;
        }
        if shared.rec_stop.load(Ordering::SeqCst) {
            break;
        }
        if last_disk_check.elapsed() >= Duration::from_secs(5) {
            last_disk_check = Instant::now();
            if platform::free_disk(dir).is_some_and(|free| disk_nearly_full(free, rec.bytes)) {
                platform::log("gravação: disco quase cheio, parando");
                shared.set_event(false, t().rec_disk_full);
                notify(shared, t().rec_disk_full, Toast::Busy);
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(300));
    }

    // O pedaço em andamento faz parte da gravação: espera ele fechar.
    let stopped = SystemTime::now();
    let deadline = Instant::now() + Duration::from_millis(SEGMENT_SECS as u64 * 1000 + 1500);
    while rec.last_end.is_none_or(|end| end < stopped) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
        if collect_segments(shared, started, &mut rec).is_err() {
            break;
        }
    }
    let Some(video_end) = rec.last_end else {
        return Err(t().buffer_empty.into());
    };

    // A pasta de trabalho é uma só: espera um clipe que esteja salvando.
    while shared.saving.swap(true, Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(100));
    }
    let result = assemble(shared, &rec.files, video_end);
    shared.saving.store(false, Ordering::SeqCst);
    clean_dir(dir);
    result
}

/// Junta os pedaços com o som da mesma janela de tempo num MP4 na pasta de clipes.
fn assemble(shared: &Shared, take: &[PathBuf], video_end: SystemTime) -> Result<PathBuf, String> {
    let cfg = shared.config();
    let work = &shared.paths.work_dir;
    clean_dir(work);

    // 1) Junta os pedaços sem reprocessar.
    let list = work.join("list.txt");
    let body: String = take
        .iter()
        .map(|f| {
            format!(
                "file '{}'\n",
                p(f).replace('\\', "/").replace('\'', "'\\''")
            )
        })
        .collect();
    std::fs::write(&list, body).map_err(|e| e.to_string())?;
    let video = work.join("video.mp4");
    run_ffmpeg(
        shared,
        &[
            "-f",
            "concat",
            "-safe",
            "0",
            "-i",
            &p(&list),
            "-c",
            "copy",
            &p(&video),
        ],
    )?;

    std::fs::create_dir_all(&cfg.output_dir).map_err(|e| fill(t().output_folder, &[&e]))?;
    let stamp = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S").to_string();
    let out = unique_clip_path(&cfg.output_dir, &stamp);

    // 2) Áudio da mesma janela de tempo do vídeo.
    let duration =
        probe_duration(shared, &video).unwrap_or((take.len() as u32 * SEGMENT_SECS) as f64);
    let duration = Duration::from_secs_f64(duration.max(0.1));
    let mut tracks = Vec::new();
    if let Some(start) = video_end.checked_sub(duration + VIDEO_LATENCY) {
        for (on, ring, name) in [
            (cfg.audio, &shared.audio, "pc.raw"),
            (cfg.mic, &shared.mic, "mic.raw"),
        ] {
            if on {
                let path = work.join(name);
                let rendered = ring.render_to_file(start, duration, &path)?;
                // Distingue "clipe mudo" de "som baixo" sem precisar do arquivo.
                platform::log(&format!("clipe: áudio {name}, pico {:.4}", rendered.peak));
                tracks.push(AudioTrack {
                    path: p(&path),
                    sample_rate: rendered.sample_rate,
                    channels: rendered.channels,
                });
            }
        }
    }

    // 3) Arquivo final.
    let args = mux_args(&p(&video), &tracks, &p(&out));
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    run_ffmpeg(shared, &args)?;

    // 4) Miniatura pro histórico (se falhar, tudo bem).
    let _ = std::fs::create_dir_all(&shared.paths.thumbs_dir);
    let thumb = thumb_path(shared, &out);
    let mid = format!("{:.2}", duration.as_secs_f64() / 2.0);
    let _ = run_ffmpeg(
        shared,
        &[
            "-ss",
            &mid,
            "-i",
            &p(&out),
            "-frames:v",
            "1",
            "-vf",
            "scale=480:-2",
            "-q:v",
            "4",
            &p(&thumb),
        ],
    );

    clean_dir(work);
    Ok(out)
}

pub fn thumb_path(shared: &Shared, clip: &Path) -> PathBuf {
    let stem = clip.file_stem().unwrap_or_default().to_string_lossy();
    shared.paths.thumbs_dir.join(format!("{stem}.jpg"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mon(vendor_id: u32) -> Monitor {
        Monitor {
            id: "10de:0".into(),
            label: String::new(),
            primary: true,
            adapter: 1,
            vendor_id,
            unique_vendor: true,
            output: 0,
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
            refresh: 144,
        }
    }

    fn names(order: &[usize]) -> Vec<&'static str> {
        order.iter().map(|&i| PROFILES[i].name).collect()
    }

    #[test]
    fn encoder_of_the_monitors_gpu_goes_first_and_cpu_last() {
        let o = names(&profile_order(Some(&mon(INTEL))));
        assert_eq!(o[0], "Intel (QuickSync)");
        assert!(o[3].starts_with("CPU") && o[4].starts_with("CPU"));
        let o = names(&profile_order(Some(&mon(NVIDIA))));
        assert_eq!(o[0], "NVIDIA (NVENC)");
        let o = names(&profile_order(None));
        assert_eq!(o[0], "NVIDIA (NVENC)");
    }

    fn cap(fps: u32, scale: Option<(u32, u32)>) -> Capture {
        Capture {
            fps,
            scale,
            quality: Quality::High,
        }
    }

    #[test]
    fn dda_picks_the_gpu_by_vendor() {
        let a = dda(&cap(60, None), false, Some(&mon(INTEL)));
        assert!(a.contains(&"d3d11va=rp:,vendor_id=0x8086".to_string()));
        assert!(a.contains(&"ddagrab=output_idx=0:framerate=60:draw_mouse=1[v]".to_string()));
        let mut m = mon(NVIDIA);
        m.unique_vendor = false;
        assert!(dda(&cap(30, None), true, Some(&m)).contains(&"d3d11va=rp:1".to_string()));
    }

    #[test]
    fn reducing_resolution_scales_after_download() {
        let a = dda(&cap(60, Some((1280, 720))), false, Some(&mon(NVIDIA)));
        assert!(a.contains(
            &"ddagrab=output_idx=0:framerate=60:draw_mouse=1,hwdownload,format=bgra,scale=1280:720:flags=bilinear[v]"
                .to_string()
        ));
    }

    #[test]
    fn native_follows_the_monitor_and_never_goes_above_it() {
        let m = Monitor {
            width: 2560,
            height: 1080,
            refresh: 74,
            ..mon(NVIDIA)
        };
        let mut cfg = Config::default();
        // Padrão: tudo nativo.
        assert_eq!(Capture::resolve(&cfg, Some(&m)), cap(74, None));
        // Pediu 144 num monitor de 74 Hz: fica em 74.
        cfg.frame_rate = 144;
        assert_eq!(Capture::resolve(&cfg, Some(&m)).fps, 74);
        cfg.frame_rate = 30;
        assert_eq!(Capture::resolve(&cfg, Some(&m)).fps, 30);
        // 720p e 1080p num ultrawide mantêm a proporção, com medidas pares.
        cfg.resolution = 720;
        assert_eq!(Capture::resolve(&cfg, Some(&m)).scale, Some((1280, 540)));
        cfg.resolution = 1080;
        assert_eq!(Capture::resolve(&cfg, Some(&m)).scale, Some((1920, 810)));
        // "Reduzir" pra 1440p num monitor de 1080: fica nativo.
        cfg.resolution = 1440;
        assert_eq!(Capture::resolve(&cfg, Some(&m)).scale, None);
    }

    #[test]
    fn reads_duration_from_ffmpeg_output() {
        let out = "Input #0, mov,mp4,m4a,3gp,3g2,mj2, from 'video.mp4':\n  Duration: 00:01:02.50, start: 0.000000, bitrate: 9000 kb/s\n";
        assert_eq!(parse_duration(out), Some(62.5));
        assert_eq!(parse_duration("Duration: N/A, bitrate: N/A"), None);
        assert_eq!(parse_duration("nada aqui"), None);
    }

    #[test]
    fn quality_levels_reach_the_encoder() {
        let mut c = cap(60, None);
        c.quality = Quality::Light;
        assert!((PROFILES[0].args)(&c, None).contains(&"28".to_string()));
        c.quality = Quality::Max;
        assert!((PROFILES[0].args)(&c, None).contains(&"16".to_string()));
    }

    #[test]
    fn mux_mixes_pc_and_mic() {
        let track = |path: &str| AudioTrack {
            path: path.into(),
            sample_rate: 48_000,
            channels: 2,
        };
        let a = mux_args("v.mp4", &[track("pc.raw"), track("mic.raw")], "o.mp4");
        assert!(a.contains(&"[1:a][2:a]amix=inputs=2:duration=first:normalize=0[a]".to_string()));
        assert!(a.contains(&"f32le".to_string()) && a.contains(&"48000".to_string()));
        let a = mux_args("v.mp4", &[track("pc.raw")], "o.mp4");
        assert!(a.contains(&"1:a".to_string()) && !a.iter().any(|x| x.contains("amix")));
        let a = mux_args("v.mp4", &[], "o.mp4");
        assert!(!a.contains(&"-c:a".to_string()));
    }

    #[test]
    fn takes_enough_segments_to_cover_the_seconds_before_the_press() {
        // Último pedaço fechou junto com o aperto: 30s = 15 pedaços.
        assert_eq!(segments_needed(30, Duration::ZERO), 15);
        // Fechou 1,4s depois do aperto: precisa de mais um pra não perder o começo.
        assert_eq!(segments_needed(30, Duration::from_millis(1400)), 16);
        assert_eq!(segments_needed(15, Duration::from_millis(500)), 8);
    }

    #[test]
    fn stops_recording_before_filling_the_disk() {
        const GB: u64 = 1_000_000_000;
        assert!(!disk_nearly_full(20 * GB, 5 * GB));
        // 5 GB gravados precisam de mais 10 GB pra montar, fora a folga.
        assert!(disk_nearly_full(11 * GB, 5 * GB));
        assert!(disk_nearly_full(GB, 0));
    }

    /// Passa pelo caminho real, com ffmpeg e arquivos de verdade, sem capturar
    /// a tela nem o teclado: `cargo test --release live_record -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_record() {
        let root = std::env::temp_dir().join(format!("repic-rec-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let ffmpeg = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/release/ffmpeg.exe");
        let out = root.join("clipes");
        let cfg = Config {
            notify: false,
            mic: false,
            audio: true,
            output_dir: out.clone(),
            ..Config::default()
        };
        let shared = Shared::with_paths(cfg, root.join("tmp"), root.join("app"), ffmpeg.clone());
        clean_dir(&shared.paths.buffer_dir);
        let seg_pattern = shared.paths.buffer_dir.join("seg_%03d.ts");
        let mut source = std::process::Command::new(&ffmpeg)
            .args(["-hide_banner", "-loglevel", "error", "-re", "-f", "lavfi"])
            .args(["-i", "testsrc=s=320x180:r=30", "-t", "20"])
            .args(["-c:v", "libx264", "-preset", "ultrafast", "-g", "60", "-f", "segment"])
            .args(["-segment_time", "2", "-segment_wrap", "8", "-segment_format", "mpegts"])
            .args(["-reset_timestamps", "1"])
            .arg(seg_pattern)
            .spawn()
            .unwrap();
        // Som de mentira chegando como na captura real.
        let ring = shared.audio.clone();
        let tone = std::thread::spawn(move || {
            for _ in 0..1800 {
                ring.push(vec![0.25f32; 960]);
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        let sleep = |s| std::thread::sleep(Duration::from_secs(s));
        sleep(3);
        toggle_recording(shared.clone());
        assert!(shared.recording.load(Ordering::SeqCst));
        sleep(3);
        // Clipar o passado no meio da gravação tem que funcionar.
        trigger_save(shared.clone());
        sleep(5);
        toggle_recording(shared.clone());
        finish_recording(&shared);
        while shared.saving.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = source.kill();
        let _ = source.wait();
        let _ = tone.join();

        let mut clips: Vec<PathBuf> = std::fs::read_dir(&out)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .collect();
        clips.sort();
        assert_eq!(clips.len(), 2, "devia ter o clipe e a gravação: {clips:?}");
        for clip in &clips {
            let o = std::process::Command::new(&ffmpeg)
                .args(["-hide_banner", "-i"])
                .arg(clip)
                .args(["-vn", "-af", "volumedetect", "-f", "null", "-"])
                .output()
                .unwrap();
            let text = String::from_utf8_lossy(&o.stderr);
            let duration = parse_duration(&text).unwrap();
            let max = text
                .split("max_volume: ")
                .nth(1)
                .and_then(|r| r.split(' ').next())
                .and_then(|v| v.parse::<f64>().ok())
                .unwrap();
            println!("{clip:?}: {duration:.1}s, pico {max} dB");
            assert!(max > -30.0, "clipe sem som: {clip:?}");
            assert!(duration >= 4.0, "curto demais: {clip:?}");
        }
        assert!(std::fs::read_dir(&shared.paths.record_dir).unwrap().next().is_none());
        shared.audio.clear();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn clip_name_never_overwrites() {
        let dir = std::env::temp_dir().join(format!("repic-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let a = unique_clip_path(&dir, "2026-01-01_10-00-00");
        std::fs::write(&a, b"x").unwrap();
        let b = unique_clip_path(&dir, "2026-01-01_10-00-00");
        assert_ne!(a, b);
        assert!(b.to_string_lossy().ends_with("_2.mp4"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
