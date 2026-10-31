//! Captura o som do PC (loopback do WASAPI) e o microfone padrão, e guarda os
//! últimos segundos de cada um em arquivos temporários, com o horário de cada
//! trecho. Na memória fica só o índice: 2 horas de som ocupariam quase 3 GB de RAM.
//! Na hora de salvar, monta um arquivo exatamente na janela de tempo do vídeo.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
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

/// Cada arquivo guarda até 1 minuto de som; os vencidos são apagados inteiros.
const SECS_PER_FILE: u64 = 60;
const BYTES_PER_SAMPLE: u64 = 4;

/// Trecho de som guardado em disco, sem buraco no meio.
#[derive(Clone)]
struct Extent {
    /// Começo do trecho contínuo de som a que ele pertence.
    anchor: SystemTime,
    /// Posição dentro do trecho contínuo, em frames.
    offset: u64,
    frames: u64,
    file: u64,
    /// Onde começa no arquivo, em bytes.
    pos: u64,
}

struct Inner {
    sample_rate: u32,
    channels: u16,
    extents: VecDeque<Extent>,
    /// Trecho contínuo atual: (início, frames recebidos até agora).
    run: Option<(SystemTime, u64)>,
    writer: Option<BufWriter<File>>,
    next_file: u64,
    /// Arquivo mais antigo que ainda não foi apagado.
    oldest_file: u64,
    file_pos: u64,
    write_failed: bool,
}

impl Inner {
    fn frames_to_dur(&self, frames: u64) -> Duration {
        Duration::from_secs_f64(frames as f64 / self.sample_rate as f64)
    }

    fn frame_bytes(&self) -> u64 {
        self.channels.max(1) as u64 * BYTES_PER_SAMPLE
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
    dir: PathBuf,
    pub enabled: AtomicBool,
    keep_secs: AtomicU32,
    hold: AtomicU32,
}

/// Som bruto (f32) pronto pro ffmpeg.
pub struct RenderedAudio {
    pub sample_rate: u32,
    pub channels: u16,
    pub peak: f32,
}

impl AudioRing {
    pub fn new(enabled: bool, keep_secs: u32, dir: PathBuf) -> Arc<Self> {
        let ring = Arc::new(Self {
            inner: Mutex::new(Inner {
                sample_rate: 48_000,
                channels: 2,
                extents: VecDeque::new(),
                run: None,
                writer: None,
                next_file: 0,
                oldest_file: 0,
                file_pos: 0,
                write_failed: false,
            }),
            dir,
            enabled: AtomicBool::new(enabled),
            keep_secs: AtomicU32::new(keep_secs),
            hold: AtomicU32::new(0),
        });
        // Sobras de uma execução que fechou sem limpar.
        ring.clear();
        ring
    }

    /// Enquanto um clipe é salvo ou uma gravação roda, nada do som guardado pode
    /// ser apagado. Conta os pedidos: os dois podem acontecer juntos.
    pub fn set_hold(&self, hold: bool) {
        if hold {
            self.hold.fetch_add(1, Ordering::Relaxed);
        } else {
            self.hold.fetch_sub(1, Ordering::Relaxed);
        }
    }

    pub fn set_keep_secs(&self, secs: u32) {
        self.keep_secs.store(secs, Ordering::Relaxed);
    }

    fn file_path(&self, id: u64) -> PathBuf {
        self.dir.join(format!("{id}.f32"))
    }

    fn reset_format(&self, sample_rate: u32, channels: u16) {
        let changed = {
            let inner = self.inner.lock().unwrap();
            inner.sample_rate != sample_rate || inner.channels != channels
        };
        if changed {
            self.clear();
        }
        let mut inner = self.inner.lock().unwrap();
        inner.sample_rate = sample_rate;
        inner.channels = channels;
        inner.run = None;
    }

    pub(crate) fn push(&self, samples: Vec<f32>) {
        self.push_at(SystemTime::now(), samples);
    }

