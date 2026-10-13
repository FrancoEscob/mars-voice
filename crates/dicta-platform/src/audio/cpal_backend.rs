//! Portable capture through cpal: WASAPI (Windows), CoreAudio (macOS) and, on
//! Linux when PipeWire is not the audio server, PulseAudio or ALSA.
//!
//! Real-time rule: the cpal callback only downmixes to mono f32 and pushes it
//! into a preallocated `rtrb` ring (2 s). It never locks or allocates. When the
//! ring is full it raises a flag, pushes nothing more, and this thread stops
//! capture with an error: audio is never dropped silently.
//!
//! This (normal priority) thread drains the ring every 10 ms, resamples to
//! 16 kHz with rubato's fixed-ratio FFT resampler, converts to s16le and feeds
//! the shared [`PcmBuffer`], so packets, levels and errors match PipeWire.
use super::{AudioInventory, AudioStatus, NO_AUDIO_TIMEOUT, PcmBuffer, SAMPLE_RATE, select_source};
use anyhow::{Context, Result, anyhow, bail, ensure};
use cpal::{
    FromSample, Sample, SampleFormat, SizedSample,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use dicta_core::AudioDevice;
use rubato::{Fft, FixedSync, Resampler, audioadapter_buffers::direct::InterleavedSlice};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc as std_mpsc,
    },
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, watch};

/// Ring capacity, in seconds of device audio.
const RING_SECONDS: usize = 2;
/// How often this thread drains the ring (and notices stop requests).
const POLL: Duration = Duration::from_millis(10);
/// Bound for opening a stream on a stalled audio server.
pub(crate) const OPEN_TIMEOUT: Duration = Duration::from_secs(5);

const CHOSEN_LOST: &str = "El micrófono seleccionado está desconectado o ya no está disponible. Reconectalo o elegí uno disponible en Ajustes > Micrófono";
const DEFAULT_LOST: &str = "El micrófono predeterminado está desconectado o ya no está disponible. Reconectalo o elegí uno disponible en Ajustes > Micrófono";
const OVERFLOW: &str =
    "El micrófono entregó más audio del que se pudo procesar; se detuvo para no perder palabras";

/// The system's audio host: WASAPI, CoreAudio, or on Linux PulseAudio
/// (including pipewire-pulse) when its socket answers, else ALSA.
pub(crate) fn default_host() -> cpal::Host {
    cpal::default_host()
}

/// Hosts whose server converts rate and channel count for us.
pub(crate) fn is_pulse(host: &cpal::Host) -> bool {
    #[cfg(target_os = "linux")]
    {
        host.id() == cpal::HostId::PulseAudio
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = host;
        false
    }
}

/// Stable device id: PulseAudio sink/source name (the PipeWire `node.name`
/// under pipewire-pulse, so saved settings keep matching), WASAPI endpoint ID,
/// CoreAudio device UID or ALSA PCM name. Never an index.
pub(crate) fn stable_id(device: &cpal::Device) -> Option<String> {
    let id = device.id().ok()?;
    let id = id.id().trim();
    (!id.is_empty()).then(|| id.to_owned())
}

pub(crate) fn describe(device: &cpal::Device) -> Option<AudioDevice> {
    let id = stable_id(device)?;
    let name = device
        .description()
        .ok()
        .map(|description| description.name().trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| id.clone());
    Some(AudioDevice { id, name })
}

/// Sorted by display name and deduplicated by id, like the PipeWire registry.
pub(crate) fn sorted(mut devices: Vec<AudioDevice>) -> Vec<AudioDevice> {
    devices.sort_by(|a, b| a.name.cmp(&b.name));
    devices.dedup_by(|a, b| a.id == b.id);
    devices
}

/// PulseAudio lists a `<sink>.monitor` source per output; PipeWire's
/// Audio/Source class does not, and dictating from speakers is never wanted.
fn is_monitor(id: &str) -> bool {
    id.ends_with(".monitor")
}

pub(super) fn inspect_sources() -> Result<AudioInventory> {
    inventory(&default_host())
}

