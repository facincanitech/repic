//! Gravação contínua em loop + salvar clipe.
//!
//! Como funciona:
//! - Um ffmpeg fica gravando a tela em pedaços de 2s (`seg_000.ts`, `seg_001.ts`, …).
//!   Com `-segment_wrap` ele reaproveita os nomes, então o disco nunca enche.
//! - Usamos o encoder da placa de vídeo (NVENC/AMF/QSV) pra quase não pesar no jogo.
//!   Se nenhum funcionar, cai pro x264 na CPU.
//! - No atalho, juntamos os últimos pedaços prontos sem reprocessar (`-c copy`),
//!   colamos o áudio (PC + microfone) da mesma janela de tempo e pronto.

use crate::audio;
use crate::lang::{fill, t};
use crate::platform::{self, Monitor, hidden_command};
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

const NVIDIA: u32 = 0x10de;
const AMD: u32 = 0x1002;
const INTEL: u32 = 0x8086;

struct Profile {
    name: &'static str,
    /// Fabricante da placa que esse encoder usa (None = CPU).
    vendor: Option<u32>,
    args: fn(fps: u32, mon: Option<&Monitor>) -> Vec<String>,
}

/// Fonte de vídeo via Desktop Duplication. Com o monitor conhecido, cria o
/// dispositivo D3D11 na placa onde ele está ligado; sem isso o ddagrab só
/// enxerga os monitores da primeira placa (em notebook com duas placas,
/// a tela do notebook costuma ficar de fora).
fn dda(fps: u32, download: bool, mon: Option<&Monitor>) -> Vec<String> {
    let dl = if download { ",hwdownload,format=bgra" } else { "" };
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
                    "ddagrab=output_idx={}:framerate={fps}:draw_mouse=1{dl}[v]",
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
            format!("ddagrab=output_idx=0:framerate={fps}:draw_mouse=1{dl}"),
        ],
    }
}

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