    /// Guarda um pedaço que chegou em `now`. Pedaços em sequência são
    /// emendados exatamente (contando frames), sem usar o horário de cada um:
    /// o callback varia uns milissegundos, e usar esse horário deixava
    /// buraquinhos/sobreposições entre pedaços, que viravam chiado.
    fn push_at(&self, now: SystemTime, samples: Vec<f32>) {
        let keep = Duration::from_secs(self.keep_secs.load(Ordering::Relaxed) as u64 + 20);
        let mut inner = self.inner.lock().unwrap();
        let frames = samples.len() as u64 / inner.channels.max(1) as u64;
        if frames == 0 {
            return;
        }
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

        let Some((file, pos)) = self.write_samples(&mut inner, &samples) else {
            // Disco cheio, por exemplo: esse pedaço vira silêncio no clipe.
            inner.run = None;
            return;
        };
        inner.run = Some((anchor, offset + frames));

        let frame_bytes = inner.frame_bytes();
        let merged = inner.extents.back_mut().is_some_and(|last| {
            let same = last.file == file
                && last.anchor == anchor
                && last.offset + last.frames == offset
                && last.pos + last.frames * frame_bytes == pos;
            if same {
                last.frames += frames;
            }
            same
        });
        if !merged {
            inner.extents.push_back(Extent {
                anchor,
                offset,
                frames,
                file,
                pos,
            });
        }
        // Clipe longo demora pra salvar: não apaga o começo dele enquanto isso.
        if self.hold.load(Ordering::Relaxed) == 0 {
            self.prune(&mut inner, now, keep);
        }
    }

    /// Escreve no arquivo atual (ou num novo) e devolve (arquivo, posição).
    fn write_samples(&self, inner: &mut Inner, samples: &[f32]) -> Option<(u64, u64)> {
        let file_limit = inner.sample_rate as u64 * inner.frame_bytes() * SECS_PER_FILE;
        if inner.writer.is_none() || inner.file_pos >= file_limit {
            if let Some(mut old) = inner.writer.take() {
                let _ = old.flush();
            }
            let id = inner.next_file;
            let created = std::fs::create_dir_all(&self.dir)
                .and_then(|_| File::create(self.file_path(id)));
            match created {
                Ok(f) => {
                    inner.writer = Some(BufWriter::with_capacity(256 * 1024, f));
                    inner.next_file = id + 1;
                    inner.file_pos = 0;
                }
                Err(e) => {
                    self.log_write_error(inner, &e);
                    return None;
                }
            }
        }
        let file = inner.next_file - 1;
        let pos = inner.file_pos;
        let writer = inner.writer.as_mut()?;
        for s in samples {
            if let Err(e) = writer.write_all(&s.to_le_bytes()) {
                // O arquivo ficou pela metade: o próximo pedaço começa em outro.
                inner.writer = None;
                self.log_write_error(inner, &e);
                return None;
            }
        }
        inner.file_pos += samples.len() as u64 * BYTES_PER_SAMPLE;
        inner.write_failed = false;
        Some((file, pos))
    }

    fn log_write_error(&self, inner: &mut Inner, e: &std::io::Error) {
        if !inner.write_failed {
            crate::platform::log(&format!("áudio: não consegui gravar no disco: {e}"));
            inner.write_failed = true;
        }
    }

    /// Esquece o som vencido e apaga os arquivos que ficaram sem uso.
    fn prune(&self, inner: &mut Inner, now: SystemTime, keep: Duration) {
        while let Some(front) = inner.extents.front() {
            let end = front.anchor + inner.frames_to_dur(front.offset + front.frames);
            match now.duration_since(end) {
                Ok(age) if age > keep => {
                    inner.extents.pop_front();
                }
                _ => break,
            }
        }
        let first_in_use = inner
            .extents
            .front()
            .map(|e| e.file)
            .unwrap_or(inner.next_file.saturating_sub(1));
        while inner.oldest_file < first_in_use {
            let _ = std::fs::remove_file(self.file_path(inner.oldest_file));
            inner.oldest_file += 1;
        }
    }

