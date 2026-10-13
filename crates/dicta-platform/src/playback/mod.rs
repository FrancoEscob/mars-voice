//! Read-aloud playback: an in-process OUTPUT stream on its own thread, fed from
//! an in-memory PCM timeline that keeps growing while it plays.
//!
//! Backends, chosen when the player opens: native PipeWire on Linux when
//! PipeWire is the audio server (feature `pipewire-native`, [`native`]), cpal
//! everywhere else ([`cpal_backend`]: WASAPI, CoreAudio, PulseAudio/ALSA).
//!
//! Threading. [`Player`] methods take `&self`, never wait for the audio thread
//! and only hold a mutex for a copy or a flag. The audio thread renders each
//! buffer through [`Engine`], a pure DSP state machine that holds the same mutex
//! for one buffer (≈20 ms of audio, far below 1 ms of CPU) and never allocates
//! there. Controller mutations (`seek`, `reset`, `set_finished`, `pause`) update
//! the published status immediately; the audible change follows within one
//! buffer plus an 8 ms fade. With cpal the engine runs on a normal-priority
//! render thread that keeps a 30–40 ms lock-free ring ahead of the device
//! callback; its position/ended status is published when the audio is heard.
//!
//! Speed. Rate 1.0 bypasses the stretcher (bit-exact apart from volume and
//! fades). Other rates use Signalsmith Stretch (C++, MIT, via the
//! `signalsmith-stretch` crate), chosen over the pure-Rust `wsola` crate after a
//! spike: on recorded speech its log-spectral distance to the time-mapped
//! original was 7.0–8.8 dB versus 8.8–12.0 dB for WSOLA (0.5×–2.5×, worst gap at
//! 2.5×) and it ran ≈3× faster. Its build needs a C++ compiler plus libclang for
//! bindgen, which the PipeWire bindings already require (`CLANG_PATH`).
//! Switching between bypass and stretching restarts at the current position
//! with a short fade-out/fade-in, so it never clicks.
use anyhow::{Result, anyhow, ensure};
use dicta_core::AudioDevice;
use std::{
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

mod cpal_backend;
#[cfg(all(target_os = "linux", feature = "pipewire-native"))]
mod native;

pub const MIN_RATE: f32 = 0.5;
pub const MAX_RATE: f32 = 2.5;
const FADE_SECONDS: f32 = 0.008;
const LEVEL_SECONDS: f32 = 0.02;
const DECLICK_SECONDS: f32 = 0.002;
/// Largest DSP block; PipeWire buffers are rendered in pieces of this size.
const BLOCK: usize = 1024;
/// Timeline storage unit. Appends never move already stored audio.
const CHUNK: usize = 1 << 14;
/// Used only if PipeWire does not report how many frames it wants.
#[cfg_attr(
    not(all(target_os = "linux", feature = "pipewire-native")),
    allow(dead_code)
)]
const FALLBACK_FRAMES: usize = 1024;
const IDLE_TICK: Duration = Duration::from_millis(250);
/// Paused or ended: release the device after ≈1 s of silence.
const IDLE_TICKS_STOPPED: u32 = 4;
/// Waiting for generated audio: keep the device ≈5 s before releasing it.
const IDLE_TICKS_STARVED: u32 = 20;

#[derive(Debug, Clone, PartialEq)]
pub struct PlayerConfig {
    /// Rate of the appended PCM (TTS output), 8000..=48000. PipeWire,
    /// PulseAudio and WASAPI resample; on other cpal hosts the player does.
    pub sample_rate: u32,
    /// Stable output id from [`enumerate_outputs`] (PipeWire `node.name` for
    /// `target.object`); None follows the default output.
    pub device: Option<String>,
    pub volume: f32,
    pub rate: f32,
}

#[derive(Debug, Clone, Default)]
pub struct PlayerStatus {
    /// Read head in the loaded timeline, in INPUT samples at `sample_rate`.
    /// While stretching it is the audible point (stretcher latency excluded).
    pub position_samples: u64,
    /// Total samples appended since the last reset.
    pub buffered_samples: u64,
    /// Not paused (the controller's play/pause flag).
    pub playing: bool,
    /// Playing, not finished, and out of buffered audio. While stretching the
    /// stretcher keeps ≈0.1–0.2 s of look-ahead, so `position_samples` can be
    /// that much below `buffered_samples` when this is set.
    pub starved: bool,
    /// `set_finished(true)` was called and the read head reached the end.
    pub ended: bool,
    /// Recent output RMS (after volume), 0..=1, like the capture meter.
    pub level: f32,
    /// Increments only when a new level is computed (≈50 Hz while audible,
    /// once more when output falls silent).
    pub level_seq: u64,
    /// Stream/device failure, in Spanish. The player stops rendering; create a
    /// new one to retry.
    pub error: Option<String>,
}

/// Mono s16 PCM player. Opens paused; every method is cheap and non-blocking.
/// Dropping it stops the stream and joins its thread.
pub struct Player {
    control: Control,
    signals: Signals,
    thread: Option<thread::JoinHandle<()>>,
}

#[derive(Clone, Copy)]
enum Signal {
    Wake,
    Quit,
}

