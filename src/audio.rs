//! Captura o som do PC (loopback do WASAPI) e o microfone padrão, e guarda
//! os últimos segundos de cada um na memória, com o horário de cada pedaço.
//! Na hora de salvar, monta um WAV exatamente na janela de tempo do vídeo.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    /// O que sai nos fones/caixas.
    Loopback,
    /// O microfone padrão do Windows.
    Mic,
}

impl Source {
    fn label(self) -> &'static str {
        match self {
            Source::Loopback => "som do PC",
            Source::Mic => "microfone",
        }
    }
}

/// Se um pedaço chega até esse tanto fora do esperado, é continuação do
/// anterior (só variação de horário do callback). Mais que isso = buraco
/// de verdade (o WASAPI fica quieto quando nada toca).
const CONTIGUOUS_TOLERANCE: Duration = Duration::from_millis(60);

struct Chunk {
    /// Começo do trecho contínuo de som a que esse pedaço pertence.
    anchor: SystemTime,
    /// Posição do pedaço dentro do trecho, em frames.
    offset: u64,
    /// Amostras intercaladas.
    data: Vec<f32>,
}

struct Inner {
    sample_rate: u32,
    channels: u16,
    chunks: VecDeque<Chunk>,
    /// Trecho contínuo atual: (início, frames recebidos até agora).
    run: Option<(SystemTime, u64)>,
}

impl Inner {
    fn frames_to_dur(&self, frames: u64) -> Duration {
        Duration::from_secs_f64(frames as f64 / self.sample_rate as f64)
    }
}

fn signed_secs(t: SystemTime, from: SystemTime) -> f64 {
    match t.duration_since(from) {
        Ok(d) => d.as_secs_f64(),
        Err(e) => -e.duration().as_secs_f64(),
    }
}

pub struct AudioRing {
    inner: Mutex<Inner>,
    pub enabled: AtomicBool,
    keep_secs: AtomicU32,
}

pub struct RenderedAudio {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub channels: u16,
}

impl AudioRing {
    pub fn new(enabled: bool, keep_secs: u32) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner {
                sample_rate: 48_000,
                channels: 2,
                chunks: VecDeque::new(),
                run: None,
            }),
            enabled: AtomicBool::new(enabled),
            keep_secs: AtomicU32::new(keep_secs),
        })
    }

    pub fn set_keep_secs(&self, secs: u32) {
        self.keep_secs.store(secs, Ordering::Relaxed);
    }

    fn reset_format(&self, sample_rate: u32, channels: u16) {
        let mut inner = self.inner.lock().unwrap();
        if inner.sample_rate != sample_rate || inner.channels != channels {
            inner.chunks.clear();
        }
        inner.sample_rate = sample_rate;
        inner.channels = channels;
        inner.run = None;
    }

    fn push(&self, samples: Vec<f32>) {
        self.push_at(SystemTime::now(), samples);
    }

    /// Guarda um pedaço que chegou em `now`. Pedaços em sequência são
    /// emendados exatamente (contando frames), sem usar o horário de cada um:
    /// o callback varia uns milissegundos, e usar esse horário deixava
    /// buraquinhos/sobreposições entre pedaços, que viravam chiado.
    fn push_at(&self, now: SystemTime, samples: Vec<f32>) {
        let keep = Duration::from_secs(self.keep_secs.load(Ordering::Relaxed) as u64 + 20);
        let mut inner = self.inner.lock().unwrap();
        let frames = (samples.len() / inner.channels.max(1) as usize) as u64;
        // O callback chega no fim do pedaço: o início foi `frames` antes.
        let wall_start = now
            .checked_sub(inner.frames_to_dur(frames))
            .unwrap_or(now);

        let continues = inner.run.map(|(anchor, total)| {
            let expected = anchor + inner.frames_to_dur(total);
            signed_secs(wall_start, expected).abs() < CONTIGUOUS_TOLERANCE.as_secs_f64()
        });
        let (anchor, offset) = match (continues, inner.run) {
            (Some(true), Some((anchor, total))) => (anchor, total),
            _ => (wall_start, 0),
        };
        inner.run = Some((anchor, offset + frames));
        inner.chunks.push_back(Chunk {
            anchor,
            offset,
            data: samples,
        });

        while let Some(c) = inner.chunks.front() {
            let end = c.anchor + inner.frames_to_dur(c.offset);
            match now.duration_since(end) {
                Ok(age) if age > keep => {
                    inner.chunks.pop_front();
                }
                _ => break,
            }
        }
    }

    pub fn clear(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.chunks.clear();
        inner.run = None;
    }

    /// Monta o áudio de `start` até `start + duration`. Onde não chegou som
    /// (o WASAPI fica quieto quando nada toca) vira silêncio, então o clipe
    /// sempre tem faixa de áudio.
    pub fn render(&self, start: SystemTime, duration: Duration) -> RenderedAudio {
        let inner = self.inner.lock().unwrap();
        let sr = inner.sample_rate as f64;
        let ch = inner.channels as usize;
        let total_frames = (duration.as_secs_f64() * sr).round() as usize;
        let mut out = vec![0.0f32; total_frames * ch];

        for c in &inner.chunks {
            let data = &c.data;
            let frames = data.len() / ch;
            // Todos os pedaços do mesmo trecho usam a mesma âncora, então
            // caem colados um no outro, sem buraco.
            let base = (signed_secs(c.anchor, start) * sr).round() as i64;
            let first_frame = base + c.offset as i64;
            for f in 0..frames {
                let pos = first_frame + f as i64;
                if pos < 0 || pos as usize >= total_frames {
                    continue;
                }
                let dst = pos as usize * ch;
                out[dst..dst + ch].copy_from_slice(&data[f * ch..f * ch + ch]);
            }
        }
        RenderedAudio {
            samples: out,
            sample_rate: inner.sample_rate,
            channels: inner.channels,
        }
    }
}

