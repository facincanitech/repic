//! Gravação contínua em loop + salvar clipe.
//!
//! Como funciona:
//! - Um ffmpeg fica gravando a tela em pedaços de 2s (`seg_000.ts`, `seg_001.ts`, …).
//!   Com `-segment_wrap` ele reaproveita os nomes, então o disco nunca enche.
//! - Usamos o encoder da placa de vídeo (NVENC/AMF/QSV) pra quase não pesar no jogo.
//!   Se nenhum funcionar, cai pro x264 na CPU.
//! - No F8, juntamos os últimos pedaços prontos sem reprocessar (`-c copy`),
//!   colamos o áudio da mesma janela de tempo e pronto.

use crate::audio;
use crate::platform::{self, hidden_command};
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

struct Profile {
    name: &'static str,
    args: fn(fps: u32) -> Vec<String>,
}

fn dda(fps: u32, download: bool) -> Vec<String> {
    let mut src = format!("ddagrab=output_idx=0:framerate={fps}:draw_mouse=1");
    if download {
        src.push_str(",hwdownload,format=bgra");
    }
    vec!["-f".into(), "lavfi".into(), "-i".into(), src]
}

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

/// Em ordem de preferência: placa de vídeo primeiro, CPU por último.
const PROFILES: &[Profile] = &[
    Profile {
        name: "NVIDIA (NVENC)",
        args: |fps| {
            let mut a = dda(fps, false);
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
        args: |fps| {
            let mut a = dda(fps, false);
            a.extend(s(&[
                "-c:v", "h264_amf", "-quality", "speed", "-rc", "cqp", "-qp_i", "22", "-qp_p", "24",
            ]));
            a
        },
    },
    Profile {
        name: "Intel (QuickSync)",
        args: |fps| {
            let mut a = dda(fps, true);
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
        args: |fps| {
            let mut a = dda(fps, true);
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
        name: "CPU (x264, modo compatível)",
        args: |fps| {
            let fps = fps.min(30).to_string();
            s(&[
                "-f",
                "gdigrab",
                "-framerate",
                &fps,
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
            ])
        },
    },
];

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

fn spawn_profile(shared: &Shared, profile: &Profile) -> Result<Running, String> {
    let cfg = shared.config();
    let dir = &shared.paths.buffer_dir;
    clean_dir(dir);
    let wrap = cfg.seconds / SEGMENT_SECS + 6;
    let gop = (cfg.fps * SEGMENT_SECS).to_string();

    let mut cmd = hidden_command(&shared.paths.ffmpeg);
    cmd.args(["-hide_banner", "-loglevel", "error", "-y"]);
    cmd.args((profile.args)(cfg.fps));
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
            "ffmpeg.exe não encontrado. Ele precisa ficar na mesma pasta do ClipCraft.exe"
                .to_string()
        } else {
            format!("não consegui abrir o ffmpeg: {e}")
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
                "ffmpeg fechou sozinho".into()
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
            let mut profile_idx = 0usize;
            let mut crashes = 0u32;

            loop {
                if current.is_none() {
                    shared.set_status(Health::Starting, "Procurando o melhor jeito de gravar…");
                    let mut last_err = String::new();
                    for i in profile_idx..PROFILES.len() {
                        match spawn_profile(&shared, &PROFILES[i]) {
                            Ok(r) => {
                                profile_idx = i;
                                current = Some(r);
                                break;
                            }
                            Err(e) => {
                                let missing = e.contains("não encontrado");
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
                            shared.set_status(
                                Health::Recording,
                                format!(
                                    "Gravando em loop · {} · {} fps · últimos {}s",
                                    PROFILES[profile_idx].name, cfg.fps, cfg.seconds
                                ),
                            );
                        }
                        None => {
                            shared.set_status(
                                Health::Error,
                                format!("Não deu pra gravar: {last_err}"),
                            );
                            profile_idx = 0;
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
                        shared.audio.clear();
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
                            if crashes >= 3 && profile_idx + 1 < PROFILES.len() {
                                profile_idx += 1;
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

/// Chamado pelo atalho. Roda numa thread pra não travar nada.
pub fn trigger_save(shared: Arc<Shared>) {
    if shared.saving.swap(true, Ordering::SeqCst) {
        return; // já está salvando
    }
    std::thread::spawn(move || {
        shared.set_event(true, "Salvando clipe…");
        match save_clip(&shared) {
            Ok(path) => {
                platform::beep_ok();
                let name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                shared.set_event(true, format!("Clipe salvo: {name}"));
                shared.history_dirty.store(true, Ordering::SeqCst);
            }
            Err(e) => {
                platform::beep_error();
                shared.set_event(false, format!("Não salvou: {e}"));
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

fn save_clip(shared: &Shared) -> Result<PathBuf, String> {
    let cfg = shared.config();
    let segs = completed_segments(&shared.paths.buffer_dir);
    if segs.is_empty() {
        return Err("o buffer ainda está vazio, espera uns segundos".into());
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

    std::fs::create_dir_all(&cfg.output_dir).map_err(|e| format!("pasta de saída: {e}"))?;
    let name = chrono::Local::now()
        .format("Clip_%Y-%m-%d_%H-%M-%S.mp4")
        .to_string();
    let out = cfg.output_dir.join(&name);

    // 2) Áudio da mesma janela de tempo do vídeo.
    let duration =
        probe_duration(shared, &video).unwrap_or((take.len() as u32 * SEGMENT_SECS) as f64);
    let duration = Duration::from_secs_f64(duration.max(0.1));
    let audio = if cfg.audio {
        video_end
            .checked_sub(duration)
            .and_then(|start| shared.audio.render(start, duration))
    } else {
        None
    };

    // 3) Arquivo final.
    match audio {
        Some(a) => {
            let wav = work.join("audio.wav");
            audio::write_wav(&wav, &a)?;
            run_ffmpeg(
                shared,
                &[
                    "-i",
                    &p(&video),
                    "-i",
                    &p(&wav),
                    "-map",
                    "0:v",
                    "-map",
                    "1:a",
                    "-c:v",
                    "copy",
                    "-c:a",
                    "aac",
                    "-b:a",
                    "192k",
                    "-shortest",
                    "-movflags",
                    "+faststart",
                    &p(&out),
                ],
            )?;
        }
        None => {
            run_ffmpeg(
                shared,
                &[
                    "-i",
                    &p(&video),
                    "-c",
                    "copy",
                    "-movflags",
                    "+faststart",
                    &p(&out),
                ],
            )?;
        }
    }

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
            "scale=320:-2",
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