/// The backend is picked on the player thread before `Player::new` returns.
/// Until then signals go to both channels; afterwards only to the chosen one
/// (a PipeWire channel would keep queueing for a dropped receiver).
struct Signals {
    #[cfg(all(target_os = "linux", feature = "pipewire-native"))]
    native: pipewire::channel::Sender<Signal>,
    portable: mpsc::Sender<Signal>,
    /// Set by the player thread when it chose cpal.
    is_portable: Arc<AtomicBool>,
}
struct SignalReceivers {
    #[cfg(all(target_os = "linux", feature = "pipewire-native"))]
    native: pipewire::channel::Receiver<Signal>,
    portable: mpsc::Receiver<Signal>,
}
impl Signals {
    fn new() -> (Self, SignalReceivers) {
        #[cfg(all(target_os = "linux", feature = "pipewire-native"))]
        let (native, native_rx) = pipewire::channel::channel();
        let (portable, portable_rx) = mpsc::channel();
        (
            Self {
                #[cfg(all(target_os = "linux", feature = "pipewire-native"))]
                native,
                portable,
                is_portable: Arc::new(AtomicBool::new(false)),
            },
            SignalReceivers {
                #[cfg(all(target_os = "linux", feature = "pipewire-native"))]
                native: native_rx,
                portable: portable_rx,
            },
        )
    }
    fn send(&self, signal: Signal) {
        #[cfg(all(target_os = "linux", feature = "pipewire-native"))]
        if !self.is_portable.load(Ordering::Acquire) {
            let _ = self.native.send(signal);
        }
        let _ = self.portable.send(signal);
    }
}

impl Player {
    /// Opens the output stream paused (inactive until [`Player::play`]).
    /// Blocks until the audio server accepted the stream (normally a few ms,
    /// at most 5 s). Device failures reported later appear in
    /// [`PlayerStatus::error`].
    pub fn new(config: PlayerConfig) -> Result<Self> {
        ensure!(
            (8_000..=48_000).contains(&config.sample_rate),
            "La frecuencia de lectura debe estar entre 8000 y 48000 Hz"
        );
        ensure!(
            config.device.as_ref().is_none_or(|d| !d.trim().is_empty()),
            "Elegí una salida de audio válida en Ajustes > Lectura"
        );
        let control = Control::new(config.rate, config.volume);
        let (signals, receivers) = Signals::new();
        let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<(), String>>(1);
        let thread_portable = signals.is_portable.clone();
        let thread_control = control.clone();
        let thread = thread::Builder::new()
            .name("dicta-playback".into())
            .spawn(move || {
                let failed = ready_tx.clone();
                if let Err(error) = run(
                    config,
                    thread_control.clone(),
                    receivers,
                    ready_tx,
                    &thread_portable,
                ) {
                    let message = format!("No se pudo reproducir la lectura: {error:#}");
                    let _ = failed.try_send(Err(message.clone()));
                    thread_control.fail(message);
                }
            })?;
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(Self {
                control,
                signals,
                thread: Some(thread),
            }),
            Ok(Err(message)) => {
                let _ = thread.join();
                Err(anyhow!(message))
            }
            Err(_) => {
                // A stalled daemon would block a join; let the thread end alone.
                signals.send(Signal::Quit);
                if cfg!(all(target_os = "linux", feature = "pipewire-native"))
                    && !signals.is_portable.load(Ordering::Acquire)
                {
                    Err(anyhow!(
                        "PipeWire no respondió al abrir la salida de audio; revisá PipeWire y volvé a intentar"
                    ))
                } else {
                    Err(anyhow!(
                        "El sistema de audio no respondió al abrir la salida de audio; revisá el dispositivo y volvé a intentar"
                    ))
                }
            }
        }
    }
    /// Clears the timeline: position 0, not finished, play/pause flag kept.
    pub fn reset(&self) {
        self.control.reset();
        self.wake();
    }
    /// Mono s16 at `sample_rate`, appended at the end. Resumes a starved,
    /// playing player automatically.
    pub fn append(&self, pcm: &[i16]) {
        self.control.append(pcm);
        self.wake();
    }
    pub fn set_finished(&self, finished: bool) {
        self.control.set_finished(finished);
        self.wake();
    }
    pub fn play(&self) {
        self.control.play();
        self.wake();
    }
    pub fn pause(&self) {
        self.control.pause();
    }
    /// Clamped to the buffered audio. Reflected by `status()` immediately.
    pub fn seek(&self, sample: u64) {
        self.control.seek(sample);
        self.wake();
    }
    /// Clamped to 0.5..=2.5; non-finite values are ignored.
    pub fn set_rate(&self, rate: f32) {
        self.control.set_rate(rate);
    }
    /// Clamped to 0..=1 and ramped over 8 ms; non-finite values are ignored.
    pub fn set_volume(&self, volume: f32) {
        self.control.set_volume(volume);
    }
    pub fn status(&self) -> PlayerStatus {
        self.control.status()
    }
    fn wake(&self) {
        self.signals.send(Signal::Wake);
    }
}
impl Drop for Player {
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            self.signals.send(Signal::Quit);
            let _ = thread.join();
        }
    }
}

/// Player thread body: native PipeWire when it is the audio server, else cpal.
fn run(
    config: PlayerConfig,
    control: Control,
    receivers: SignalReceivers,
    ready: mpsc::SyncSender<Result<(), String>>,
    portable: &AtomicBool,
) -> Result<()> {
    let SignalReceivers {
        #[cfg(all(target_os = "linux", feature = "pipewire-native"))]
            native: native_rx,
        portable: portable_rx,
    } = receivers;
    #[cfg(all(target_os = "linux", feature = "pipewire-native"))]
    {
        if crate::audio::native::serves_audio() {
            drop(portable_rx);
            return native::playback_loop(config, control, native_rx, ready);
        }
        drop(native_rx);
    }
    portable.store(true, Ordering::Release);
    cpal_backend::playback_loop(config, control, portable_rx, ready)
}