/// Liga a thread que mantém a captura viva (reconecta se o dispositivo mudar).
pub fn start(ring: Arc<AudioRing>, source: Source) {
    std::thread::Builder::new()
        .name(format!("audio-{}", source.label()))
        .spawn(move || {
            loop {
                if !ring.enabled.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(500));
                    continue;
                }
                if let Err(e) = run_once(&ring, source) {
                    eprintln!("{}: {e}", source.label());
                }
                std::thread::sleep(Duration::from_secs(2));
            }
        })
        .expect("thread de áudio");
}

fn default_device(host: &cpal::Host, source: Source) -> Option<cpal::Device> {
    match source {
        Source::Loopback => host.default_output_device(),
        Source::Mic => host.default_input_device(),
    }
}

fn run_once(ring: &Arc<AudioRing>, source: Source) -> Result<(), String> {
    let host = cpal::default_host();
    let device = default_device(&host, source).ok_or("nenhum dispositivo encontrado")?;
    let device_id = device.id().ok();
    let supported = match source {
        Source::Loopback => device.default_output_config(),
        Source::Mic => device.default_input_config(),
    }
    .map_err(|e| e.to_string())?;
    let config = supported.config();
    ring.reset_format(config.sample_rate, config.channels);

    let failed = Arc::new(AtomicBool::new(false));
    let stream = match supported.sample_format() {
        SampleFormat::F32 => build::<f32>(&device, config, ring, &failed),
        SampleFormat::I16 => build::<i16>(&device, config, ring, &failed),
        SampleFormat::I32 => build::<i32>(&device, config, ring, &failed),
        SampleFormat::U16 => build::<u16>(&device, config, ring, &failed),
        other => return Err(format!("formato de áudio não suportado: {other:?}")),
    }?;
    stream.play().map_err(|e| e.to_string())?;

    // Fica vivo até dar erro, o áudio ser desligado ou o Windows trocar o
    // dispositivo padrão (ex.: plugou um fone).
    let mut last_check = Instant::now();
    while !failed.load(Ordering::Relaxed) && ring.enabled.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(300));
        if last_check.elapsed() >= Duration::from_secs(3) {
            last_check = Instant::now();
            let now_id = default_device(&host, source).and_then(|d| d.id().ok());
            if now_id != device_id {
                break;
            }
        }
    }
    drop(stream);
    ring.clear();
    Ok(())
}

fn build<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    ring: &Arc<AudioRing>,
    failed: &Arc<AtomicBool>,
) -> Result<cpal::Stream, String>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let ring = ring.clone();
    let failed = failed.clone();
    // No WASAPI, abrir um stream de *entrada* num dispositivo de *saída*
    // liga o modo loopback automaticamente. No microfone é entrada normal.
    device
        .build_input_stream(
            config,
            move |data: &[T], _: &cpal::InputCallbackInfo| {
                ring.push(data.iter().map(|s| s.to_sample::<f32>()).collect());
            },
            move |err| {
                eprintln!("stream de áudio: {err}");
                failed.store(true, Ordering::Relaxed);
            },
            None,
        )
        .map_err(|e| e.to_string())
}

