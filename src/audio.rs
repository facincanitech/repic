//! Captura o som que o PC está tocando (loopback do WASAPI) e guarda
//! os últimos segundos na memória, com o horário de cada pedaço.
//! Na hora de salvar, monta um WAV exatamente na janela de tempo do vídeo.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

struct Inner {
    sample_rate: u32,
    channels: u16,
    /// (horário em que o pedaço chegou, amostras intercaladas)
    chunks: VecDeque<(SystemTime, Vec<f32>)>,
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
    }

    fn push(&self, samples: Vec<f32>) {
        let now = SystemTime::now();
        let keep = Duration::from_secs(self.keep_secs.load(Ordering::Relaxed) as u64 + 20);
        let mut inner = self.inner.lock().unwrap();
        inner.chunks.push_back((now, samples));
        while let Some((t, _)) = inner.chunks.front() {
            match now.duration_since(*t) {
                Ok(age) if age > keep => {
                    inner.chunks.pop_front();
                }
                _ => break,
            }
        }
    }

    pub fn clear(&self) {
        self.inner.lock().unwrap().chunks.clear();
    }

    /// Monta o áudio de `start` até `start + duration`. Onde não chegou som
    /// (o WASAPI fica quieto quando nada toca) vira silêncio.
    pub fn render(&self, start: SystemTime, duration: Duration) -> Option<RenderedAudio> {
        let inner = self.inner.lock().unwrap();
        if inner.chunks.is_empty() {
            return None;
        }
        let sr = inner.sample_rate as f64;
        let ch = inner.channels as usize;
        let total_frames = (duration.as_secs_f64() * sr).round() as usize;
        let mut out = vec![0.0f32; total_frames * ch];

        for (t_end, data) in &inner.chunks {
            let frames = data.len() / ch;
            // O callback chega no fim do pedaço: o início foi `frames` antes.
            let end_offset = match t_end.duration_since(start) {
                Ok(d) => d.as_secs_f64(),
                Err(e) => -e.duration().as_secs_f64(),
            };
            let first_frame = (end_offset * sr).round() as i64 - frames as i64;
            for f in 0..frames {
                let pos = first_frame + f as i64;
                if pos < 0 || pos as usize >= total_frames {
                    continue;
                }
                let dst = pos as usize * ch;
                out[dst..dst + ch].copy_from_slice(&data[f * ch..f * ch + ch]);
            }
        }
        Some(RenderedAudio {
            samples: out,
            sample_rate: inner.sample_rate,
            channels: inner.channels,
        })
    }
}

/// Liga a thread que mantém a captura viva (reconecta se o dispositivo mudar).
pub fn start(ring: Arc<AudioRing>) {
    std::thread::Builder::new()
        .name("audio-loopback".into())
        .spawn(move || {
            loop {
                if !ring.enabled.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(500));
                    continue;
                }
                if let Err(e) = run_once(&ring) {
                    eprintln!("áudio: {e}");
                }
                std::thread::sleep(Duration::from_secs(2));
            }
        })
        .expect("thread de áudio");
}

fn run_once(ring: &Arc<AudioRing>) -> Result<(), String> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or("nenhum dispositivo de saída")?;
    let supported = device.default_output_config().map_err(|e| e.to_string())?;
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

    // Fica vivo até dar erro (ex.: trocou de fone) ou o áudio ser desligado.
    while !failed.load(Ordering::Relaxed) && ring.enabled.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(300));
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
    // liga o modo loopback automaticamente.
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
    fn render_places_chunks_on_timeline_and_fills_gaps_with_silence() {
        let ring = AudioRing::new(true, 30);
        ring.reset_format(10, 1); // 10 Hz mono pra facilitar a conta
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
        {
            let mut inner = ring.inner.lock().unwrap();
            // pedaço que terminou em t0+1s com 10 amostras -> cobre [t0, t0+1)
            inner
                .chunks
                .push_back((t0 + Duration::from_secs(1), vec![1.0; 10]));
            // silêncio do WASAPI entre 1s e 2s, depois pedaço [2s, 2.5s)
            inner
                .chunks
                .push_back((t0 + Duration::from_millis(2500), vec![2.0; 5]));
        }
        let a = ring
            .render(t0 + Duration::from_millis(500), Duration::from_secs(2))
            .unwrap();
        assert_eq!(a.samples.len(), 20);
        assert!(a.samples[..5].iter().all(|&s| s == 1.0)); // 0.5s..1.0s
        assert!(a.samples[5..15].iter().all(|&s| s == 0.0)); // buraco
        assert!(a.samples[15..20].iter().all(|&s| s == 2.0)); // 2.0s..2.5s
    }
}