fn inventory(host: &cpal::Host) -> Result<AudioInventory> {
    let pulse = is_pulse(host);
    let devices = host
        .input_devices()
        .map_err(|e| anyhow!("No se pudieron consultar los micrófonos del sistema: {e}"))?
        .filter_map(|device| describe(&device))
        .filter(|device| !(pulse && is_monitor(&device.id)))
        .collect();
    let default_source = host
        .default_input_device()
        .and_then(|device| stable_id(&device))
        .filter(|id| !(pulse && is_monitor(id)));
    Ok(AudioInventory {
        devices: sorted(devices),
        default_source,
        configured_default_source: None,
    })
}

struct InputFormat {
    config: cpal::StreamConfig,
    sample_format: SampleFormat,
}

/// PulseAudio converts on the server: ask for exactly what the STT wants.
/// Without a fixed size its record fragments default to ≈2 s. cpal also uses
/// the fragment as the server-side `maxlength`, so 40 ms (not PipeWire's
/// 20 ms) leaves slack before the server drops audio while the level meter
/// still updates 25 times a second. Other hosts capture their default format
/// and this module downmixes and resamples.
fn input_format(host: &cpal::Host, device: &cpal::Device, lost: &str) -> Result<InputFormat> {
    if is_pulse(host) {
        return Ok(InputFormat {
            config: cpal::StreamConfig {
                channels: 1,
                sample_rate: SAMPLE_RATE,
                buffer_size: cpal::BufferSize::Fixed(SAMPLE_RATE / 25),
            },
            sample_format: SampleFormat::I16,
        });
    }
    let default = device
        .default_input_config()
        .map_err(|e| anyhow!(open_error(&e, lost)))?;
    Ok(InputFormat {
        config: default.config(),
        sample_format: default.sample_format(),
    })
}

pub(super) fn capture_loop(
    device: Option<String>,
    tx: mpsc::Sender<Vec<u8>>,
    status: watch::Sender<AudioStatus>,
    stop: std_mpsc::Receiver<bool>,
    stopping: Arc<AtomicBool>,
) -> Result<()> {
    let host = default_host();
    // Enumeration opens no microphone.
    let target = select_source(&inventory(&host)?, device.as_deref())?;
    let explicit = device.as_deref().is_some_and(|d| !d.trim().is_empty());
    let lost = if explicit { CHOSEN_LOST } else { DEFAULT_LOST };
    let input = host
        .input_devices()
        .ok()
        .and_then(|mut devices| devices.find(|d| stable_id(d).as_deref() == Some(&target)))
        .context(lost)?;
    if stopping.load(Ordering::Acquire) {
        return Ok(());
    }
    let format = input_format(&host, &input, lost)?;
    let rate = format.config.sample_rate;
    ensure!(rate > 0, "El micrófono informó una frecuencia de 0 Hz");
    let (producer, consumer) = rtrb::RingBuffer::new(rate as usize * RING_SECONDS);
    let overflow = Arc::new(AtomicBool::new(false));
    let (errors_tx, errors) = std_mpsc::channel();
    let mut pump = Pump::new(consumer, overflow.clone(), rate)?;
    let stream = build_input(&input, &format, producer, overflow, errors_tx)
        .map_err(|e| anyhow!(open_error(&e, lost)))?;
    if stopping.load(Ordering::Acquire) {
        return Ok(());
    }
    stream.play().map_err(|e| anyhow!(open_error(&e, lost)))?;
    let started = Instant::now();
    let mut recording = false;
    let graceful = loop {
        match stop.recv_timeout(POLL) {
            Ok(flush) => break flush,
            Err(std_mpsc::RecvTimeoutError::Disconnected) => break false,
            Err(std_mpsc::RecvTimeoutError::Timeout) => {}
        }
        if let Some(message) = errors.try_iter().find_map(|e| runtime_error(&e, lost)) {
            bail!(message);
        }
        if let Some(level) = pump.drain(&tx)? {
            recording = true;
            status.send_replace(AudioStatus {
                recording: true,
                level,
                error: None,
            });
        }
        if !recording && started.elapsed() >= NO_AUDIO_TIMEOUT {
            #[cfg(not(windows))]
            bail!(
                "No llegó audio en 8 segundos; revisá el micrófono y los permisos de audio del sistema"
            );
            #[cfg(windows)]
            bail!(
                "No llegó audio en 8 segundos; revisá el micrófono y Configuración > Privacidad y seguridad > Micrófono"
            );
        }
    };
    // Close the microphone first; what it already captured stays in the ring.
    drop(stream);
    if graceful {
        pump.finish(&tx)?;
    }
    status.send_replace(AudioStatus::default());
    Ok(())
}

