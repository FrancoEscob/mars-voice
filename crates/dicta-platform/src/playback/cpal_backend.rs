//! cpal shell of the read-aloud player (WASAPI, CoreAudio, PulseAudio/ALSA).
//!
//! The player thread is a normal-priority render thread: it runs [`Engine`] in
//! 10 ms blocks and keeps 30–40 ms of mono f32 queued in a lock-free `rtrb`
//! ring (more only if the device asks for larger buffers). The cpal callback
//! just pops, converts the sample format and copies to every channel: no locks
//! and no allocation on the real-time thread.
//!
//! Position. The engine's read head runs ahead of the speaker by the ring fill
//! (plus the resampler delay). Each rendered block leaves a [`Marker`] tagged
//! with the device frame where it ends; the marker is published only once the
//! callback has consumed that frame, so `position_samples`, `ended` and
//! `starved` describe what is heard, and a seek/reset invalidates older ones.
//!
//! Rate. PulseAudio and WASAPI convert on the server, so the stream opens mono
//! at the TTS rate. Elsewhere (CoreAudio sets the hardware format, ALSA may be
//! a raw device) it opens the device's current format and this thread
//! resamples with rubato's fixed-ratio FFT resampler; the callback upmixes.
//!
//! Idle release matches PipeWire: after ≈1 s of silence (≈5 s while waiting
//! for generated audio) the stream is dropped, and play/append rebuild it.
use super::{
    CHOSEN_OUTPUT_LOST, Control, DEFAULT_OUTPUT_LOST, Engine, IDLE_TICK, IDLE_TICKS_STARVED,
    IDLE_TICKS_STOPPED, Marker, PlayerConfig, Signal,
};
use crate::audio::{default_host, stable_id};
use anyhow::{Context, Result, anyhow};
use cpal::{
    FromSample, Sample, SampleFormat, SizedSample,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use dicta_core::AudioDevice;
use rubato::{Fft, FixedSync, Resampler, audioadapter_buffers::direct::InterleavedSlice};
use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

/// Audio kept queued ahead of the device callback.
const TARGET_SECONDS: f64 = 0.035;
/// Ring capacity; only devices with very large callbacks use more than the target.
const RING_SECONDS: f64 = 0.25;
/// Render block, in seconds of TTS audio.
const BLOCK_SECONDS: f64 = 0.01;
/// How often the render thread tops the ring up while a stream is open.
const RENDER_TICK: Duration = Duration::from_millis(5);

pub(super) fn enumerate_outputs() -> Result<Vec<AudioDevice>> {
    let devices = default_host()
        .output_devices()
        .map_err(|e| anyhow!("No se pudieron consultar las salidas de audio del sistema: {e}"))?
        .filter_map(|device| crate::audio::describe(&device))
        .collect();
    Ok(crate::audio::sorted(devices))
}

fn find_output(host: &cpal::Host, id: &str) -> Option<cpal::Device> {
    host.output_devices()
        .ok()?
        .find(|device| stable_id(device).as_deref() == Some(id))
}

pub(super) fn playback_loop(
    config: PlayerConfig,
    control: Control,
    signals: mpsc::Receiver<Signal>,
    ready: mpsc::SyncSender<Result<(), String>>,
) -> Result<()> {
    let host = default_host();
    let chosen = config.device.clone();
    let lost = if chosen.is_some() {
        CHOSEN_OUTPUT_LOST
    } else {
        DEFAULT_OUTPUT_LOST
    };
    // A missing chosen output fails here, before any stream exists.
    if let Some(id) = &chosen {
        find_output(&host, id).context(CHOSEN_OUTPUT_LOST)?;
    }
    let mut engine = Engine::new(control.clone(), config.sample_rate);
    engine.deferred = true;
    let _ = ready.send(Ok(()));
    let mut output: Option<Output> = None;
    let mut idle_ticks = 0u32;
    let mut heard = false;
    let mut next_tick = Instant::now() + IDLE_TICK;
    loop {
        let wait = if output.is_some() {
            RENDER_TICK
        } else {
            IDLE_TICK
        };
        match signals.recv_timeout(wait) {
            Ok(Signal::Quit) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Ok(Signal::Wake) => {
                idle_ticks = 0;
                if output.is_none() && control.lock().playing {
                    let opened = Output::open(
                        &host,
                        chosen.as_deref(),
                        config.sample_rate,
                        lost,
                        &mut engine,
                    )
                    .map_err(|message| anyhow!(message))?;
                    output = Some(opened);
                    next_tick = Instant::now() + IDLE_TICK;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        let Some(out) = output.as_mut() else {
            continue;
        };
        if let Some(message) = out.failure(lost) {
            return Err(anyhow!(message));
        }
        heard |= out.feeder.fill(&mut engine, &control);
        if Instant::now() >= next_tick {
            next_tick = Instant::now() + IDLE_TICK;
            if heard {
                idle_ticks = 0;
            } else {
                let waiting = {
                    let s = control.lock();
                    s.playing && !s.ended
                };
                let limit = if waiting {
                    IDLE_TICKS_STARVED
                } else {
                    IDLE_TICKS_STOPPED
                };
                idle_ticks += 1;
                if idle_ticks >= limit {
                    idle_ticks = 0;
                    if let Some(out) = output.take() {
                        out.close(&control);
                    }
                }
            }
            heard = false;
        }
    }
    Ok(())
}

/// An open device stream and the render side of its ring.
struct Output {
    stream: cpal::Stream,
    feeder: Feeder,
    errors: mpsc::Receiver<cpal::Error>,
}

struct Format {
    config: cpal::StreamConfig,
    sample_format: SampleFormat,
}

impl Output {
    fn open(
        host: &cpal::Host,
        chosen: Option<&str>,
        tts_rate: u32,
        lost: &str,
        engine: &mut Engine,
    ) -> Result<Self, String> {
        let device = match chosen {
            Some(id) => find_output(host, id).ok_or(CHOSEN_OUTPUT_LOST)?,
            None => host.default_output_device().ok_or(
                "No hay una salida de audio disponible. Conectá una o elegí otra en Ajustes > Lectura",
            )?,
        };
        let mut last_error = None;
        for format in formats(host, &device, tts_rate) {
            match Self::open_format(&device, &format, tts_rate, engine) {
                Ok(output) => return Ok(output),
                Err(error) if error.kind() == cpal::ErrorKind::DeviceNotAvailable => {
                    return Err(lost.into());
                }
                Err(error) => last_error = Some(error),
            }
        }
        let error = match last_error {
            Some(error) => error,
            None => device
                .default_output_config()
                .err()
                .unwrap_or_else(|| cpal::Error::new(cpal::ErrorKind::UnsupportedConfig)),
        };
        Err(output_error(&error, lost).unwrap_or_else(|| lost.into()))
    }

    fn open_format(
        device: &cpal::Device,
        format: &Format,
        tts_rate: u32,
        engine: &mut Engine,
    ) -> Result<Self, cpal::Error> {
        let rate = format.config.sample_rate;
        let channels = usize::from(format.config.channels).max(1);
        let capacity = ((f64::from(rate) * RING_SECONDS).ceil() as usize).max(1);
        let (producer, consumer) = rtrb::RingBuffer::new(capacity);
        let consumed = Arc::new(AtomicU64::new(0));
        let (errors_tx, errors) = mpsc::channel();
        let stream = build_output(
            device,
            format,
            channels,
            consumer,
            consumed.clone(),
            errors_tx,
        )?;
        let period = stream.buffer_size().unwrap_or(0) as usize;
        let mut feeder = Feeder::new(producer, consumed, tts_rate, rate, period).map_err(|e| {
            cpal::Error::with_message(cpal::ErrorKind::UnsupportedConfig, e.to_string())
        })?;
        // Start first: if the device refuses, the engine has not rendered
        // audio nobody will hear. The render thread fills the ring right away.
        stream.play()?;
        let control = engine.control.clone();
        feeder.fill(engine, &control);
        Ok(Self {
            stream,
            feeder,
            errors,
        })
    }

    /// First fatal error reported by the device since the last call.
    fn failure(&self, lost: &str) -> Option<String> {
        self.errors.try_iter().find_map(|e| output_error(&e, lost))
    }

    /// Idle release: the ring holds only silence by now, so the latest
    /// rendered state is the audible one.
    fn close(mut self, control: &Control) {
        drop(self.stream);
        self.feeder.publish_latest(control);
    }
}

/// Mono f32 at the TTS rate first where the server converts; then the
/// device's current format (never forcing a hardware rate change).
fn formats(host: &cpal::Host, device: &cpal::Device, tts_rate: u32) -> Vec<Format> {
    let mut formats = Vec::with_capacity(2);
    let pulse = crate::audio::is_pulse(host);
    if pulse || cfg!(windows) {
        formats.push(Format {
            config: cpal::StreamConfig {
                channels: 1,
                sample_rate: tts_rate,
                // PulseAudio's default buffer is ≈2 s; ask for 20 ms periods.
                buffer_size: if pulse {
                    cpal::BufferSize::Fixed(tts_rate / 50)
                } else {
                    cpal::BufferSize::Default
                },
            },
            sample_format: SampleFormat::F32,
        });
    }
    if let Ok(default) = device.default_output_config() {
        formats.push(Format {
            config: default.config(),
            sample_format: default.sample_format(),
        });
    }
    formats
}

fn build_output(
    device: &cpal::Device,
    format: &Format,
    channels: usize,
    ring: rtrb::Consumer<f32>,
    consumed: Arc<AtomicU64>,
    errors: mpsc::Sender<cpal::Error>,
) -> Result<cpal::Stream, cpal::Error> {
    let config = format.config;
    let args = (ring, consumed, errors);
    match format.sample_format {
        SampleFormat::I8 => build::<i8>(device, config, channels, args),
        SampleFormat::I16 => build::<i16>(device, config, channels, args),
        SampleFormat::I24 => build::<cpal::I24>(device, config, channels, args),
        SampleFormat::I32 => build::<i32>(device, config, channels, args),
        SampleFormat::I64 => build::<i64>(device, config, channels, args),
        SampleFormat::U8 => build::<u8>(device, config, channels, args),
        SampleFormat::U16 => build::<u16>(device, config, channels, args),
        SampleFormat::U24 => build::<cpal::U24>(device, config, channels, args),
        SampleFormat::U32 => build::<u32>(device, config, channels, args),
        SampleFormat::U64 => build::<u64>(device, config, channels, args),
        SampleFormat::F32 => build::<f32>(device, config, channels, args),
        SampleFormat::F64 => build::<f64>(device, config, channels, args),
        other => Err(cpal::Error::with_message(
            cpal::ErrorKind::UnsupportedConfig,
            format!("formato {other}"),
        )),
    }
}

fn build<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    channels: usize,
    (mut ring, consumed, errors): (
        rtrb::Consumer<f32>,
        Arc<AtomicU64>,
        mpsc::Sender<cpal::Error>,
    ),
) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample + FromSample<f32>,
{
    device.build_output_stream::<T, _, _>(
        config,
        move |data: &mut [T], _| play_out(&mut ring, &consumed, data, channels),
        move |error| {
            let _ = errors.send(error);
        },
        Some(crate::audio::OPEN_TIMEOUT),
    )
}

/// Real-time callback body: pop mono frames, write them to every channel,
/// silence on underrun. Lock-free and allocation-free.
fn play_out<T>(
    ring: &mut rtrb::Consumer<f32>,
    consumed: &AtomicU64,
    data: &mut [T],
    channels: usize,
) where
    T: Sample + FromSample<f32>,
{
    let mut frames = data.chunks_exact_mut(channels);
    let available = ring.slots().min(frames.len());
    if let Ok(chunk) = ring.read_chunk(available) {
        let (first, second) = chunk.as_slices();
        for (sample, frame) in first.iter().chain(second).zip(&mut frames) {
            frame.fill(T::from_sample(*sample));
        }
        chunk.commit_all();
    }
    for frame in frames {
        frame.fill(T::EQUILIBRIUM);
    }
    consumed.fetch_add(available as u64, Ordering::Release);
}

/// Render side of the ring. Independent of cpal, so it is testable.
struct Feeder {
    ring: rtrb::Producer<f32>,
    /// Frames the callback has popped, ever.
    consumed: Arc<AtomicU64>,
    /// Frames pushed, ever.
    written: u64,
    /// Device frames to keep queued.
    target: usize,
    block: Vec<f32>,
    convert: Option<RateConverter>,
    /// Device frames between a rendered sample and its place in the ring.
    delay: u64,
    /// (device frame where the block ends, state after rendering it)
    markers: VecDeque<(u64, Marker)>,
}

impl Feeder {
    fn new(
        ring: rtrb::Producer<f32>,
        consumed: Arc<AtomicU64>,
        tts_rate: u32,
        device_rate: u32,
        period: usize,
    ) -> Result<Self> {
        let capacity = ring.buffer().capacity();
        let convert = (tts_rate != device_rate)
            .then(|| RateConverter::new(tts_rate, device_rate))
            .transpose()?;
        let block_len = match &convert {
            Some(convert) => convert.block,
            None => ((f64::from(tts_rate) * BLOCK_SECONDS).round() as usize).max(1),
        };
        let block_out = convert.as_ref().map_or(block_len, |c| c.max_out());
        let wanted = ((f64::from(device_rate) * TARGET_SECONDS).ceil() as usize).max(2 * period);
        let target = wanted.min(capacity.saturating_sub(block_out)).max(1);
        Ok(Self {
            ring,
            consumed,
            written: 0,
            target,
            block: vec![0.0; block_len],
            delay: convert.as_ref().map_or(0, |c| c.delay() as u64),
            convert,
            markers: VecDeque::with_capacity(capacity / block_out.max(1) + 4),
        })
    }

    fn queued(&self) -> u64 {
        self.written - self.consumed.load(Ordering::Acquire).min(self.written)
    }

    /// Publishes what the device has played, then renders until `target`
    /// frames are queued. Returns true if anything audible was rendered.
    fn fill(&mut self, engine: &mut Engine, control: &Control) -> bool {
        self.publish(control);
        let mut heard = false;
        while (self.queued() as usize) < self.target {
            engine.render(&mut self.block);
            heard |= !engine.quiet();
            let out = match &mut self.convert {
                Some(convert) => convert.process(&self.block),
                None => &self.block[..],
            };
            // Room is guaranteed: target ≤ capacity − largest block.
            let pushed = match self.ring.write_chunk_uninit(out.len()) {
                Ok(chunk) => chunk.fill_from_iter(out.iter().copied()),
                Err(_) => 0,
            };
            self.written += pushed as u64;
            self.markers
                .push_back((self.written + self.delay, engine.marker));
            if pushed < out.len() {
                break;
            }
        }
        heard
    }

    fn publish(&mut self, control: &Control) {
        let consumed = self.consumed.load(Ordering::Acquire);
        let mut heard = None;
        while let Some(&(at, marker)) = self.markers.front() {
            if at > consumed {
                break;
            }
            heard = Some(marker);
            self.markers.pop_front();
        }
        if let Some(marker) = heard {
            marker.apply(&mut control.lock());
        }
    }

    fn publish_latest(&mut self, control: &Control) {
        if let Some((_, marker)) = self.markers.drain(..).next_back() {
            marker.apply(&mut control.lock());
        }
    }
}

/// TTS rate → device rate, fixed ratio. Blocks are a multiple of the smallest
/// exact FFT block when that is ≤ 100 ms, so the delay is constant.
struct RateConverter {
    fft: Fft<f32>,
    block: usize,
    output: Vec<f32>,
}
impl RateConverter {
    fn new(from: u32, to: u32) -> Result<Self> {
        let (from, to) = (from as usize, to as usize);
        let gcd = gcd(from, to);
        let unit = from / gcd;
        let nominal = ((from as f64 * BLOCK_SECONDS).round() as usize).max(1);
        let block = if unit <= from / 10 {
            nominal.div_ceil(unit) * unit
        } else {
            nominal
        };
        let fft = Fft::<f32>::new(from, to, block, 1, FixedSync::Input)
            .map_err(|e| anyhow!("No se pudo convertir la lectura de {from} Hz a {to} Hz: {e}"))?;
        let output = vec![0.0; fft.output_frames_max()];
        Ok(Self { fft, block, output })
    }
    fn max_out(&self) -> usize {
        self.output.len()
    }
    fn delay(&self) -> usize {
        self.fft.output_delay()
    }
    fn process(&mut self, input: &[f32]) -> &[f32] {
        let capacity = self.output.len();
        let written = InterleavedSlice::new(input, 1, input.len())
            .ok()
            .zip(InterleavedSlice::new_mut(&mut self.output[..], 1, capacity).ok())
            .and_then(|(input, mut output)| {
                self.fft.process_into_buffer(&input, &mut output, None).ok()
            })
            // Buffers are sized from the resampler itself; this cannot fail.
            .map_or(0, |(_, written)| written);
        &self.output[..written]
    }
}
fn gcd(a: usize, b: usize) -> usize {
    if b == 0 { a.max(1) } else { gcd(b, a % b) }
}

/// Device errors while playing. None for informational ones (xruns,
/// real-time refusal, a default output that was followed to a new device).
fn output_error(error: &cpal::Error, lost: &str) -> Option<String> {
    use cpal::ErrorKind::*;
    Some(match error.kind() {
        Xrun | RealtimeDenied | DeviceChanged => return None,
        DeviceNotAvailable | StreamInvalidated => lost.into(),
        DeviceBusy => "La salida de audio está ocupada por otra aplicación. Cerrala o elegí otra en Ajustes > Lectura".into(),
        PermissionDenied => "El sistema negó el acceso a la salida de audio".into(),
        HostUnavailable => format!("El servidor de audio del sistema no responde: {error}"),
        UnsupportedConfig => format!("La salida de audio no acepta un formato compatible: {error}"),
        _ => format!("Salida de audio: {error}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 24_000;

    fn setup(
        device_rate: u32,
        period: usize,
    ) -> (Control, Engine, Feeder, rtrb::Consumer<f32>, Arc<AtomicU64>) {
        let control = Control::new(1.0, 1.0);
        let mut engine = Engine::new(control.clone(), SR);
        engine.deferred = true;
        let capacity = (f64::from(device_rate) * RING_SECONDS).ceil() as usize;
        let (producer, consumer) = rtrb::RingBuffer::new(capacity);
        let consumed = Arc::new(AtomicU64::new(0));
        let feeder = Feeder::new(producer, consumed.clone(), SR, device_rate, period).unwrap();
        (control, engine, feeder, consumer, consumed)
    }
    /// The device callback, `frames` at a time, `channels` wide.
    fn device(
        ring: &mut rtrb::Consumer<f32>,
        consumed: &AtomicU64,
        frames: usize,
        channels: usize,
    ) -> Vec<f32> {
        let mut data = vec![9.0f32; frames * channels];
        play_out(ring, consumed, &mut data, channels);
        data
    }
    fn ramp(len: usize) -> Vec<i16> {
        (0..len).map(|i| (i % 20_000) as i16).collect()
    }

    #[test]
    fn ring_holds_30_to_40_ms_and_position_follows_what_is_heard() {
        let (control, mut engine, mut feeder, mut ring, consumed) = setup(SR, 240);
        control.append(&ramp(SR as usize));
        control.play();
        assert!(feeder.fill(&mut engine, &control));
        let queued = feeder.queued() as f64 / f64::from(SR);
        assert!((0.03..=0.045).contains(&queued), "{queued} s queued");
        // Rendered ahead, but nothing heard yet.
        assert!(engine.pos >= 840);
        assert_eq!(control.status().position_samples, 0);
        let out = device(&mut ring, &consumed, 480, 1);
        feeder.fill(&mut engine, &control);
        assert_eq!(control.status().position_samples, 480);
        // After the 8 ms fade-in the device gets the exact PCM.
        assert_eq!(out[479], f32::from(479i16) / 32768.0);
        // The callback never blocks: an underrun is silence, counted apart.
        let drained = device(&mut ring, &consumed, 4_800, 1);
        assert!(drained[4_700..].iter().all(|v| *v == 0.0));
    }

    #[test]
    fn seek_reports_the_target_until_new_audio_is_heard() {
        let (control, mut engine, mut feeder, mut ring, consumed) = setup(SR, 240);
        control.append(&ramp(SR as usize * 2));
        control.play();
        feeder.fill(&mut engine, &control);
        device(&mut ring, &consumed, 480, 1);
        feeder.fill(&mut engine, &control);
        control.seek(30_000);
        assert_eq!(control.status().position_samples, 30_000);
        // Old audio still in the ring plays out without moving the position back.
        let queued = feeder.queued() as usize;
        device(&mut ring, &consumed, queued, 1);
        feeder.fill(&mut engine, &control);
        assert_eq!(control.status().position_samples, 30_000);
        device(&mut ring, &consumed, 480, 1);
        feeder.fill(&mut engine, &control);
        let position = control.status().position_samples;
        assert!(position > 30_000 && position <= 30_480, "{position}");
    }

    #[test]
    fn ended_waits_until_the_last_sample_is_played() {
        let (control, mut engine, mut feeder, mut ring, consumed) = setup(SR, 240);
        control.append(&ramp(2_400));
        control.set_finished(true);
        control.play();
        // Rendering reaches the end long before the device does.
        feeder.fill(&mut engine, &control);
        device(&mut ring, &consumed, 960, 1);
        feeder.fill(&mut engine, &control);
        device(&mut ring, &consumed, 960, 1);
        feeder.fill(&mut engine, &control);
        assert!(engine.pos >= 2_400);
        let status = control.status();
        assert!(
            !status.ended && status.position_samples == 1_920,
            "{status:?}"
        );
        device(&mut ring, &consumed, 480, 1);
        feeder.fill(&mut engine, &control);
        let status = control.status();
        assert!(status.ended, "{status:?}");
        assert_eq!(status.position_samples, 2_400);
    }

    #[test]
    fn resampled_output_accounts_for_the_converter_delay() {
        let (control, mut engine, mut feeder, mut ring, consumed) = setup(48_000, 480);
        let delay = feeder.delay;
        assert!(delay > 0);
        control.append(&ramp(SR as usize));
        control.play();
        feeder.fill(&mut engine, &control);
        let queued = feeder.queued() as f64 / 48_000.0;
        assert!((0.03..=0.045).contains(&queued), "{queued} s queued");
        // 240-frame callbacks until the delay plus 200 ms have been played.
        let mut out = Vec::new();
        for _ in 0..(9_600 + delay as usize) / 240 {
            out.extend(device(&mut ring, &consumed, 240, 2));
            feeder.fill(&mut engine, &control);
        }
        assert_eq!(consumed.load(Ordering::Acquire), 9_600 + delay);
        // 9600 device frames at 48 kHz after the delay = 4800 TTS samples.
        let position = control.status().position_samples;
        assert_eq!(position, 4_800);
        // Upmixed: both channels carry the same sample.
        assert!(out.as_chunks::<2>().0.iter().all(|[l, r]| l == r));
        assert!(out.iter().any(|v| v.abs() > 0.01));
    }

    #[test]
    fn rate_converter_uses_exact_blocks_for_common_rates() {
        for (from, to, block) in [
            (24_000, 48_000, 240),
            (24_000, 44_100, 240),
            (22_050, 48_000, 294),
            (8_000, 48_000, 80),
        ] {
            let mut convert = RateConverter::new(from, to).unwrap();
            assert_eq!(convert.block, block, "{from}->{to}");
            let input = vec![0.25f32; block];
            let mut total = 0;
            for _ in 0..10 {
                let out = convert.process(&input).len();
                assert_eq!(out * from as usize, block * to as usize, "{from}->{to}");
                total += out;
            }
            assert_eq!(total, 10 * block * to as usize / from as usize);
        }
    }

    #[test]
    fn device_errors_map_to_reading_settings_hints() {
        let lost = cpal::Error::new(cpal::ErrorKind::DeviceNotAvailable);
        assert_eq!(
            output_error(&lost, CHOSEN_OUTPUT_LOST).as_deref(),
            Some(CHOSEN_OUTPUT_LOST)
        );
        let invalid = cpal::Error::new(cpal::ErrorKind::StreamInvalidated);
        assert_eq!(
            output_error(&invalid, DEFAULT_OUTPUT_LOST).as_deref(),
            Some(DEFAULT_OUTPUT_LOST)
        );
        for kind in [cpal::ErrorKind::Xrun, cpal::ErrorKind::DeviceChanged] {
            assert_eq!(
                output_error(&cpal::Error::new(kind), CHOSEN_OUTPUT_LOST),
                None
            );
        }
    }
}