pub fn write_wav(path: &std::path::Path, audio: &RenderedAudio) -> Result<(), String> {
    let spec = hound::WavSpec {
        channels: audio.channels,
        sample_rate: audio.sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut w = hound::WavWriter::create(path, spec).map_err(|e| e.to_string())?;
    for s in &audio.samples {
        w.write_sample(*s).map_err(|e| e.to_string())?;
    }
    w.finalize().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jittery_callbacks_still_produce_continuous_sound() {
        // 48 kHz mono, pedaços de 10 ms chegando com horário variando ±3 ms
        // (é o que o WASAPI faz). Antes, isso deixava buraquinhos = chiado.
        let ring = AudioRing::new(true, 30);
        ring.reset_format(48_000, 1);
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
        let wave = |i: usize| (i as f32 * 0.05).sin() * 0.5 + 0.6; // nunca 0
        let jitter_ms = [0i64, 3, -2, 1, -3, 2, 0, -1, 3, -2];
        let mut i = 0usize;
        for k in 0..200 {
            let chunk: Vec<f32> = (0..480).map(|n| wave(i + n)).collect();
            i += 480;
            let ideal_end_ms = (k as i64 + 1) * 10 + jitter_ms[k % jitter_ms.len()];
            ring.push_at(t0 + Duration::from_millis(ideal_end_ms as u64), chunk);
        }
        // Pega 1 s do meio: tem que ser a onda perfeita, sem zero nem pulo.
        let a = ring.render(t0 + Duration::from_millis(500), Duration::from_secs(1));
        assert_eq!(a.samples.len(), 48_000);
        assert!(a.samples.iter().all(|&s| s > 0.0), "tem buraco no áudio");
        let first = (0..i).find(|&n| wave(n) == a.samples[0]).unwrap();
        for (n, &s) in a.samples.iter().enumerate() {
            assert_eq!(s, wave(first + n), "descontinuidade na amostra {n}");
        }
    }

    #[test]
    fn render_places_chunks_on_timeline_and_fills_gaps_with_silence() {
        let ring = AudioRing::new(true, 30);
        ring.reset_format(10, 1); // 10 Hz mono pra facilitar a conta
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
        // pedaço que terminou em t0+1s com 10 amostras -> cobre [t0, t0+1)
        ring.push_at(t0 + Duration::from_secs(1), vec![1.0; 10]);
        // silêncio do WASAPI entre 1s e 2s, depois pedaço [2s, 2.5s)
        ring.push_at(t0 + Duration::from_millis(2500), vec![2.0; 5]);
        let a = ring.render(t0 + Duration::from_millis(500), Duration::from_secs(2));
        assert_eq!(a.samples.len(), 20);
        assert!(a.samples[..5].iter().all(|&s| s == 1.0)); // 0.5s..1.0s
        assert!(a.samples[5..15].iter().all(|&s| s == 0.0)); // buraco
        assert!(a.samples[15..20].iter().all(|&s| s == 2.0)); // 2.0s..2.5s
    }

    /// Captura o som do PC de verdade por 4s. Rodar com algo tocando:
    /// `cargo test live_loopback -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_loopback() {
        let ring = AudioRing::new(true, 30);
        start(ring.clone(), Source::Loopback);
        std::thread::sleep(Duration::from_secs(4));
        {
            let inner = ring.inner.lock().unwrap();
            let runs: std::collections::HashSet<_> = inner.chunks.iter().map(|c| c.anchor).collect();
            let frames: usize = inner.chunks.iter().map(|c| c.data.len()).sum::<usize>() / inner.channels as usize;
            println!(
                "formato {} Hz x{} | pedaços {} | trechos {} | {:.2}s de som",
                inner.sample_rate,
                inner.channels,
                inner.chunks.len(),
                runs.len(),
                frames as f64 / inner.sample_rate as f64
            );
        }
        let now = SystemTime::now();
        let a = ring.render(now - Duration::from_secs(3), Duration::from_secs(3));
        let peak = a.samples.iter().fold(0f32, |m, s| m.max(s.abs()));
        let zeros = a.samples.iter().filter(|s| **s == 0.0).count();
        println!("render 3s: pico {peak:.3} | zeros {:.1}%", 100.0 * zeros as f64 / a.samples.len() as f64);
    }

    #[test]
    fn render_without_any_sound_is_silence_not_nothing() {
        let ring = AudioRing::new(true, 30);
        ring.reset_format(10, 2);
        let a = ring.render(SystemTime::now(), Duration::from_secs(3));
        assert_eq!(a.samples.len(), 60);
        assert!(a.samples.iter().all(|&s| s == 0.0));
    }
}