fn build_input(
    device: &cpal::Device,
    format: &InputFormat,
    ring: rtrb::Producer<f32>,
    overflow: Arc<AtomicBool>,
    errors: std_mpsc::Sender<cpal::Error>,
) -> Result<cpal::Stream, cpal::Error> {
    let config = format.config;
    match format.sample_format {
        SampleFormat::I8 => build::<i8>(device, config, ring, overflow, errors),
        SampleFormat::I16 => build::<i16>(device, config, ring, overflow, errors),
        SampleFormat::I24 => build::<cpal::I24>(device, config, ring, overflow, errors),
        SampleFormat::I32 => build::<i32>(device, config, ring, overflow, errors),
        SampleFormat::I64 => build::<i64>(device, config, ring, overflow, errors),
        SampleFormat::U8 => build::<u8>(device, config, ring, overflow, errors),
        SampleFormat::U16 => build::<u16>(device, config, ring, overflow, errors),
        SampleFormat::U24 => build::<cpal::U24>(device, config, ring, overflow, errors),
        SampleFormat::U32 => build::<u32>(device, config, ring, overflow, errors),
        SampleFormat::U64 => build::<u64>(device, config, ring, overflow, errors),
        SampleFormat::F32 => build::<f32>(device, config, ring, overflow, errors),
        SampleFormat::F64 => build::<f64>(device, config, ring, overflow, errors),
        other => Err(cpal::Error::with_message(
            cpal::ErrorKind::UnsupportedConfig,
            format!("formato {other}"),
        )),
    }
}

fn build<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    mut ring: rtrb::Producer<f32>,
    overflow: Arc<AtomicBool>,
    errors: std_mpsc::Sender<cpal::Error>,
) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let channels = usize::from(config.channels).max(1);
    device.build_input_stream::<T, _, _>(
        config,
        move |data: &[T], _| {
            // Once full, push nothing more: the PCM must never hide a gap.
            if !overflow.load(Ordering::Relaxed) && !push_downmixed(data, channels, &mut ring) {
                overflow.store(true, Ordering::Release);
            }
        },
        move |error| {
            let _ = errors.send(error);
        },
        Some(OPEN_TIMEOUT),
    )
}

/// Callback side: interleaved device frames → mono f32 into the ring. Pushes
/// everything or nothing; false means the ring had no room.
fn push_downmixed<T>(data: &[T], channels: usize, ring: &mut rtrb::Producer<f32>) -> bool
where
    T: Sample,
    f32: FromSample<T>,
{
    let frames = data.len() / channels;
    match ring.write_chunk_uninit(frames) {
        Ok(chunk) => {
            chunk.fill_from_iter(data.chunks_exact(channels).map(downmix));
            true
        }
        Err(_) => false,
    }
}

/// Average of the channels, so a stereo mic keeps the level of each side.
fn downmix<T>(frame: &[T]) -> f32
where
    T: Sample,
    f32: FromSample<T>,
{
    match frame {
        [mono] => mono.to_sample::<f32>(),
        _ => frame.iter().map(|s| s.to_sample::<f32>()).sum::<f32>() / frame.len() as f32,
    }
}