    pub fn clear(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.extents.clear();
        inner.run = None;
        inner.writer = None;
        inner.file_pos = 0;
        inner.oldest_file = inner.next_file;
        if let Ok(entries) = std::fs::read_dir(&self.dir) {
            for e in entries.flatten() {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }

    /// Monta em `out` o áudio de `start` até `start + duration`. Onde não chegou
    /// som (o WASAPI fica quieto quando nada toca) vira silêncio, então o clipe
    /// sempre tem faixa de áudio. Lê sem segurar a trava: a captura continua
    /// enquanto um clipe longo é montado.
    pub fn render_to_file(
        &self,
        start: SystemTime,
        duration: Duration,
        out: &Path,
    ) -> Result<RenderedAudio, String> {
        let (sample_rate, channels, extents) = {
            let mut inner = self.inner.lock().unwrap();
            if let Some(w) = inner.writer.as_mut() {
                let _ = w.flush();
            }
            (inner.sample_rate, inner.channels, inner.extents.clone())
        };
        let sr = sample_rate as f64;
        let frame_bytes = channels.max(1) as u64 * BYTES_PER_SAMPLE;
        let total = (duration.as_secs_f64() * sr).round() as i64;
        let mut output = File::create(out).map_err(|e| e.to_string())?;
        // O arquivo já nasce cheio de zeros, que em f32 é silêncio.
        output
            .set_len(total as u64 * frame_bytes)
            .map_err(|e| e.to_string())?;

        let mut peak = 0f32;
        let mut buf = vec![0u8; 1024 * 1024 - (1024 * 1024 % frame_bytes as usize)];
        let mut source: Option<(u64, File)> = None;
        for x in &extents {
            // Todos os pedaços do mesmo trecho usam a mesma âncora, então
            // caem colados um no outro, sem buraco.
            let base = (signed_secs(x.anchor, start) * sr).round() as i64;
            let first = base + x.offset as i64;
            let from = first.max(0);
            let to = (first + x.frames as i64).min(total);
            if from >= to {
                continue;
            }
            if source.as_ref().is_none_or(|(id, _)| *id != x.file) {
                source = File::open(self.file_path(x.file)).ok().map(|f| (x.file, f));
            }
            // Arquivo apagado no meio do caminho: esse trecho fica em silêncio.
            let Some((_, input)) = source.as_mut() else {
                continue;
            };
            let skip = (from - first) as u64 * frame_bytes;
            input
                .seek(SeekFrom::Start(x.pos + skip))
                .map_err(|e| e.to_string())?;
            output
                .seek(SeekFrom::Start(from as u64 * frame_bytes))
                .map_err(|e| e.to_string())?;
            let mut left = (to - from) as u64 * frame_bytes;
            while left > 0 {
                let n = (left as usize).min(buf.len());
                input.read_exact(&mut buf[..n]).map_err(|e| e.to_string())?;
                output.write_all(&buf[..n]).map_err(|e| e.to_string())?;
                for b in buf[..n].chunks_exact(4) {
                    peak = peak.max(f32::from_le_bytes([b[0], b[1], b[2], b[3]]).abs());
                }
                left -= n as u64;
            }
        }
        output.flush().map_err(|e| e.to_string())?;
        Ok(RenderedAudio {
            sample_rate,
            channels,
            peak,
        })
    }
}

/// Liga a thread que mantém a captura viva (reconecta se o dispositivo mudar).
pub fn start(ring: Arc<AudioRing>, source: Source) {
    std::thread::Builder::new()
        .name(format!("audio-{}", source.label()))
        .spawn(move || {
            // Tenta de novo a cada 2s: anota só quando o erro muda, senão enche o diário.
            let mut last_err = String::new();
            loop {
                if !ring.enabled.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(500));
                    continue;
                }
                if let Err(e) = run_once(&ring, source) {
                    if e != last_err {
                        crate::platform::log(&format!("áudio ({}): {e}", source.label()));
                        last_err = e;
                    }
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
    // Som tocando em outro dispositivo não entra no clipe: o diário mostra qual é o capturado.
    let name = device
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_else(|_| "?".into());
    crate::platform::log(&format!(
        "áudio ({}): capturando \"{name}\", {} Hz, {} canais, {:?}",
        source.label(),
        config.sample_rate,
        config.channels,
        supported.sample_format()
    ));

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
    // Trocar de fone não pode apagar o som de uma gravação longa; desligar o
    // áudio pode, senão o som de antes voltaria num clipe depois de religar.
    if !ring.enabled.load(Ordering::Relaxed) {
        ring.clear();
    }
    Ok(())
}

fn stops_capture(kind: cpal::ErrorKind) -> bool {
    kind != cpal::ErrorKind::Xrun
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
    let mut xrun_logged = false;
    // No WASAPI, abrir um stream de *entrada* num dispositivo de *saída*
    // liga o modo loopback automaticamente. No microfone é entrada normal.
    device
        .build_input_stream(
            config,
            move |data: &[T], _: &cpal::InputCallbackInfo| {
                ring.push(data.iter().map(|s| s.to_sample::<f32>()).collect());
            },
            move |err| {
                // Xrun é só um salto no áudio, comum em call: a captura continua.
                // Reiniciar aqui apagava o buffer sem parar e o clipe saía mudo.
                if !stops_capture(err.kind()) {
                    if !xrun_logged {
                        crate::platform::log(&format!("áudio: salto na captura: {err}"));
                        xrun_logged = true;
                    }
                    return;
                }
                crate::platform::log(&format!("áudio: a captura caiu: {err}"));
                failed.store(true, Ordering::Relaxed);
            },
            None,
        )
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn test_dir() -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("repic-audio-test-{}-{n}", std::process::id()))
    }

    /// Monta a janela e devolve as amostras.
    fn render(ring: &AudioRing, start: SystemTime, duration: Duration) -> Vec<f32> {
        let out = ring.dir.with_extension("raw");
        ring.render_to_file(start, duration, &out).unwrap();
        let bytes = std::fs::read(&out).unwrap();
        let _ = std::fs::remove_file(&out);
        bytes
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect()
    }

    fn cleanup(ring: &AudioRing) {
        ring.clear();
        let _ = std::fs::remove_dir_all(&ring.dir);
    }

    #[test]
    fn jittery_callbacks_still_produce_continuous_sound() {
        // 48 kHz mono, pedaços de 10 ms chegando com horário variando ±3 ms
        // (é o que o WASAPI faz). Antes, isso deixava buraquinhos = chiado.
        let ring = AudioRing::new(true, 30, test_dir());
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
        let a = render(&ring, t0 + Duration::from_millis(500), Duration::from_secs(1));
        assert_eq!(a.len(), 48_000);
        assert!(a.iter().all(|&s| s > 0.0), "tem buraco no áudio");
        let first = (0..i).find(|&n| wave(n) == a[0]).unwrap();
        for (n, &s) in a.iter().enumerate() {
            assert_eq!(s, wave(first + n), "descontinuidade na amostra {n}");
        }
        cleanup(&ring);
    }

    #[test]
    fn render_places_chunks_on_timeline_and_fills_gaps_with_silence() {
        let ring = AudioRing::new(true, 30, test_dir());
        ring.reset_format(10, 1); // 10 Hz mono pra facilitar a conta
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
        // pedaço que terminou em t0+1s com 10 amostras -> cobre [t0, t0+1)
        ring.push_at(t0 + Duration::from_secs(1), vec![1.0; 10]);
        // silêncio do WASAPI entre 1s e 2s, depois pedaço [2s, 2.5s)
        ring.push_at(t0 + Duration::from_millis(2500), vec![2.0; 5]);
        let a = render(&ring, t0 + Duration::from_millis(500), Duration::from_secs(2));
        assert_eq!(a.len(), 20);
        assert!(a[..5].iter().all(|&s| s == 1.0)); // 0.5s..1.0s
        assert!(a[5..15].iter().all(|&s| s == 0.0)); // buraco
        assert!(a[15..20].iter().all(|&s| s == 2.0)); // 2.0s..2.5s
        cleanup(&ring);
    }

    #[test]
    fn xrun_does_not_stop_capture() {
        assert!(!stops_capture(cpal::ErrorKind::Xrun));
        assert!(stops_capture(cpal::ErrorKind::DeviceBusy));
    }

    #[test]
    fn long_buffers_stay_on_disk_and_old_files_are_deleted() {
        // 10 Hz mono guardando ~21 s, por 5 "minutos" de som contínuo.
        let ring = AudioRing::new(true, 1, test_dir());
        ring.reset_format(10, 1);
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
        for k in 1..=300u64 {
            ring.push_at(t0 + Duration::from_secs(k), vec![k as f32; 10]);
        }
        // Só sobra o arquivo atual e o anterior, não os 5 minutos inteiros.
        let files = std::fs::read_dir(&ring.dir).unwrap().count();
        assert!(files <= 2, "sobraram {files} arquivos");
        // Som contínuo vira poucos trechos no índice, não um por pedaço.
        assert!(ring.inner.lock().unwrap().extents.len() <= 2);
        // Os últimos 3 segundos continuam certos, mesmo cruzando arquivos.
        let a = render(&ring, t0 + Duration::from_secs(297), Duration::from_secs(3));
        assert_eq!(a.len(), 30);
        for (i, s) in a.iter().enumerate() {
            assert_eq!(*s, 298.0 + (i / 10) as f32);
        }
        cleanup(&ring);
    }

    #[test]
    fn saving_a_clip_keeps_its_beginning() {
        // Guarda ~21 s, mas o som continua chegando durante um save demorado.
        let ring = AudioRing::new(true, 1, test_dir());
        ring.reset_format(10, 1);
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
        for k in 1..=10u64 {
            ring.push_at(t0 + Duration::from_secs(k), vec![k as f32; 10]);
        }
        ring.set_hold(true);
        for k in 11..=200u64 {
            ring.push_at(t0 + Duration::from_secs(k), vec![k as f32; 10]);
        }
        let a = render(&ring, t0, Duration::from_secs(2));
        assert_eq!(a[0], 1.0, "apagou o começo do clipe durante o save");
        ring.set_hold(false);
        ring.push_at(t0 + Duration::from_secs(201), vec![201.0; 10]);
        let a = render(&ring, t0, Duration::from_secs(2));
        assert_eq!(a[0], 0.0, "voltou a limpar depois do save");
        cleanup(&ring);
    }

    #[test]
    fn render_crosses_file_boundaries() {
        let ring = AudioRing::new(true, 7200, test_dir());
        ring.reset_format(10, 1);
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
        for k in 1..=130u64 {
            ring.push_at(t0 + Duration::from_secs(k), vec![k as f32; 10]);
        }
        let a = render(&ring, t0 + Duration::from_secs(55), Duration::from_secs(10));
        assert_eq!(a.len(), 100);
        for (i, s) in a.iter().enumerate() {
            assert_eq!(*s, 56.0 + (i / 10) as f32);
        }
        cleanup(&ring);
    }

    /// Captura o som do PC de verdade por 4s. Rodar com algo tocando:
    /// `cargo test live_loopback -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_loopback() {
        let ring = AudioRing::new(true, 30, test_dir());
        start(ring.clone(), Source::Loopback);
        std::thread::sleep(Duration::from_secs(4));
        {
            let inner = ring.inner.lock().unwrap();
            let frames: u64 = inner.extents.iter().map(|x| x.frames).sum();
            println!(
                "formato {} Hz x{} | trechos {} | {:.2}s de som",
                inner.sample_rate,
                inner.channels,
                inner.extents.len(),
                frames as f64 / inner.sample_rate as f64
            );
        }
        let now = SystemTime::now();
        let out = ring.dir.with_extension("raw");
        let a = ring
            .render_to_file(now - Duration::from_secs(3), Duration::from_secs(3), &out)
            .unwrap();
        let _ = std::fs::remove_file(&out);
        println!("render 3s: pico {:.3}", a.peak);
        ring.enabled.store(false, Ordering::Relaxed);
    }

    #[test]
    fn render_without_any_sound_is_silence_not_nothing() {
        let ring = AudioRing::new(true, 30, test_dir());
        ring.reset_format(10, 2);
        let a = render(&ring, SystemTime::now(), Duration::from_secs(3));
        assert_eq!(a.len(), 60);
        assert!(a.iter().all(|&s| s == 0.0));
        cleanup(&ring);
    }
}