/// Outputs for the settings dropdown. Same shape as microphone enumeration:
/// ids are stable (PipeWire/PulseAudio node names, WASAPI endpoint IDs,
/// CoreAudio UIDs), sorted by display name. No stream is created.
pub fn enumerate_outputs() -> Result<Vec<AudioDevice>> {
    #[cfg(all(target_os = "linux", feature = "pipewire-native"))]
    if let Some(inventory) = crate::audio::native::audio_server_nodes(&crate::audio::native::SINKS)?
    {
        return Ok(inventory.devices);
    }
    cpal_backend::enumerate_outputs()
}

// ---------------------------------------------------------------------------
// Shared state between the controller-facing handle and the audio thread.

#[derive(Default)]
struct Timeline {
    chunks: Vec<Box<[i16]>>,
    len: u64,
}
impl Timeline {
    fn append(&mut self, mut pcm: &[i16]) {
        while !pcm.is_empty() {
            let offset = (self.len % CHUNK as u64) as usize;
            if offset == 0 {
                self.chunks.push(vec![0; CHUNK].into_boxed_slice());
            }
            let n = (CHUNK - offset).min(pcm.len());
            let chunk = self.chunks.last_mut().expect("chunk just ensured");
            chunk[offset..offset + n].copy_from_slice(&pcm[..n]);
            self.len += n as u64;
            pcm = &pcm[n..];
        }
    }
    /// Copies samples starting at `from` as f32; zero outside `0..len`.
    fn read(&self, from: i64, out: &mut [f32]) {
        let mut at = from;
        let mut i = 0;
        while i < out.len() {
            if at < 0 {
                let n = (out.len() - i).min(at.unsigned_abs().min(usize::MAX as u64) as usize);
                out[i..i + n].fill(0.0);
                i += n;
                at += n as i64;
                continue;
            }
            let pos = at as u64;
            if pos >= self.len {
                out[i..].fill(0.0);
                return;
            }
            let chunk = &self.chunks[(pos / CHUNK as u64) as usize];
            let offset = (pos % CHUNK as u64) as usize;
            let n = (CHUNK - offset)
                .min(out.len() - i)
                .min((self.len - pos).min(usize::MAX as u64) as usize);
            for (o, s) in out[i..i + n].iter_mut().zip(&chunk[offset..offset + n]) {
                *o = f32::from(*s) / 32768.0;
            }
            i += n;
            at += n as i64;
        }
    }
}

struct Shared {
    timeline: Timeline,
    finished: bool,
    playing: bool,
    rate: f32,
    volume: f32,
    /// Seek requested by the controller, consumed by the engine.
    seek: Option<u64>,
    /// Bumped by reset: the engine's state refers to discarded audio.
    epoch: u64,
    /// Bumped by seek and reset: transport states rendered before are stale.
    transport: u64,
    position: u64,
    starved: bool,
    ended: bool,
    level: f32,
    level_seq: u64,
    error: Option<String>,
}

#[derive(Clone)]
struct Control(Arc<Mutex<Shared>>);
impl Control {
    fn new(rate: f32, volume: f32) -> Self {
        Self(Arc::new(Mutex::new(Shared {
            timeline: Timeline::default(),
            finished: false,
            playing: false,
            rate: clamp_rate(rate).unwrap_or(1.0),
            volume: clamp_volume(volume).unwrap_or(1.0),
            seek: None,
            epoch: 0,
            transport: 0,
            position: 0,
            starved: false,
            ended: false,
            level: 0.0,
            level_seq: 0,
            error: None,
        })))
    }
    fn lock(&self) -> MutexGuard<'_, Shared> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
    fn reset(&self) {
        let discarded = {
            let mut s = self.lock();
            s.epoch += 1;
            s.transport += 1;
            s.seek = None;
            s.finished = false;
            s.position = 0;
            s.starved = false;
            s.ended = false;
            std::mem::take(&mut s.timeline)
        };
        // Freed outside the lock so the audio thread never waits for it.
        drop(discarded);
    }
    fn append(&self, pcm: &[i16]) {
        if !pcm.is_empty() {
            self.lock().timeline.append(pcm);
        }
    }
    fn set_finished(&self, finished: bool) {
        let mut s = self.lock();
        s.finished = finished;
        s.ended = finished && s.seek.is_none() && s.position >= s.timeline.len;
        if finished {
            s.starved = false;
        }
    }
    fn play(&self) {
        self.lock().playing = true;
    }
    fn pause(&self) {
        let mut s = self.lock();
        s.playing = false;
        s.starved = false;
    }
    fn seek(&self, sample: u64) {
        let mut s = self.lock();
        let target = sample.min(s.timeline.len);
        s.transport += 1;
        s.seek = Some(target);
        s.position = target;
        s.ended = s.finished && target >= s.timeline.len;
        s.starved = false;
    }
    fn set_rate(&self, rate: f32) {
        if let Some(rate) = clamp_rate(rate) {
            self.lock().rate = rate;
        }
    }
    fn set_volume(&self, volume: f32) {
        if let Some(volume) = clamp_volume(volume) {
            self.lock().volume = volume;
        }
    }
    fn status(&self) -> PlayerStatus {
        let s = self.lock();
        PlayerStatus {
            position_samples: s.position,
            buffered_samples: s.timeline.len,
            playing: s.playing,
            starved: s.starved,
            ended: s.ended,
            level: s.level,
            level_seq: s.level_seq,
            error: s.error.clone(),
        }
    }
    fn fail(&self, message: String) {
        let mut s = self.lock();
        s.error.get_or_insert(message);
        s.starved = false;
        if s.level != 0.0 {
            s.level = 0.0;
            s.level_seq += 1;
        }
    }
}
fn clamp_rate(rate: f32) -> Option<f32> {
    rate.is_finite().then(|| rate.clamp(MIN_RATE, MAX_RATE))
}
fn clamp_volume(volume: f32) -> Option<f32> {
    volume.is_finite().then(|| volume.clamp(0.0, 1.0))
}