/// Worker side of the ring: overflow check, 16 kHz conversion, packets.
struct Pump {
    ring: rtrb::Consumer<f32>,
    overflow: Arc<AtomicBool>,
    convert: To16k,
    pcm: PcmBuffer,
    bytes: Vec<u8>,
}
impl Pump {
    fn new(ring: rtrb::Consumer<f32>, overflow: Arc<AtomicBool>, rate: u32) -> Result<Self> {
        Ok(Self {
            ring,
            overflow,
            convert: To16k::new(rate)?,
            pcm: PcmBuffer::default(),
            bytes: Vec::with_capacity(SAMPLE_RATE as usize / 5 * 2),
        })
    }
    /// Converts everything queued. Returns the RMS of the new 16 kHz audio, or
    /// None while the resampler is still collecting its first block.
    fn drain(&mut self, tx: &mpsc::Sender<Vec<u8>>) -> Result<Option<f32>> {
        ensure!(!self.overflow.load(Ordering::Acquire), OVERFLOW);
        let available = self.ring.slots();
        if available == 0 {
            return Ok(None);
        }
        self.bytes.clear();
        let chunk = self
            .ring
            .read_chunk(available)
            .expect("slots() frames are readable");
        let (first, second) = chunk.as_slices();
        self.convert.push(first, &mut self.bytes);
        self.convert.push(second, &mut self.bytes);
        chunk.commit_all();
        if self.bytes.is_empty() {
            return Ok(None);
        }
        self.pcm.push(&self.bytes, tx).map(Some)
    }
    /// Graceful stop: the rest of the ring, the resampler tail, then the
    /// trailing sub-100 ms packet.
    fn finish(&mut self, tx: &mpsc::Sender<Vec<u8>>) -> Result<()> {
        self.drain(tx)?;
        self.bytes.clear();
        self.convert.finish(&mut self.bytes);
        if !self.bytes.is_empty() {
            self.pcm.push(&self.bytes, tx)?;
        }
        self.pcm.flush(tx)
    }
}

/// Mono f32 at the device rate → 16 kHz s16le with a fixed-ratio FFT
/// resampler. The filter delay is trimmed, so output sample n lines up with
/// input time n/16000 s, and [`To16k::finish`] makes the total length exactly
/// round(input × 16000 / rate). At 16 kHz it is a bit-exact passthrough.
pub(super) struct To16k {
    fft: Option<Fft<f32>>,
    rate: u32,
    input: Vec<f32>,
    filled: usize,
    output: Vec<f32>,
    /// Filter delay still to discard, in output frames.
    skip: usize,
    fed: u64,
    emitted: u64,
}
impl To16k {
    pub(super) fn new(rate: u32) -> Result<Self> {
        let fft = if rate == SAMPLE_RATE {
            None
        } else {
            // 20 ms blocks, like the PipeWire callbacks and the level meter.
            let block = (rate as usize / 50).max(1);
            Some(
                Fft::<f32>::new(
                    rate as usize,
                    SAMPLE_RATE as usize,
                    block,
                    1,
                    FixedSync::Input,
                )
                .map_err(|e| anyhow!("No se pudo convertir el micrófono de {rate} Hz: {e}"))?,
            )
        };
        let (input, output, skip) = match &fft {
            Some(fft) => (
                vec![0.0; fft.input_frames_max()],
                vec![0.0; fft.output_frames_max()],
                fft.output_delay(),
            ),
            None => (Vec::new(), Vec::new(), 0),
        };
        Ok(Self {
            fft,
            rate,
            input,
            filled: 0,
            output,
            skip,
            fed: 0,
            emitted: 0,
        })
    }

    pub(super) fn push(&mut self, mut samples: &[f32], bytes: &mut Vec<u8>) {
        self.fed += samples.len() as u64;
        let Some(fft) = &self.fft else {
            self.emitted += samples.len() as u64;
            extend_s16le(bytes, samples);
            return;
        };
        let block = fft.input_frames_next();
        while !samples.is_empty() {
            let n = (block - self.filled).min(samples.len());
            self.input[self.filled..self.filled + n].copy_from_slice(&samples[..n]);
            self.filled += n;
            samples = &samples[n..];
            if self.filled == block {
                self.process(u64::MAX, bytes);
            }
        }
    }