/// Ordem padrão: placa de vídeo primeiro, CPU por último. Na hora de gravar,
/// o encoder da placa onde o monitor está ligado vai pra frente da fila.
const PROFILES: &[Profile] = &[
    Profile {
        name: "NVIDIA (NVENC)",
        vendor: Some(NVIDIA),
        args: |fps, mon| {
            let mut a = dda(fps, false, mon);
            a.extend(s(&[
                "-c:v",
                "h264_nvenc",
                "-preset",
                "p4",
                "-rc",
                "vbr",
                "-cq",
                "23",
                "-b:v",
                "0",
            ]));
            a
        },
    },
    Profile {
        name: "AMD (AMF)",
        vendor: Some(AMD),
        args: |fps, mon| {
            let mut a = dda(fps, false, mon);
            a.extend(s(&[
                "-c:v", "h264_amf", "-quality", "speed", "-rc", "cqp", "-qp_i", "22", "-qp_p", "24",
            ]));
            a
        },
    },
    Profile {
        name: "Intel (QuickSync)",
        vendor: Some(INTEL),
        args: |fps, mon| {
            let mut a = dda(fps, true, mon);
            a.extend(s(&[
                "-c:v",
                "h264_qsv",
                "-preset",
                "veryfast",
                "-global_quality",
                "24",
                "-pix_fmt",
                "nv12",
            ]));
            a
        },
    },
    Profile {
        name: "CPU (x264)",
        vendor: None,
        args: |fps, mon| {
            let mut a = dda(fps, true, mon);
            a.extend(s(&[
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-crf",
                "23",
                "-pix_fmt",
                "yuv420p",
            ]));
            a
        },
    },
    Profile {
        name: "CPU (x264, GDI)",
        vendor: None,
        args: |fps, mon| {
            let fps = fps.min(30).to_string();
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
            a.extend(s(&[
                "-i",
                "desktop",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-crf",
                "23",
                "-pix_fmt",
                "yuv420p",
            ]));
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
    let dir = &shared.paths.buffer_dir;
    clean_dir(dir);
    let wrap = cfg.seconds / SEGMENT_SECS + 6;
    let gop = (cfg.fps * SEGMENT_SECS).to_string();

    let mut cmd = hidden_command(&shared.paths.ffmpeg);
    cmd.args(["-hide_banner", "-loglevel", "error", "-y"]);
    cmd.args((profile.args)(cfg.fps, mon));
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
                            let encoder = PROFILES[order[pos]].name;
                            shared.set_status(
                                Health::Recording,
                                fill(t().recording, &[&tela, &encoder, &cfg.fps, &cfg.seconds]),
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
        platform::beep_error();
        shared.set_event(false, t().no_game);
        return;
    }
    trigger_save(shared);
}

/// Salva o clipe numa thread pra não travar nada.
pub fn trigger_save(shared: Arc<Shared>) {
    if shared.saving.swap(true, Ordering::SeqCst) {
        return; // já está salvando
    }
    std::thread::spawn(move || {
        shared.set_event(true, t().saving);
        match save_clip(&shared) {
            Ok(path) => {
                platform::beep_ok();
                let name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                shared.set_event(true, fill(t().saved, &[&name]));
                shared.history_dirty.store(true, Ordering::SeqCst);
            }
            Err(e) => {
                platform::beep_error();
                shared.set_event(false, fill(t().save_failed, &[&e]));
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

fn probe_duration(shared: &Shared, file: &Path) -> Option<f64> {
    let out = hidden_command(&shared.paths.ffprobe)
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "csv=p=0",
        ])
        .arg(file)
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
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

/// Argumentos do ffmpeg pra juntar vídeo + faixas de áudio num MP4.
fn mux_args(video: &str, wavs: &[String], out: &str) -> Vec<String> {
    let mut a = vec!["-i".to_string(), video.to_string()];
    for w in wavs {
        a.extend(["-i".to_string(), w.clone()]);
    }
    match wavs.len() {
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
            a.extend(s(&["-c:v", "copy", "-c:a", "aac", "-b:a", "192k", "-shortest"]));
        }
    }
    a.extend(s(&["-movflags", "+faststart", out]));
    a
}

fn save_clip(shared: &Shared) -> Result<PathBuf, String> {
    let cfg = shared.config();
    let segs = completed_segments(&shared.paths.buffer_dir);
    if segs.is_empty() {
        return Err(t().buffer_empty.into());
    }
    let wanted = cfg.seconds.div_ceil(SEGMENT_SECS) as usize;
    let take = &segs[segs.len().saturating_sub(wanted)..];
    let video_end = take.last().unwrap().1;

    let work = &shared.paths.work_dir;
    clean_dir(work);

    // 1) Junta os pedaços sem reprocessar.
    let list = work.join("list.txt");
    let body: String = take
        .iter()
        .map(|(f, _)| {
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
    let mut wavs = Vec::new();
    if let Some(start) = video_end.checked_sub(duration) {
        for (on, ring, name) in [
            (cfg.audio, &shared.audio, "pc.wav"),
            (cfg.mic, &shared.mic, "mic.wav"),
        ] {
            if on {
                let wav = work.join(name);
                audio::write_wav(&wav, &ring.render(start, duration))?;
                wavs.push(p(&wav));
            }
        }
    }

    // 3) Arquivo final.
    let args = mux_args(&p(&video), &wavs, &p(&out));
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
            id: "\\\\.\\DISPLAY1".into(),
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

    #[test]
    fn dda_picks_the_gpu_by_vendor() {
        let a = dda(60, false, Some(&mon(INTEL)));
        assert!(a.contains(&"d3d11va=rp:,vendor_id=0x8086".to_string()));
        assert!(a.contains(&"ddagrab=output_idx=0:framerate=60:draw_mouse=1[v]".to_string()));
        let mut m = mon(NVIDIA);
        m.unique_vendor = false;
        assert!(dda(30, true, Some(&m)).contains(&"d3d11va=rp:1".to_string()));
    }

    #[test]
    fn mux_mixes_pc_and_mic() {
        let a = mux_args("v.mp4", &["pc.wav".into(), "mic.wav".into()], "o.mp4");
        assert!(a.contains(&"[1:a][2:a]amix=inputs=2:duration=first:normalize=0[a]".to_string()));
        let a = mux_args("v.mp4", &["pc.wav".into()], "o.mp4");
        assert!(a.contains(&"1:a".to_string()) && !a.iter().any(|x| x.contains("amix")));
        let a = mux_args("v.mp4", &[], "o.mp4");
        assert!(!a.contains(&"-c:a".to_string()));
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