// ---------------------------------------------------------------------------
// Pure DSP engine. No PipeWire, no allocation while rendering.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Direct,
    Stretch,
}
fn mode_for(rate: f32) -> Mode {
    if (rate - 1.0).abs() < 1e-4 {
        Mode::Direct
    } else {
        Mode::Stretch
    }
}
#[derive(Clone, Copy, Debug)]
enum Restart {
    At(u64),
    /// Current position (bypass/stretch switch).
    Here,
}

/// Transport status for the audio rendered so far. Published right away by
/// the PipeWire shell; the cpal shell holds it until the device plays that
/// audio, so position and `ended` never run ahead of what is heard.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Marker {
    position: u64,
    ended: bool,
    starving: bool,
    /// [`Shared::transport`] when rendered.
    transport: u64,
}
impl Marker {
    fn apply(&self, s: &mut Shared) {
        if self.transport != s.transport {
            // A seek or reset already published a newer position.
            return;
        }
        s.position = self.position;
        s.ended = self.ended && s.finished;
        s.starved = s.playing && !s.finished && self.starving;
    }
}

struct Stretcher {
    inner: signalsmith_stretch::Stretch,
    input_latency: u64,
    output_latency: usize,
}

struct Engine {
    control: Control,
    epoch: u64,
    fade_len: usize,
    declick_decay: f32,
    level_window: usize,
    mode: Mode,
    stretcher: Stretcher,
    primed: bool,
    /// Audible position in input samples.
    pos: u64,
    /// Next input sample handed to the stretcher (== pos when Direct).
    feed: u64,
    /// feed - pos while stretching.
    lag: u64,
    lag_rate: f32,
    frac: f64,
    gain: f32,
    volume: f32,
    pending: Option<Restart>,
    starving: bool,
    declick: f32,
    last: f32,
    level_sum: f64,
    level_count: usize,
    quiet: bool,
    /// Keep transport status in `marker` instead of publishing it.
    deferred: bool,
    marker: Marker,
    input: Vec<f32>,
    preroll: Vec<f32>,
    discard: Vec<f32>,
    /// PipeWire F32LE buffers only.
    #[cfg_attr(
        not(all(target_os = "linux", feature = "pipewire-native")),
        allow(dead_code)
    )]
    bytes_scratch: Vec<f32>,
}

impl Engine {
    fn new(control: Control, sample_rate: u32) -> Self {
        let rate = sample_rate as f32;
        let inner = signalsmith_stretch::Stretch::preset_default(1, sample_rate);
        let output_latency = inner.output_latency();
        let stretcher = Stretcher {
            input_latency: inner.input_latency() as u64,
            output_latency,
            inner,
        };
        let (epoch, volume, mode) = {
            let s = control.lock();
            (s.epoch, s.volume, mode_for(s.rate))
        };
        let input_len = (BLOCK.max(output_latency) as f32 * MAX_RATE).ceil() as usize + 4;
        Self {
            control,
            epoch,
            fade_len: ((rate * FADE_SECONDS).round() as usize).max(1),
            declick_decay: (-1.0 / (rate * DECLICK_SECONDS)).exp(),
            level_window: ((rate * LEVEL_SECONDS).round() as usize).max(1),
            mode,
            stretcher,
            primed: false,
            pos: 0,
            feed: 0,
            lag: 0,
            lag_rate: 1.0,
            frac: 0.0,
            gain: 0.0,
            volume,
            pending: None,
            starving: false,
            declick: 0.0,
            last: 0.0,
            level_sum: 0.0,
            level_count: 0,
            quiet: true,
            deferred: false,
            marker: Marker::default(),
            input: vec![0.0; input_len],
            // presetDefault analyses 0.12 s blocks every 0.03 s; seek() keeps
            // at most block + interval of pre-roll.
            preroll: vec![0.0; (rate * 0.15).ceil() as usize + 1],
            discard: vec![0.0; output_latency],
            bytes_scratch: vec![0.0; BLOCK * 4],
        }
    }

    /// True when the last render produced only silence.
    fn quiet(&self) -> bool {
        self.quiet
    }