    /// Flushes the filter with silence until every input sample is out.
    pub(super) fn finish(&mut self, bytes: &mut Vec<u8>) {
        let Some(fft) = &self.fft else {
            return;
        };
        let block = fft.input_frames_next();
        let expected =
            (self.fed * u64::from(SAMPLE_RATE) + u64::from(self.rate) / 2) / u64::from(self.rate);
        while self.emitted < expected {
            self.input[self.filled..block].fill(0.0);
            self.filled = block;
            self.process(expected, bytes);
        }
    }

    fn process(&mut self, limit: u64, bytes: &mut Vec<u8>) {
        let fft = self.fft.as_mut().expect("only called when resampling");
        let frames = fft.input_frames_next();
        let capacity = self.output.len();
        let written = InterleavedSlice::new(&self.input[..frames], 1, frames)
            .ok()
            .zip(InterleavedSlice::new_mut(&mut self.output[..], 1, capacity).ok())
            .and_then(|(input, mut output)| fft.process_into_buffer(&input, &mut output, None).ok())
            // Buffers are sized from the resampler itself; this cannot fail.
            .map_or(0, |(_, written)| written);
        self.filled = 0;
        let mut out = &self.output[..written];
        let skip = self.skip.min(out.len());
        self.skip -= skip;
        out = &out[skip..];
        let room = limit.saturating_sub(self.emitted).min(out.len() as u64) as usize;
        out = &out[..room];
        self.emitted += out.len() as u64;
        extend_s16le(bytes, out);
    }
}

fn extend_s16le(bytes: &mut Vec<u8>, samples: &[f32]) {
    bytes.extend(samples.iter().flat_map(|x| to_i16(*x).to_le_bytes()));
}
/// Inverse of cpal's i16 → f32 (x / 32768), so 16 kHz i16 input round-trips.
fn to_i16(x: f32) -> i16 {
    (x * 32768.0).round().clamp(-32768.0, 32767.0) as i16
}

#[cfg(not(windows))]
const PERMISSION_DENIED: &str = "El sistema negó el acceso al micrófono. Permití el micrófono para Mars voice en la privacidad del sistema y volvé a intentar";
#[cfg(windows)]
const PERMISSION_DENIED: &str = "Windows bloqueó el micrófono por privacidad. Abrí Configuración > Privacidad y seguridad > Micrófono, activá «Acceso al micrófono» y «Permitir que las aplicaciones de escritorio accedan al micrófono», y volvé a intentar";

/// WASAPI reports a microphone blocked in the privacy settings as
/// E_ACCESSDENIED, which cpal files under a generic backend error. Its text
/// carries the HRESULT, as hex or as the signed OS error code.
#[cfg_attr(not(windows), allow(dead_code))]
fn access_denied(error: &cpal::Error) -> bool {
    let text = error.to_string();
    text.contains("0x80070005") || text.contains("-2147024891")
}