    /// Fills a PipeWire F32LE mono buffer.
    #[cfg_attr(
        not(all(target_os = "linux", feature = "pipewire-native")),
        allow(dead_code)
    )]
    fn render_bytes(&mut self, bytes: &mut [u8]) {
        let mut scratch = std::mem::take(&mut self.bytes_scratch);
        let mut quiet = true;
        for part in bytes.chunks_mut(scratch.len() * 4) {
            let frames = part.len() / 4;
            self.render(&mut scratch[..frames]);
            quiet &= self.quiet;
            let (frames_out, _) = part.as_chunks_mut::<4>();
            for (dst, sample) in frames_out.iter_mut().zip(&scratch[..frames]) {
                *dst = sample.to_le_bytes();
            }
        }
        self.bytes_scratch = scratch;
        self.quiet = quiet;
    }

    fn render(&mut self, out: &mut [f32]) {
        let control = self.control.clone();
        let mut shared = control.lock();
        self.sync(&mut shared);
        let mut quiet = true;
        for block in out.chunks_mut(BLOCK) {
            quiet &= self.render_block(&mut shared, block);
        }
        self.publish(&mut shared, quiet);
        self.quiet = quiet;
    }

    fn sync(&mut self, s: &mut Shared) {
        if s.epoch != self.epoch {
            // The audio being played was discarded: smooth the cut, restart.
            self.epoch = s.epoch;
            self.declick = self.last;
            self.gain = 0.0;
            self.pending = Some(Restart::At(s.seek.take().unwrap_or(0)));
        }
        if let Some(target) = s.seek.take() {
            self.pending = Some(Restart::At(target));
        }
        let mode = mode_for(s.rate);
        if mode != self.mode && self.pending.is_none() {
            self.pending = Some(Restart::Here);
        }
        if mode == Mode::Stretch
            && self.mode == Mode::Stretch
            && self.primed
            && s.rate != self.lag_rate
        {
            // Eventual latency at the new rate; pos stays monotonic.
            self.lag = self.stretcher.input_latency + self.discard_input(s.rate) as u64;
            self.lag_rate = s.rate;
        }
    }

    fn discard_input(&self, rate: f32) -> usize {
        (self.stretcher.output_latency as f64 * f64::from(rate)).round() as usize
    }

    fn restart(&mut self, s: &Shared, restart: Restart) {
        let target = match restart {
            Restart::At(target) => target,
            Restart::Here => self.pos,
        }
        .min(s.timeline.len);
        self.pos = target;
        self.feed = target;
        self.frac = 0.0;
        self.lag = 0;
        self.primed = false;
        self.starving = false;
        self.mode = mode_for(s.rate);
    }

    /// Prime the stretcher so its next output is the audio at `pos`, mirroring
    /// Signalsmith's own `exact()`: pre-roll ending `input_latency` ahead, then
    /// drop the first `output_latency` outputs, which precede the seek point.
    fn try_prime(&mut self, s: &Shared) -> bool {
        let len = s.timeline.len;
        if s.finished && self.pos >= len {
            return false;
        }
        let discard_in = self.discard_input(s.rate);
        let ahead = self.stretcher.input_latency + discard_in as u64;
        if !s.finished && len < self.pos + ahead + 1 {
            return false;
        }
        let preroll_end = self.pos as i64 + self.stretcher.input_latency as i64;
        s.timeline
            .read(preroll_end - self.preroll.len() as i64, &mut self.preroll);
        let stretch = &mut self.stretcher.inner;
        stretch.reset();
        stretch.seek(&self.preroll, f64::from(s.rate));
        self.feed = preroll_end as u64;
        s.timeline
            .read(self.feed as i64, &mut self.input[..discard_in]);
        stretch.process(&self.input[..discard_in], &mut self.discard[..]);
        self.feed += discard_in as u64;
        self.lag = self.stretcher.input_latency + discard_in as u64;
        self.lag_rate = s.rate;
        self.frac = 0.0;
        self.primed = true;
        true
    }

    /// Output samples that can be produced from buffered audio right now. A
    /// finished timeline is zero-padded so the stretcher drains completely.
    fn available(&mut self, s: &Shared) -> usize {
        let len = s.timeline.len;
        let available = match self.mode {
            Mode::Direct => len.saturating_sub(self.feed),
            Mode::Stretch => {
                if !self.primed && !self.try_prime(s) {
                    0
                } else {
                    let limit = if s.finished { len + self.lag } else { len };
                    let input = limit.saturating_sub(self.feed) as f64 - self.frac;
                    (input.max(0.0) / f64::from(s.rate)).floor() as u64
                }
            }
        };
        if available == 0 && s.finished {
            self.pos = len;
        }
        available.min(usize::MAX as u64 / 2) as usize
    }

    /// Raw (pre-gain) audio for `out`, advancing the read head.
    fn produce(&mut self, s: &Shared, out: &mut [f32]) {
        match self.mode {
            Mode::Direct => {
                s.timeline.read(self.feed as i64, out);
                self.feed += out.len() as u64;
                self.pos = self.feed;
            }
            Mode::Stretch => {
                let total = out.len() as f64 * f64::from(s.rate) + self.frac;
                let needed = total.floor() as usize;
                self.frac = total - needed as f64;
                s.timeline.read(self.feed as i64, &mut self.input[..needed]);
                self.stretcher.inner.process(&self.input[..needed], out);
                self.feed += needed as u64;
                self.pos = self
                    .pos
                    .max(self.feed.saturating_sub(self.lag))
                    .min(s.timeline.len);
            }
        }
    }

    /// Returns true when the block is silent (nothing audible was produced).
    fn render_block(&mut self, s: &mut Shared, out: &mut [f32]) -> bool {
        let n = out.len();
        let mut i = 0;
        let mut audible = false;
        let mut declicked = false;
        while i < n {
            if self.gain <= 0.0 {
                self.gain = 0.0;
                if let Some(restart) = self.pending.take() {
                    self.restart(s, restart);
                }
            }
            let want = s.playing && self.pending.is_none();
            let available = if want || self.gain > 0.0 {
                self.available(s)
            } else {
                0
            };
            let rest = n - i;
            if want && available > self.fade_len {
                // Play freely, keeping enough audio to fade out if it runs dry.
                let m = rest.min(available - self.fade_len);
                self.produce(s, &mut out[i..i + m]);
                self.apply_gain(&mut out[i..i + m], s.volume, None);
                declicked |= self.emit(&mut out[i..i + m]);
                self.starving = false;
                audible = true;
                i += m;
            } else if self.gain > 0.0 {
                if available == 0 {
                    // Nothing left to fade with: smooth the step instead.
                    // `last` already includes any declick still decaying.
                    self.declick = self.last;
                    self.gain = 0.0;
                    continue;
                }
                // Pause, seek, rate switch or audio running out: fade to zero.
                let steps =
                    ((self.gain * self.fade_len as f32 - 1e-3).ceil() as usize).clamp(1, available);
                let m = rest.min(steps);
                self.produce(s, &mut out[i..i + m]);
                self.apply_gain(&mut out[i..i + m], s.volume, Some(steps));
                if m == steps {
                    self.gain = 0.0;
                }
                declicked |= self.emit(&mut out[i..i + m]);
                audible = true;
                i += m;
            } else if want && s.finished && available > 0 {
                // A finished tail shorter than a fade: consume it silently.
                let m = rest.min(available);
                self.produce(s, &mut out[i..i + m]);
                out[i..i + m].fill(0.0);
                declicked |= self.emit(&mut out[i..i + m]);
                i += m;
            } else {
                out[i..].fill(0.0);
                declicked |= self.emit(&mut out[i..]);
                if want && !s.finished {
                    self.starving = true;
                }
                break;
            }
        }
        let quiet = !audible && !declicked;
        if !quiet {
            for sample in out.iter() {
                self.level_sum += f64::from(*sample) * f64::from(*sample);
                self.level_count += 1;
                if self.level_count == self.level_window {
                    s.level = (self.level_sum / self.level_count as f64).sqrt() as f32;
                    s.level_seq += 1;
                    self.level_sum = 0.0;
                    self.level_count = 0;
                }
            }
        }
        quiet
    }

    /// Adds the decaying offset left by a hard cut and remembers the last
    /// emitted sample. Returns whether any offset was applied.
    fn emit(&mut self, out: &mut [f32]) -> bool {
        let declicked = self.declick != 0.0;
        for sample in out.iter_mut() {
            if self.declick != 0.0 {
                *sample += self.declick;
                self.declick *= self.declick_decay;
                if self.declick.abs() < 1e-6 {
                    self.declick = 0.0;
                }
            }
        }
        if let Some(last) = out.last() {
            self.last = *last;
        }
        declicked
    }

    /// Ramp transport gain (to 1, or to 0 over `fade_out` samples) and volume.
    fn apply_gain(&mut self, out: &mut [f32], volume: f32, fade_out: Option<usize>) {
        let step = 1.0 / self.fade_len as f32;
        let down = fade_out.map(|steps| self.gain / steps as f32);
        for sample in out.iter_mut() {
            self.gain = match down {
                Some(down) => (self.gain - down).max(0.0),
                None => (self.gain + step).min(1.0),
            };
            self.volume = if self.volume < volume {
                (self.volume + step).min(volume)
            } else {
                (self.volume - step).max(volume)
            };
            *sample *= self.gain * self.volume;
        }
    }

    fn publish(&mut self, s: &mut Shared, quiet: bool) {
        let marker = Marker {
            position: match self.pending {
                Some(Restart::At(target)) => target.min(s.timeline.len),
                _ => self.pos,
            },
            ended: s.finished && self.pending.is_none() && self.pos >= s.timeline.len,
            starving: self.starving,
            transport: s.transport,
        };
        if self.deferred {
            self.marker = marker;
        } else {
            marker.apply(s);
        }
        if quiet {
            self.level_sum = 0.0;
            self.level_count = 0;
            if s.level != 0.0 {
                s.level = 0.0;
                s.level_seq += 1;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Backend shells: native PipeWire (playback/native.rs) or cpal
// (playback/cpal_backend.rs). Both drive the same Engine.

const CHOSEN_OUTPUT_LOST: &str = "La salida de audio elegida se desconectó o no está disponible. Conectala o elegí otra en Ajustes > Lectura";
const DEFAULT_OUTPUT_LOST: &str = "La salida de audio se desconectó. Revisá el dispositivo predeterminado o elegí otro en Ajustes > Lectura";

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 24_000;

    fn engine(rate: f32) -> (Control, Engine) {
        let control = Control::new(rate, 1.0);
        let engine = Engine::new(control.clone(), SR);
        (control, engine)
    }
    /// Renders in PipeWire-sized buffers, like the real callback.
    fn render(engine: &mut Engine, samples: usize) -> Vec<f32> {
        let mut out = vec![0.0; samples];
        for buffer in out.chunks_mut(480) {
            engine.render(buffer);
        }
        out
    }
    fn sine(freq: f32, amplitude: f32, samples: usize) -> Vec<i16> {
        (0..samples)
            .map(|i| {
                let t = i as f32 / SR as f32;
                ((2.0 * std::f32::consts::PI * freq * t).sin() * amplitude * 32767.0) as i16
            })
            .collect()
    }
    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
    }
    fn zero_crossing_hz(x: &[f32]) -> f32 {
        let rising: Vec<usize> = (1..x.len())
            .filter(|&i| x[i - 1] <= 0.0 && x[i] > 0.0)
            .collect();
        let (first, last) = (rising[0], rising[rising.len() - 1]);
        (rising.len() - 1) as f32 * SR as f32 / (last - first) as f32
    }

    #[test]
    fn rate_one_is_a_bit_exact_bypass_after_the_fade_in() {
        let (control, mut engine) = engine(1.0);
        let pcm: Vec<i16> = (0..24_000i32)
            .map(|i| ((i * 7919) % 65_536 - 32_768) as i16)
            .collect();
        control.append(&pcm);
        control.set_finished(true);
        control.play();
        let out = render(&mut engine, 12_000);
        for i in engine.fade_len..out.len() {
            assert_eq!(out[i].to_bits(), (f32::from(pcm[i]) / 32768.0).to_bits());
        }
        assert_eq!(control.status().position_samples, 12_000);
    }

    #[test]
    fn stretched_position_advances_by_input_consumed() {
        let (control, mut engine) = engine(1.5);
        control.append(&sine(220.0, 0.5, SR as usize * 3));
        control.play();
        let out = render(&mut engine, 12_000);
        let position = control.status().position_samples as f64;
        assert!((position - 18_000.0).abs() <= 2.0, "{position}");
        assert!(rms(&out[2_000..]) > 0.25);
    }

    #[test]
    fn double_speed_keeps_pitch_and_consumes_twice_the_input() {
        let (control, mut engine) = engine(2.0);
        control.append(&sine(440.0, 0.5, SR as usize * 4));
        control.set_finished(true);
        control.play();
        let out = render(&mut engine, SR as usize);
        let position = control.status().position_samples as f64;
        assert!(
            (position / (2.0 * SR as f64) - 1.0).abs() < 0.01,
            "{position}"
        );
        let hz = zero_crossing_hz(&out[4_800..]);
        assert!((hz / 440.0 - 1.0).abs() < 0.03, "{hz} Hz");
        assert!(rms(&out[4_800..]) > 0.25);
    }

    #[test]
    fn seek_clamps_reports_immediately_and_moves_the_read_head() {
        let (control, mut engine) = engine(1.0);
        let pcm: Vec<i16> = (0..4_000).map(|i| i as i16).collect();
        control.append(&pcm);
        control.play();
        render(&mut engine, 1_000);
        assert_eq!(control.status().position_samples, 1_000);
        control.seek(10_000);
        assert_eq!(control.status().position_samples, 4_000);
        control.seek(500);
        assert_eq!(control.status().position_samples, 500);
        let out = render(&mut engine, 2_000);
        let position = control.status().position_samples;
        assert!(position > 500 && position < 2_500, "{position}");
        // After the fade-out at 1000 and fade-in at 500, playback is exact again.
        assert_eq!(out[1_999], f32::from(pcm[position as usize - 1]) / 32768.0);
        // Seeking while paused applies without rendering audio first.
        control.pause();
        render(&mut engine, 1_000);
        control.seek(100);
        render(&mut engine, 480);
        assert_eq!(control.status().position_samples, 100);
    }

    #[test]
    fn appending_while_starved_resumes_automatically() {
        for rate in [1.0, 1.5] {
            let (control, mut engine) = engine(rate);
            control.append(&sine(300.0, 0.5, 2_400));
            control.play();
            let out = render(&mut engine, 4_800);
            let status = control.status();
            assert!(status.starved, "rate {rate}");
            assert!(!status.ended);
            assert!(status.position_samples <= 2_400);
            assert!(out[3_800..].iter().all(|v| *v == 0.0), "rate {rate}");
            control.append(&sine(300.0, 0.5, SR as usize));
            let out = render(&mut engine, 4_800);
            let status = control.status();
            assert!(!status.starved, "rate {rate}");
            assert!(status.position_samples > 2_400, "rate {rate}");
            assert!(rms(&out[2_400..]) > 0.25, "rate {rate}");
        }
    }

    #[test]
    fn finished_audio_reaches_ended_at_the_exact_end() {
        for rate in [1.0, 2.0, 0.5] {
            let (control, mut engine) = engine(rate);
            control.append(&sine(300.0, 0.5, 4_800));
            control.set_finished(true);
            control.play();
            let out = render(&mut engine, 24_000);
            let status = control.status();
            assert!(status.ended, "rate {rate}");
            assert!(!status.starved, "rate {rate}");
            assert_eq!(status.position_samples, 4_800, "rate {rate}");
            assert!(out[23_000..].iter().all(|v| *v == 0.0), "rate {rate}");
            // Replaying from the start clears `ended` right away.
            control.seek(0);
            assert!(!control.status().ended);
        }
    }

    #[test]
    fn pause_keeps_the_position_and_play_continues() {
        for rate in [1.0, 1.5] {
            let (control, mut engine) = engine(rate);
            control.append(&sine(300.0, 0.5, SR as usize * 2));
            control.play();
            render(&mut engine, 2_400);
            control.pause();
            assert!(!control.status().playing);
            render(&mut engine, 2_400);
            let paused_at = control.status().position_samples;
            let silent = render(&mut engine, 4_800);
            assert_eq!(control.status().position_samples, paused_at, "rate {rate}");
            assert!(silent.iter().all(|v| *v == 0.0));
            assert!(!control.status().starved);
            control.play();
            render(&mut engine, 2_400);
            assert!(control.status().position_samples > paused_at, "rate {rate}");
        }
    }

    #[test]
    fn volume_scales_rms_smoothly_and_feeds_the_meter() {
        let (control, mut engine) = engine(1.0);
        control.append(&sine(300.0, 0.5, SR as usize * 2));
        control.play();
        let loud = render(&mut engine, 4_800);
        let seq = control.status().level_seq;
        assert!(seq >= 9);
        assert!((control.status().level - rms(&loud[4_320..])).abs() < 0.01);
        control.set_volume(0.5);
        let quiet = render(&mut engine, 4_800);
        let ratio = rms(&quiet[2_400..]) / rms(&loud[2_400..]);
        assert!((ratio - 0.5).abs() < 0.01, "{ratio}");
        assert!(quiet.windows(2).all(|w| (w[1] - w[0]).abs() < 0.05));
        let status = control.status();
        assert!(status.level_seq > seq);
        assert!((status.level - 0.5 * 0.5 / 2f32.sqrt()).abs() < 0.01);
        // Silence publishes zero once, then the sequence stops moving.
        control.pause();
        render(&mut engine, 2_400);
        let paused = control.status();
        assert_eq!(paused.level, 0.0);
        render(&mut engine, 2_400);
        assert_eq!(control.status().level_seq, paused.level_seq);
    }

    #[test]
    fn reset_clears_the_timeline_but_keeps_the_play_flag() {
        let (control, mut engine) = engine(1.0);
        control.append(&sine(300.0, 0.8, SR as usize));
        control.set_finished(true);
        control.play();
        let before = render(&mut engine, 1_000);
        control.reset();
        let status = control.status();
        assert_eq!(status.position_samples, 0);
        assert_eq!(status.buffered_samples, 0);
        assert!(status.playing && !status.ended && !status.starved);
        let after = render(&mut engine, 1_000);
        // The cut is smoothed rather than a step to zero.
        assert!((after[0] - before[999]).abs() < 0.05);
        assert!(after[900..].iter().all(|v| v.abs() < 1e-3));
        assert!(control.status().starved);
        let fresh: Vec<i16> = (0..SR as i32).map(|i| (i % 1000) as i16).collect();
        control.append(&fresh);
        let out = render(&mut engine, 2_000);
        let position = control.status().position_samples as usize;
        assert_eq!(position, 2_000);
        assert_eq!(out[1_999], f32::from(fresh[1_999]) / 32768.0);
    }

    #[test]
    fn transport_and_rate_changes_never_click() {
        let (control, mut engine) = engine(1.0);
        control.append(&sine(100.0, 0.8, SR as usize * 20));
        control.play();
        let mut out = render(&mut engine, 4_800);
        let steps: [&dyn Fn(&Control); 9] = [
            &|c| c.set_rate(1.5),
            &|c| c.set_rate(1.0),
            &|c| c.seek(96_000),
            &|c| c.pause(),
            &|c| c.play(),
            &|c| c.set_rate(2.5),
            &|c| c.seek(1_000),
            &|c| c.set_rate(0.5),
            &|c| c.set_volume(0.2),
        ];
        for step in steps {
            step(&control);
            out.extend(render(&mut engine, 4_800));
        }
        // A 100 Hz sine at 0.8 moves at most ≈0.021 per sample; a click jumps.
        let worst = out
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max);
        assert!(worst < 0.05, "max step {worst}");
    }

    #[test]
    fn timeline_spans_chunks_and_zero_pads_outside() {
        let mut timeline = Timeline::default();
        let pcm: Vec<i16> = (0..(CHUNK * 2 + 77)).map(|i| (i % 30_000) as i16).collect();
        for part in pcm.chunks(5_003) {
            timeline.append(part);
        }
        assert_eq!(timeline.len, pcm.len() as u64);
        let mut out = vec![1.0; 40];
        timeline.read(CHUNK as i64 - 20, &mut out);
        for (k, v) in out.iter().enumerate() {
            assert_eq!(*v, f32::from(pcm[CHUNK - 20 + k]) / 32768.0);
        }
        timeline.read(-5, &mut out);
        assert!(out[..5].iter().all(|v| *v == 0.0));
        assert_eq!(out[5], 0.0);
        assert_eq!(out[6], f32::from(pcm[1]) / 32768.0);
        timeline.read(pcm.len() as i64 - 3, &mut out);
        assert_eq!(out[2], f32::from(pcm[pcm.len() - 1]) / 32768.0);
        assert!(out[3..].iter().all(|v| *v == 0.0));
    }

    #[test]
    fn controls_clamp_and_ignore_invalid_values() {
        let control = Control::new(f32::NAN, 7.0);
        assert_eq!(control.lock().rate, 1.0);
        assert_eq!(control.lock().volume, 1.0);
        control.set_rate(9.0);
        assert_eq!(control.lock().rate, MAX_RATE);
        control.set_rate(0.1);
        assert_eq!(control.lock().rate, MIN_RATE);
        control.set_rate(f32::INFINITY);
        assert_eq!(control.lock().rate, MIN_RATE);
        control.set_volume(-1.0);
        assert_eq!(control.lock().volume, 0.0);
        control.set_volume(f32::NAN);
        assert_eq!(control.lock().volume, 0.0);
    }

    #[test]
    fn player_handle_is_shareable_across_threads() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Player>();
    }
}