/// Opening/starting a stream failed.
fn open_error(error: &cpal::Error, lost: &str) -> String {
    use cpal::ErrorKind::*;
    match error.kind() {
        DeviceNotAvailable | StreamInvalidated => lost.into(),
        PermissionDenied => PERMISSION_DENIED.into(),
        _ if cfg!(windows) && access_denied(error) => PERMISSION_DENIED.into(),
        DeviceBusy => "El micrófono está ocupado por otra aplicación. Cerrala o elegí otro en Ajustes > Micrófono".into(),
        HostUnavailable => format!("El servidor de audio del sistema no responde: {error}"),
        UnsupportedConfig => format!("El micrófono no ofrece un formato de audio compatible: {error}"),
        _ => format!("Audio del sistema: {error}"),
    }
}
/// Errors reported while capturing. Xruns, real-time refusals and default
/// device reroutes are informational; everything else stops capture.
fn runtime_error(error: &cpal::Error, lost: &str) -> Option<String> {
    use cpal::ErrorKind::*;
    match error.kind() {
        Xrun | RealtimeDenied | DeviceChanged => None,
        _ => Some(open_error(error, lost)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, hz: f32, len: usize) -> Vec<f32> {
        (0..len)
            .map(|i| 0.5 * (2.0 * std::f32::consts::PI * hz * i as f32 / rate as f32).sin())
            .collect()
    }
    fn samples(bytes: &[u8]) -> Vec<f32> {
        bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| f32::from(i16::from_le_bytes(*b)) / 32768.0)
            .collect()
    }
    fn convert(rate: u32, input: &[f32], piece: usize) -> Vec<f32> {
        let mut to16k = To16k::new(rate).unwrap();
        let mut bytes = Vec::new();
        for part in input.chunks(piece) {
            to16k.push(part, &mut bytes);
        }
        to16k.finish(&mut bytes);
        samples(&bytes)
    }

    #[test]
    fn downmix_averages_channels_and_converts_formats() {
        assert_eq!(downmix(&[0.5f32, -0.25]), 0.125);
        assert_eq!(downmix(&[16384i16]), 0.5);
        assert_eq!(downmix(&[i16::MIN, i16::MIN]), -1.0);
        assert_eq!(downmix(&[128u8 + 64, 128u8 + 64, 128, 128]), 0.25);
        let (mut producer, mut consumer) = rtrb::RingBuffer::new(8);
        // Three stereo frames; a trailing half frame is ignored.
        assert!(push_downmixed(
            &[0.2f32, 0.4, -1.0, 1.0, 0.5, 0.5, 0.9],
            2,
            &mut producer
        ));
        let mono: Vec<f32> = std::iter::from_fn(|| consumer.pop().ok()).collect();
        assert_eq!(mono.len(), 3);
        assert!((mono[0] - 0.3).abs() < 1e-6);
        assert_eq!(&mono[1..], &[0.0, 0.5]);
    }

    #[test]
    fn resampling_48k_to_16k_keeps_length_alignment_and_frequency() {
        let input = sine(48_000, 1_000.0, 48_000);
        // Callback-sized pieces that do not divide the 20 ms resampler block.
        let output = convert(48_000, &input, 441);
        assert_eq!(output.len(), 16_000);
        let middle = &output[400..15_600];
        let rms = (middle.iter().map(|v| v * v).sum::<f32>() / middle.len() as f32).sqrt();
        assert!((rms / (0.5 / 2f32.sqrt()) - 1.0).abs() < 0.01, "rms {rms}");
        // Filter delay trimmed: best alignment with the ideal 16 kHz sine is
        // lag 0 (the FFT leaves at most a sub-sample offset).
        let ideal = sine(16_000, 1_000.0, 16_000);
        let correlation =
            |lag: usize| -> f32 { (400..15_600).map(|i| output[i] * ideal[i + lag - 2]).sum() };
        let best = (0..=4usize)
            .max_by(|a, b| correlation(*a).total_cmp(&correlation(*b)))
            .unwrap();
        assert_eq!(best, 2, "lag {}", best as i32 - 2);
        let rising = (1..output.len())
            .filter(|&i| output[i - 1] <= 0.0 && output[i] > 0.0)
            .count();
        assert!((rising as i64 - 1_000).abs() <= 1, "{rising} Hz");
    }

    #[test]
    fn resampling_odd_rates_rounds_the_length() {
        for (rate, len) in [(44_100u32, 44_100usize), (22_050, 7_001), (96_000, 12_345)] {
            let input = sine(rate, 440.0, len);
            let output = convert(rate, &input, 512);
            let expected = (len as f64 * 16_000.0 / f64::from(rate)).round() as usize;
            assert_eq!(output.len(), expected, "{rate} Hz");
        }
    }

    #[test]
    fn sixteen_khz_is_a_bit_exact_passthrough() {
        let pcm: Vec<i16> = (0..4_000)
            .map(|i| ((i * 7919) % 65_536 - 32_768) as i16)
            .collect();
        let floats: Vec<f32> = pcm.iter().map(|s| s.to_sample::<f32>()).collect();
        let mut to16k = To16k::new(16_000).unwrap();
        let mut bytes = Vec::new();
        to16k.push(&floats, &mut bytes);
        to16k.finish(&mut bytes);
        let expected: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
        assert_eq!(bytes, expected);
    }

    #[test]
    fn ring_overflow_stops_capture_with_an_error_instead_of_a_gap() {
        let (mut producer, consumer) = rtrb::RingBuffer::new(100);
        let overflow = Arc::new(AtomicBool::new(false));
        let mut pump = Pump::new(consumer, overflow.clone(), 16_000).unwrap();
        let (tx, _rx) = mpsc::channel(8);
        assert!(push_downmixed(&[0i16; 60], 1, &mut producer));
        // No room for 60 more: nothing is written, not even the 40 that fit.
        assert!(!push_downmixed(&[0i16; 60], 1, &mut producer));
        assert_eq!(producer.slots(), 40);
        overflow.store(true, Ordering::Release);
        let error = pump.drain(&tx).unwrap_err().to_string();
        assert!(error.contains("no perder palabras"), "{error}");
        assert!(pump.finish(&tx).is_err());
    }

    #[test]
    fn chunking_matches_the_pipewire_path() {
        // Same PCM as audio::tests::chunks_preserve_pcm_and_flush_tail.
        let pcm: Vec<i16> = (0..3500i16).collect();
        let bytes: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
        let (tx, mut rx) = mpsc::channel(8);
        let mut pipewire = PcmBuffer::default();
        for part in bytes.chunks(222) {
            pipewire.push(part, &tx).unwrap();
        }
        pipewire.flush(&tx).unwrap();
        let mut expected = Vec::new();
        while let Ok(packet) = rx.try_recv() {
            expected.push(packet);
        }

        // PulseAudio delivers 16 kHz mono i16 in callback-sized pieces.
        let (mut producer, consumer) = rtrb::RingBuffer::new(32_000);
        let mut pump = Pump::new(consumer, Arc::new(AtomicBool::new(false)), 16_000).unwrap();
        let mut levels = Vec::new();
        for part in pcm.chunks(111) {
            assert!(push_downmixed(part, 1, &mut producer));
            levels.extend(pump.drain(&tx).unwrap());
        }
        pump.finish(&tx).unwrap();
        let mut packets = Vec::new();
        while let Ok(packet) = rx.try_recv() {
            packets.push(packet);
        }
        assert_eq!(packets, expected);
        assert_eq!(
            packets.iter().map(Vec::len).collect::<Vec<_>>(),
            vec![3200, 3200, 600]
        );
        assert_eq!(levels.len(), pcm.chunks(111).count());
        assert!(levels.iter().all(|level| (0.0..=1.0).contains(level)));
    }

    #[test]
    fn device_errors_are_actionable_and_xruns_are_not_fatal() {
        let lost = cpal::Error::new(cpal::ErrorKind::DeviceNotAvailable);
        assert_eq!(
            runtime_error(&lost, CHOSEN_LOST).as_deref(),
            Some(CHOSEN_LOST)
        );
        let denied = cpal::Error::new(cpal::ErrorKind::PermissionDenied);
        assert!(open_error(&denied, DEFAULT_LOST).contains("privacidad"));
        // WASAPI's E_ACCESSDENIED (privacy switch off) arrives as a backend error.
        let blocked = cpal::Error::with_message(
            cpal::ErrorKind::BackendError,
            "Acceso denegado. (os error -2147024891)",
        );
        assert!(access_denied(&blocked));
        assert!(!access_denied(&cpal::Error::with_message(
            cpal::ErrorKind::BackendError,
            "otro fallo (os error 2)"
        )));
        #[cfg(windows)]
        assert!(open_error(&blocked, DEFAULT_LOST).contains("Privacidad y seguridad > Micrófono"));
        for kind in [
            cpal::ErrorKind::Xrun,
            cpal::ErrorKind::DeviceChanged,
            cpal::ErrorKind::RealtimeDenied,
        ] {
            assert_eq!(runtime_error(&cpal::Error::new(kind), CHOSEN_LOST), None);
        }
        assert!(is_monitor(
            "alsa_output.pci-0000_08_00.6.analog-stereo.monitor"
        ));
        assert!(!is_monitor("alsa_input.pci-0000_08_00.6.analog-stereo"));
    }
}
