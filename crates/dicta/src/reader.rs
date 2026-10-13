//! Lectura en voz alta. Vive en el hilo del controlador: decide qué locuciones
//! generar, guarda el audio por versión y mueve el reproductor. Red, disco y
//! selección corren en tareas aparte y vuelven como `ReaderEvent`.
//!
//! Modelo: el texto preparado se divide en fragmentos (`Chunk`). Cada versión
//! (voz y ajustes) tiene una caché por fragmento. El reproductor contiene un
//! tramo contiguo de fragmentos de la versión activa, `run_start..run_end`,
//! más el fragmento que se está recibiendo en streaming.
use crate::{
    history_ops,
    wav::{read_history_wav, write_wav_file},
};
use dicta_core::{
    AppConfig, AppPaths, AppSnapshot, AudioDevice, Command, ProviderMode, TtsPhase, TtsSnapshot,
    readings::NewReading,
    store::Store,
    tts::{
        self, CharTime, CleanOptions, Lookahead, NewTextAction, OnDictation, PreparedText,
        SameTextAction, TtsConfirm, TtsSource, TtsVersion, TtsVoice, VoiceSettings, WordTiming,
    },
};
use dicta_platform::{
    playback::{Player, PlayerConfig, PlayerStatus},
    selection::{self, SelectionSource},
};
use dicta_provider::{
    CancellationToken, Credentials, ProviderError, TtsAudio, TtsEvent, TtsRequest, TtsSessionEnd,
    XaiProvider,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    time::{Duration, Instant},
};
use tokio::runtime::Handle;

/// Bytes máximos leídos de la selección; el límite en caracteres se aplica después.
const SELECTION_MAX_BYTES: usize = 4 * 1024 * 1024;
const MAX_QUEUE: usize = 20;
const MAX_VERSIONS: usize = 6;
/// Locuciones enviadas por delante de la que se está generando.
const QUEUE_AHEAD: usize = 2;
const DEMO_TEXT: &str = "Demo de lectura en voz alta. Mars voice lee el texto que seleccionás en cualquier aplicación. Podés pausar, saltar diez segundos, cambiar la velocidad o regenerar con otra voz. En la demo no se usa la API ni se lee tu selección real.";
static SERIAL: AtomicU64 = AtomicU64::new(0);

// ---------------------------------------------------------------------------
// Salida de audio
// ---------------------------------------------------------------------------

/// Reproductor real (PipeWire) o virtual (demo y pruebas).
pub(crate) trait AudioOut: Send {
    fn reset(&self);
    fn append(&self, pcm: &[i16]);
    fn set_finished(&self, finished: bool);
    fn play(&self);
    fn pause(&self);
    fn seek(&self, sample: u64);
    fn set_rate(&self, rate: f32);
    fn set_volume(&self, volume: f32);
    fn status(&self) -> PlayerStatus;
}
impl AudioOut for Player {
    fn reset(&self) {
        Player::reset(self)
    }
    fn append(&self, pcm: &[i16]) {
        Player::append(self, pcm)
    }
    fn set_finished(&self, finished: bool) {
        Player::set_finished(self, finished)
    }
    fn play(&self) {
        Player::play(self)
    }
    fn pause(&self) {
        Player::pause(self)
    }
    fn seek(&self, sample: u64) {
        Player::seek(self, sample)
    }
    fn set_rate(&self, rate: f32) {
        Player::set_rate(self, rate)
    }
    fn set_volume(&self, volume: f32) {
        Player::set_volume(self, volume)
    }
    fn status(&self) -> PlayerStatus {
        Player::status(self)
    }
}
/// Reloj de la salida virtual. Las pruebas lo avanzan a mano.
#[derive(Clone)]
pub(crate) enum Clock {
    Real(Instant),
    #[cfg_attr(not(test), allow(dead_code))]
    Manual(Arc<AtomicU64>),
}
impl Clock {
    fn now(&self) -> Duration {
        match self {
            Self::Real(start) => start.elapsed(),
            Self::Manual(ms) => Duration::from_millis(ms.load(Ordering::Acquire)),
        }
    }
}
struct VirtualState {
    buffered: u64,
    position: f64,
    playing: bool,
    finished: bool,
    rate: f32,
    last: Duration,
    level_seq: u64,
}
/// Avanza en tiempo real sin abrir dispositivos. La demo no reproduce sonido.
pub(crate) struct VirtualPlayer {
    sample_rate: u32,
    clock: Clock,
    state: Mutex<VirtualState>,
}
impl VirtualPlayer {
    pub(crate) fn new(sample_rate: u32, clock: Clock) -> Self {
        let last = clock.now();
        Self {
            sample_rate,
            clock,
            state: Mutex::new(VirtualState {
                buffered: 0,
                position: 0.0,
                playing: false,
                finished: false,
                rate: 1.0,
                last,
                level_seq: 0,
            }),
        }
    }
    fn with<R>(&self, f: impl FnOnce(&mut VirtualState) -> R) -> R {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let now = self.clock.now();
        let dt = now.saturating_sub(state.last).as_secs_f64();
        state.last = now;
        if state.playing && (state.position as u64) < state.buffered {
            state.position = (state.position
                + dt * f64::from(self.sample_rate) * f64::from(state.rate))
            .min(state.buffered as f64);
            state.level_seq += 1;
        }
        f(&mut state)
    }
}
impl AudioOut for VirtualPlayer {
    fn reset(&self) {
        self.with(|s| {
            s.buffered = 0;
            s.position = 0.0;
            s.finished = false;
        })
    }
    fn append(&self, pcm: &[i16]) {
        self.with(|s| s.buffered += pcm.len() as u64)
    }
    fn set_finished(&self, finished: bool) {
        self.with(|s| s.finished = finished)
    }
    fn play(&self) {
        self.with(|s| s.playing = true)
    }
    fn pause(&self) {
        self.with(|s| s.playing = false)
    }
    fn seek(&self, sample: u64) {
        self.with(|s| s.position = sample.min(s.buffered) as f64)
    }
    fn set_rate(&self, rate: f32) {
        self.with(|s| s.rate = rate.clamp(tts::RATE_MIN, tts::RATE_MAX))
    }
    fn set_volume(&self, _: f32) {}
    fn status(&self) -> PlayerStatus {
        self.with(|s| {
            let at_end = s.position as u64 >= s.buffered;
            PlayerStatus {
                position_samples: s.position as u64,
                buffered_samples: s.buffered,
                playing: s.playing,
                starved: s.playing && at_end && !s.finished,
                ended: s.finished && at_end,
                level: if s.playing && !at_end { 0.25 } else { 0.0 },
                level_seq: s.level_seq,
                error: None,
            }
        })
    }
}

/// Comandos que atiende el lector. El historial de lecturas (listar, abrir,
/// borrar) sigue en el controlador junto al de dictados.
pub(crate) fn is_reader_command(command: &Command) -> bool {
    matches!(
        command,
        Command::ReadSelection
            | Command::ReadClipboard
            | Command::ReadText(_)
            | Command::ConfirmReading
            | Command::TtsToggle
            | Command::TtsPlay
            | Command::TtsPause
            | Command::TtsStop
            | Command::TtsSeek(_)
            | Command::TtsSeekText(_)
            | Command::TtsSkip(_)
            | Command::TtsSentence(_)
            | Command::TtsSetRate(_)
            | Command::TtsSetVolume(_)
            | Command::TtsRegenerate { .. }
            | Command::TtsUseVersion(_)
            | Command::TtsCopyText
            | Command::TtsExport(_)
            | Command::RefreshVoices
            | Command::PreviewVoice(_)
            | Command::ReplayReading(_)
    )
}

// ---------------------------------------------------------------------------
// Eventos de tareas
// ---------------------------------------------------------------------------

pub(crate) enum ReaderEvent {
    Text {
        request: u64,
        /// Mensaje si no se encontró texto.
        empty: &'static str,
        result: Result<Option<(String, TextOrigin)>, String>,
    },
    Wire {
        reading: u64,
        version: u64,
        session: u64,
        event: TtsEvent,
    },
    SessionEnd {
        reading: u64,
        session: u64,
        result: Result<TtsSessionEnd, ProviderError>,
    },
    Rest {
        reading: u64,
        version: u64,
        session: u64,
        chunk: usize,
        result: Result<TtsAudio, ProviderError>,
    },
    Voices {
        explicit: bool,
        result: Result<Vec<TtsVoice>, String>,
    },
    Outputs(Result<Vec<AudioDevice>, String>),
    Preview {
        request: u64,
        voice: String,
        key: u64,
        result: Result<(Vec<i16>, u32), String>,
    },
    Notice(Result<String, String>),
    SavedAudio {
        history_id: i64,
        result: Result<(String, u64, String), String>,
    },
    ReplayAudio {
        request: u64,
        history_id: i64,
        text: String,
        voice: String,
        chars_sent: u64,
        result: Result<(Vec<i16>, u32, AudioLayout), String>,
    },
}

/// Cómo se obtiene el texto cuando se pulsa el atajo o un botón.
enum TextRequest {
    Copy {
        restore: bool,
        terminals: Vec<String>,
    },
    Sources(Vec<SelectionSource>),
}
impl TextRequest {
    fn empty_message(&self) -> &'static str {
        match self {
            Self::Copy { .. } => {
                "No hay texto seleccionado en la ventana activa. Seleccioná texto y repetí el atajo"
            }
            Self::Sources(sources) if sources.len() > 1 => {
                "No hay texto seleccionado ni en el portapapeles"
            }
            Self::Sources(sources) if sources.first() == Some(&SelectionSource::Clipboard) => {
                "El portapapeles está vacío"
            }
            Self::Sources(_) => {
                "No hay selección primaria. Si tu compositor la desactiva, elegí Copiar la selección automáticamente en Ajustes → Lectura"
            }
        }
    }
}
/// De dónde salió el texto; el respaldo al portapapeles se avisa.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TextOrigin {
    Direct,
    Copied,
    ClipboardFallback,
    Demo,
}

#[derive(Clone)]
enum Backend {
    Real {
        provider: XaiProvider,
        credentials: Credentials,
        allow_env: bool,
    },
    Demo {
        chunk_delay: Duration,
    },
}

/// Distribución del WAV guardado para recuperar los fragmentos sin regenerar.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct AudioLayout {
    sample_rate: u32,
    text_key: u64,
    chunks: Vec<u64>,
}

// ---------------------------------------------------------------------------
// Estado de una lectura
// ---------------------------------------------------------------------------

struct ChunkAudio {
    pcm: Arc<Vec<i16>>,
    /// Palabras con rangos globales y tiempos relativos al inicio del fragmento.
    words: Vec<WordTiming>,
    duration_ms: u64,
}
struct Version {
    id: u64,
    settings: VoiceSettings,
    label: String,
    chunks: Vec<Option<ChunkAudio>>,
    /// El WebSocket no abrió: se genera por REST, fragmento a fragmento.
    rest: bool,
}
struct Partial {
    chunk: usize,
    bytes: Vec<u8>,
    marks: Vec<CharTime>,
}
struct Session {
    id: u64,
    version: u64,
    cancel: CancellationToken,
    tx: Option<tokio::sync::mpsc::Sender<(u64, String)>>,
    rest: bool,
    /// Fragmentos enviados que todavía no terminaron, en orden.
    queued: VecDeque<usize>,
    next_push: usize,
    ready: bool,
    got_audio: bool,
}
enum HistoryTarget {
    New,
    Existing { id: i64, chars_sent: u64 },
}
struct Active {
    id: u64,
    prepared: Arc<PreparedText>,
    text_key: u64,
    history_id: Option<i64>,
    provider: ProviderMode,
    versions: Vec<Version>,
    active: usize,
    session: Option<Session>,
    partials: HashMap<u64, Partial>,
    run_start: usize,
    run_end: usize,
    /// Fragmento `run_end` que se agrega al reproductor mientras llega.
    streaming: Option<(usize, usize)>,
    player_rate: u32,
    chars_sent: u64,
    error: Option<String>,
    layout_dirty: bool,
    finished_at: Option<Instant>,
    history_saved: bool,
}
impl Active {
    fn version(&self) -> &Version {
        &self.versions[self.active]
    }
    fn chunk_chars(&self, k: usize) -> usize {
        self.prepared.chunk_text(k).chars().count()
    }
    /// Milisegundos por carácter medidos en esta versión, o la estimación.
    fn ms_per_char(&self) -> f64 {
        let v = self.version();
        let (mut ms, mut chars) = (0u64, 0usize);
        for (k, c) in v.chunks.iter().enumerate() {
            if let Some(c) = c {
                ms += c.duration_ms;
                chars += self.chunk_chars(k);
            }
        }
        if chars > 0 && ms > 0 {
            ms as f64 / chars as f64
        } else {
            tts::estimate_ms(1000, v.settings.speed) as f64 / 1000.0
        }
    }
    fn chunk_ms(&self, k: usize, per_char: f64) -> u64 {
        match &self.version().chunks[k] {
            Some(c) => c.duration_ms,
            None => (self.chunk_chars(k) as f64 * per_char).round() as u64,
        }
    }
    fn offset_ms(&self, k: usize) -> u64 {
        let per_char = self.ms_per_char();
        (0..k).map(|i| self.chunk_ms(i, per_char)).sum()
    }
    fn total_ms(&self) -> u64 {
        self.offset_ms(self.prepared.chunks.len())
    }
    /// Fragmento y desplazamiento dentro de él para una posición global.
    fn chunk_at(&self, ms: u64) -> (usize, u64) {
        let per_char = self.ms_per_char();
        let mut start = 0;
        let n = self.prepared.chunks.len();
        for k in 0..n {
            let len = self.chunk_ms(k, per_char);
            if ms < start + len || k + 1 == n {
                return (k, ms.saturating_sub(start).min(len));
            }
            start += len;
        }
        (0, 0)
    }
    fn sample_rate(&self) -> u32 {
        self.player_rate
    }
    fn generating(&self) -> bool {
        self.session.is_some()
    }
}
struct Pending {
    prepared: Arc<PreparedText>,
    history: HistoryTarget,
}
/// Audio guardado en el historial para reproducir sin regenerar.
struct Prefill {
    voice: String,
    pcm: Vec<i16>,
    rate: u32,
    layout: AudioLayout,
}
struct Preview {
    request: u64,
    voice: String,
    player: Option<Box<dyn AudioOut>>,
}

// ---------------------------------------------------------------------------
// Reader
// ---------------------------------------------------------------------------

/// Lo que el controlador debe hacer después de un comando de lectura.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Outcome {
    /// Persistir `snapshot.config` (velocidad o volumen recordados).
    pub save_config: bool,
    /// Detener el audio del historial (pw-play): empieza una lectura.
    pub stop_note_audio: bool,
}

pub(crate) struct Reader {
    runtime: Handle,
    backend: Backend,
    demo: bool,
    clock: Clock,
    events_tx: Sender<ReaderEvent>,
    events: Receiver<ReaderEvent>,
    next_id: u64,
    read_request: Option<u64>,
    replay_request: Option<u64>,
    active: Option<Active>,
    pending: Option<Pending>,
    queue: VecDeque<String>,
    player: Option<Box<dyn AudioOut>>,
    player_key: Option<(u32, Option<String>)>,
    idle_error: Option<String>,
    voices_loaded: bool,
    preview: Option<Preview>,
    preview_cache: HashMap<(String, u64), (Arc<Vec<i16>>, u32)>,
    last_position: u64,
    last_level_seq: u64,
    last_phase: TtsPhase,
}

impl Reader {
    pub(crate) fn new(
        runtime: Handle,
        demo: bool,
        allow_env: bool,
        credentials: Credentials,
    ) -> anyhow::Result<Self> {
        let backend = if demo {
            Backend::Demo {
                chunk_delay: Duration::from_millis(180),
            }
        } else {
            Backend::Real {
                provider: XaiProvider::new()?,
                credentials,
                allow_env,
            }
        };
        Ok(Self::with_backend(
            runtime,
            backend,
            demo,
            Clock::Real(Instant::now()),
        ))
    }
    fn with_backend(runtime: Handle, backend: Backend, demo: bool, clock: Clock) -> Self {
        let (events_tx, events) = mpsc::channel();
        Self {
            runtime,
            backend,
            demo,
            clock,
            events_tx,
            events,
            next_id: 0,
            read_request: None,
            replay_request: None,
            active: None,
            pending: None,
            queue: VecDeque::new(),
            player: None,
            player_key: None,
            idle_error: None,
            voices_loaded: false,
            preview: None,
            preview_cache: HashMap::new(),
            last_position: 0,
            last_level_seq: 0,
            last_phase: TtsPhase::Idle,
        }
    }
    fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }
    pub(crate) fn phase(&self) -> TtsPhase {
        if self.pending.is_some() {
            return TtsPhase::Confirm;
        }
        let Some(a) = &self.active else {
            if self.read_request.is_some() || self.replay_request.is_some() {
                return TtsPhase::Reading;
            }
            return if self.idle_error.is_some() {
                TtsPhase::Error
            } else {
                TtsPhase::Idle
            };
        };
        let Some(player) = &self.player else {
            return if a.error.is_some() {
                TtsPhase::Error
            } else {
                TtsPhase::Connecting
            };
        };
        let st = player.status();
        if st.ended {
            return TtsPhase::Finished;
        }
        let has_audio = st.buffered_samples > 0;
        if st.playing {
            if st.starved {
                if a.error.is_some() && !a.generating() {
                    TtsPhase::Error
                } else if !has_audio && a.run_start == 0 {
                    TtsPhase::Connecting
                } else {
                    TtsPhase::Buffering
                }
            } else {
                TtsPhase::Playing
            }
        } else if a.error.is_some() && !has_audio {
            TtsPhase::Error
        } else {
            TtsPhase::Paused
        }
    }
    /// Suena o está por sonar.
    #[cfg(test)]
    pub(crate) fn audible(&self) -> bool {
        self.phase().audible() || self.preview.is_some()
    }

    // ---- comandos ---------------------------------------------------------

    /// Ejecuta un comando de lectura. Devuelve `Err(command)` si no es de lectura.
    pub(crate) fn handle(
        &mut self,
        command: Command,
        snap: &mut AppSnapshot,
        store: &Store,
        paths: &AppPaths,
        dictation_busy: bool,
    ) -> Result<Outcome, Command> {
        let mut outcome = Outcome::default();
        match command {
            Command::ReadSelection | Command::ReadClipboard | Command::ReadText(_)
                if dictation_busy =>
            {
                self.show_error(snap, "Terminá el dictado antes de leer en voz alta");
            }
            Command::ReplayReading(_) if dictation_busy => {
                self.show_error(snap, "Terminá el dictado antes de leer en voz alta");
            }
            Command::ReadSelection => {
                let tts = &snap.config.tts;
                let request = match tts.source {
                    TtsSource::CopySelection => TextRequest::Copy {
                        restore: tts.restore_clipboard,
                        terminals: tts.terminal_classes.clone(),
                    },
                    TtsSource::SelectionThenClipboard => TextRequest::Sources(vec![
                        SelectionSource::Primary,
                        SelectionSource::Clipboard,
                    ]),
                    TtsSource::Selection => TextRequest::Sources(vec![SelectionSource::Primary]),
                    TtsSource::Clipboard => TextRequest::Sources(vec![SelectionSource::Clipboard]),
                };
                self.read_source(request, snap);
                outcome.stop_note_audio = true;
            }
            Command::ReadClipboard => {
                self.read_source(TextRequest::Sources(vec![SelectionSource::Clipboard]), snap);
                outcome.stop_note_audio = true;
            }
            Command::ReadText(text) => {
                self.accept_text(text, HistoryTarget::New, snap, store, paths);
                outcome.stop_note_audio = true;
            }
            Command::ConfirmReading => {
                if let Some(pending) = self.pending.take() {
                    self.start_reading(pending.prepared, pending.history, snap, store, None);
                }
            }
            Command::TtsToggle => self.toggle(snap),
            Command::TtsPlay => self.play(snap),
            Command::TtsPause => {
                if let Some(p) = &self.player {
                    p.pause();
                }
            }
            Command::TtsStop => self.stop(snap, store, paths),
            Command::TtsSeek(ms) => self.seek(ms),
            Command::TtsSeekText(byte) => self.seek_text(byte),
            Command::TtsSkip(seconds) => {
                if self.active.is_some() {
                    let position = self.position_ms();
                    let target = if seconds < 0 {
                        position.saturating_sub(u64::from(seconds.unsigned_abs()) * 1000)
                    } else {
                        position + u64::from(seconds.unsigned_abs()) * 1000
                    };
                    self.seek(target);
                }
            }
            Command::TtsSentence(delta) => self.step_sentence(delta),
            Command::TtsSetRate(rate) => {
                let rate = if rate.is_finite() {
                    rate.clamp(tts::RATE_MIN, tts::RATE_MAX)
                } else {
                    1.0
                };
                if let Some(p) = &self.player {
                    p.set_rate(rate);
                }
                snap.tts.rate = rate;
                if snap.config.tts.remember_rate && snap.config.tts.playback_rate != rate {
                    snap.config.tts.playback_rate = rate;
                    outcome.save_config = true;
                }
            }
            Command::TtsSetVolume(volume) => {
                let volume = if volume.is_finite() {
                    volume.clamp(0.0, 1.0)
                } else {
                    1.0
                };
                if let Some(p) = &self.player {
                    p.set_volume(volume);
                }
                snap.tts.volume = volume;
                if snap.config.tts.volume != volume {
                    snap.config.tts.volume = volume;
                    outcome.save_config = true;
                }
            }
            Command::TtsRegenerate { voice } => self.regenerate(voice, snap),
            Command::TtsUseVersion(id) => {
                self.use_version(id);
                let config = snap.config.clone();
                self.pump(&config);
                self.sync_versions(snap);
            }
            Command::TtsCopyText => {
                if let Some(a) = &self.active {
                    let text = a.prepared.text.clone();
                    let tx = self.events_tx.clone();
                    let demo = self.demo;
                    self.runtime.spawn_blocking(move || {
                        let result = if demo {
                            Ok("Demo: copiado simulado".to_string())
                        } else {
                            dicta_platform::desktop::copy(&text)
                                .map(|_| "Texto de la lectura copiado".to_string())
                                .map_err(|e| e.to_string())
                        };
                        let _ = tx.send(ReaderEvent::Notice(result));
                    });
                }
            }
            Command::TtsExport(path) => self.export(path, snap),
            Command::RefreshVoices => self.request_voices(&snap.config, true),
            Command::PreviewVoice(voice) => self.preview(voice, snap),
            Command::ReplayReading(id) => {
                self.replay(id, snap, store, paths);
                outcome.stop_note_audio = true;
            }
            other => return Err(other),
        }
        Ok(outcome)
    }
    fn show_error(&mut self, snap: &mut AppSnapshot, message: &str) {
        if self.active.is_some() {
            snap.tts.notice = Some(message.into());
        } else {
            self.idle_error = Some(message.into());
            snap.tts.error = Some(message.into());
            snap.tts.show = true;
        }
    }
    fn read_source(&mut self, request: TextRequest, snap: &mut AppSnapshot) {
        let id = self.id();
        self.read_request = Some(id);
        self.idle_error = None;
        snap.tts.error = None;
        let empty = request.empty_message();
        if self.demo {
            let _ = self.events_tx.send(ReaderEvent::Text {
                request: id,
                empty,
                result: Ok(Some((DEMO_TEXT.into(), TextOrigin::Demo))),
            });
            return;
        }
        let tx = self.events_tx.clone();
        self.runtime.spawn_blocking(move || {
            let result = match request {
                // Copia en la ventana activa en el momento del atajo, antes de
                // que cambie el foco. Restaura el portapapeles aunque falle.
                TextRequest::Copy { restore, terminals } => {
                    selection::copy_selection(selection::CopyOptions {
                        max_bytes: SELECTION_MAX_BYTES,
                        restore_clipboard: restore,
                        extra_terminal_classes: &terminals,
                    })
                    .map(|text| {
                        text.filter(|t| !t.trim().is_empty())
                            .map(|t| (t, TextOrigin::Copied))
                    })
                    .map_err(|e| e.to_string())
                }
                TextRequest::Sources(sources) => {
                    let fallback = sources.len() > 1;
                    let mut error = None;
                    let mut found = None;
                    for (index, source) in sources.into_iter().enumerate() {
                        match selection::read_text(source, SELECTION_MAX_BYTES) {
                            Ok(Some(text)) if !text.trim().is_empty() => {
                                let origin = if fallback && index > 0 {
                                    TextOrigin::ClipboardFallback
                                } else {
                                    TextOrigin::Direct
                                };
                                found = Some((text, origin));
                                break;
                            }
                            Ok(_) => {}
                            Err(e) => error = Some(e.to_string()),
                        }
                    }
                    match (found, error) {
                        (Some(found), _) => Ok(Some(found)),
                        (None, Some(e)) => Err(e),
                        (None, None) => Ok(None),
                    }
                }
            };
            let _ = tx.send(ReaderEvent::Text {
                request: id,
                empty,
                result,
            });
        });
    }
    fn accept_text(
        &mut self,
        raw: String,
        history: HistoryTarget,
        snap: &mut AppSnapshot,
        store: &Store,
        paths: &AppPaths,
    ) {
        let Some(prepared) = tts::prepare(&raw, CleanOptions::from(&snap.config.tts)) else {
            self.show_error(snap, "El texto no tiene nada legible para leer en voz alta");
            return;
        };
        let key = tts::fnv1a(prepared.text.as_bytes());
        let phase = self.phase();
        if let Some(a) = &self.active {
            if a.text_key == key && matches!(history, HistoryTarget::New) {
                match snap.config.tts.same_text {
                    SameTextAction::TogglePause => self.toggle(snap),
                    SameTextAction::Restart => {
                        self.seek(0);
                        self.play(snap);
                    }
                }
                return;
            }
            if phase.has_reading()
                && phase != TtsPhase::Finished
                && snap.config.tts.new_text == NewTextAction::Queue
                && matches!(history, HistoryTarget::New)
            {
                if self.queue.len() >= MAX_QUEUE {
                    snap.tts.notice = Some("La cola de lectura está llena".into());
                } else {
                    self.queue.push_back(raw);
                    snap.tts.notice = Some(format!("En cola: {}", self.queue.len()));
                }
                return;
            }
        }
        self.finish_active(snap, store, paths);
        let prepared = Arc::new(prepared);
        let confirm = snap.config.tts.confirm_chars as usize;
        let chars = prepared.chars();
        if confirm > 0 && chars >= confirm {
            snap.tts.confirm = Some(TtsConfirm {
                chars: chars as u64,
                estimated_usd: tts::estimate_usd(chars as u64),
                truncated: prepared.truncated,
            });
            snap.tts.show = true;
            snap.tts.text = Arc::new(prepared.text.clone());
            snap.tts.sentences = Arc::new(prepared.sentences.clone());
            snap.tts.words = Arc::new(vec![]);
            snap.tts.generated = Arc::new(vec![]);
            self.pending = Some(Pending { prepared, history });
            return;
        }
        self.start_reading(prepared, history, snap, store, None);
    }
    fn ensure_player(&mut self, sample_rate: u32, config: &AppConfig) -> Result<(), String> {
        let key = (sample_rate, config.tts.output_device.clone());
        if self.player.is_some() && self.player_key.as_ref() == Some(&key) {
            return Ok(());
        }
        self.player = None;
        let player: Box<dyn AudioOut> = if self.demo {
            Box::new(VirtualPlayer::new(sample_rate, self.clock.clone()))
        } else {
            Box::new(
                Player::new(PlayerConfig {
                    sample_rate,
                    device: config.tts.output_device.clone(),
                    volume: config.tts.volume,
                    rate: config.tts.playback_rate,
                })
                .map_err(|e| format!("No se pudo abrir la salida de audio: {e}"))?,
            )
        };
        self.player = Some(player);
        self.player_key = Some(key);
        Ok(())
    }
    fn start_reading(
        &mut self,
        prepared: Arc<PreparedText>,
        history: HistoryTarget,
        snap: &mut AppSnapshot,
        store: &Store,
        prefill: Option<Prefill>,
    ) {
        let config = snap.config.clone();
        let mut settings = config.tts.voice_settings();
        if let Some(saved) = &prefill {
            settings.voice = saved.voice.clone();
            settings.sample_rate = saved.rate;
        }
        let id = self.id();
        let version_id = self.id();
        let (history_id, chars_base) = match history {
            HistoryTarget::Existing { id, chars_sent } => (Some(id), chars_sent),
            HistoryTarget::New if config.tts.history_enabled => {
                match store.record_reading(NewReading {
                    created_at_ms: history_ops::now_ms(),
                    text: prepared.text.clone(),
                    voice: settings.voice.clone(),
                    language: settings.language.clone(),
                    provider: config.tts.provider,
                }) {
                    Ok(row) => (Some(row), 0),
                    Err(e) => {
                        snap.tts.notice = Some(e.to_string());
                        (None, 0)
                    }
                }
            }
            HistoryTarget::New => (None, 0),
        };
        let n = prepared.chunks.len();
        let label = self.voice_label(&settings.voice, snap);
        let sample_rate = settings.sample_rate;
        self.active = Some(Active {
            id,
            text_key: tts::fnv1a(prepared.text.as_bytes()),
            prepared: prepared.clone(),
            history_id,
            provider: config.tts.provider,
            versions: vec![Version {
                id: version_id,
                settings,
                label,
                chunks: (0..n).map(|_| None).collect(),
                rest: false,
            }],
            active: 0,
            session: None,
            partials: HashMap::new(),
            run_start: 0,
            run_end: 0,
            streaming: None,
            player_rate: sample_rate,
            chars_sent: chars_base,
            error: None,
            layout_dirty: true,
            finished_at: None,
            history_saved: false,
        });
        self.idle_error = None;
        if let Err(e) = self.ensure_player(sample_rate, &config) {
            if let Some(a) = self.active.as_mut() {
                a.error = Some(e);
            }
        }
        if let Some(p) = &self.player {
            p.reset();
            p.set_finished(false);
            p.set_rate(config.tts.playback_rate);
            p.set_volume(config.tts.volume);
            if config.tts.autoplay {
                p.play();
            } else {
                p.pause();
            }
        }
        if let Some(saved) = prefill {
            self.load_saved(saved);
        }
        let tts_snap = &mut snap.tts;
        tts_snap.reading = id;
        tts_snap.show = true;
        tts_snap.text = Arc::new(prepared.text.clone());
        tts_snap.sentences = Arc::new(prepared.sentences.clone());
        tts_snap.words = Arc::new(vec![]);
        tts_snap.generated = Arc::new(vec![]);
        tts_snap.position_ms = 0;
        tts_snap.current_word = None;
        tts_snap.current_sentence = Some(0);
        tts_snap.rate = config.tts.playback_rate;
        tts_snap.volume = config.tts.volume;
        tts_snap.chars = prepared.chars() as u64;
        tts_snap.confirm = None;
        tts_snap.error = None;
        tts_snap.notice = prepared.truncated.then(|| {
            format!(
                "Se leerán los primeros {} caracteres; el resto supera el límite configurado",
                prepared.chars()
            )
        });
        if !self.voices_loaded {
            self.request_voices(&config, false);
        }
        self.pump(&config);
        self.sync_versions(snap);
    }
    fn toggle(&mut self, snap: &mut AppSnapshot) {
        match self.phase() {
            TtsPhase::Playing | TtsPhase::Buffering | TtsPhase::Connecting => {
                if let Some(p) = &self.player {
                    p.pause();
                }
            }
            TtsPhase::Finished => {
                self.seek(0);
                self.play(snap);
            }
            TtsPhase::Paused | TtsPhase::Error => self.play(snap),
            TtsPhase::Idle | TtsPhase::Reading | TtsPhase::Confirm => {}
        }
    }
    fn play(&mut self, snap: &mut AppSnapshot) {
        let Some(a) = self.active.as_mut() else {
            return;
        };
        // Reintentar tras un error es una acción explícita del usuario.
        if a.error.take().is_some() {
            snap.tts.error = None;
        }
        if self.phase() == TtsPhase::Finished {
            self.seek(0);
        }
        if let Some(p) = &self.player {
            p.play();
        }
        let config = snap.config.clone();
        self.pump(&config);
    }
    pub(crate) fn pause(&mut self) {
        if let Some(p) = &self.player {
            p.pause();
        }
        self.stop_preview();
    }
    /// Empieza un dictado: el micrófono no debe captar la lectura.
    pub(crate) fn on_dictation_start(
        &mut self,
        snap: &mut AppSnapshot,
        store: &Store,
        paths: &AppPaths,
    ) {
        self.stop_preview();
        if self.active.is_none() {
            return;
        }
        match snap.config.tts.on_dictation {
            OnDictation::Pause => {
                if let Some(p) = &self.player {
                    p.pause();
                }
            }
            OnDictation::Stop => self.stop(snap, store, paths),
        }
    }
    /// Detiene todo y cierra la ventana.
    pub(crate) fn stop(&mut self, snap: &mut AppSnapshot, store: &Store, paths: &AppPaths) {
        self.finish_active(snap, store, paths);
        self.pending = None;
        self.queue.clear();
        self.read_request = None;
        self.replay_request = None;
        self.idle_error = None;
        self.stop_preview();
        // Cerrar el stream libera el nodo PipeWire mientras no se lee.
        self.player = None;
        self.player_key = None;
        let keep = std::mem::take(&mut snap.tts);
        snap.tts = TtsSnapshot {
            voices: keep.voices,
            outputs: keep.outputs,
            account_ready: keep.account_ready,
            rate: snap.config.tts.playback_rate,
            volume: snap.config.tts.volume,
            level_seq: keep.level_seq,
            ..TtsSnapshot::default()
        };
    }
    /// Cancela la generación y guarda el historial de la lectura activa.
    fn finish_active(&mut self, snap: &mut AppSnapshot, store: &Store, paths: &AppPaths) {
        let Some(mut a) = self.active.take() else {
            return;
        };
        drop_session(&mut a, true);
        if let Some(p) = &self.player {
            p.pause();
            p.reset();
        }
        self.save_history(&mut a, snap, store, paths);
    }
    fn save_history(
        &mut self,
        a: &mut Active,
        snap: &mut AppSnapshot,
        store: &Store,
        paths: &AppPaths,
    ) {
        let Some(id) = a.history_id else {
            return;
        };
        let v = a.version();
        let duration: u64 = v.chunks.iter().flatten().map(|c| c.duration_ms).sum();
        if let Err(e) = store.update_reading(
            id,
            a.chars_sent,
            duration,
            &v.settings.voice,
            a.error.as_deref(),
        ) {
            snap.tts.notice = Some(e.to_string());
        }
        if !a.history_saved && snap.config.tts.saves_audio() && !self.demo {
            // Prefijo contiguo desde el principio: lo que se puede reproducir sin regenerar.
            let prefix: Vec<Arc<Vec<i16>>> = v
                .chunks
                .iter()
                .map_while(|c| c.as_ref().map(|c| c.pcm.clone()))
                .collect();
            if !prefix.is_empty() {
                a.history_saved = true;
                let layout = AudioLayout {
                    sample_rate: a.player_rate,
                    text_key: a.text_key,
                    chunks: prefix.iter().map(|p| p.len() as u64).collect(),
                };
                let dir = paths.data.clone();
                let tx = self.events_tx.clone();
                self.runtime.spawn_blocking(move || {
                    let result = write_history_wav(&dir, &prefix, layout.sample_rate).and_then(
                        |(name, bytes)| {
                            serde_json::to_string(&layout)
                                .map(|l| (name, bytes, l))
                                .map_err(|e| e.to_string())
                        },
                    );
                    let _ = tx.send(ReaderEvent::SavedAudio {
                        history_id: id,
                        result,
                    });
                });
            }
        }
        if let Err(e) = history_ops::refresh(store, snap) {
            snap.tts.notice = Some(e.to_string());
        }
    }
    fn position_ms(&self) -> u64 {
        let (Some(a), Some(p)) = (&self.active, &self.player) else {
            return 0;
        };
        let st = p.status();
        a.offset_ms(a.run_start) + samples_to_ms(st.position_samples, a.sample_rate())
    }
    fn seek(&mut self, ms: u64) {
        let Some(a) = self.active.as_mut() else {
            return;
        };
        let Some(player) = self.player.as_deref() else {
            return;
        };
        let ms = ms.min(a.total_ms());
        let run_from = a.offset_ms(a.run_start);
        let run_samples = player.status().buffered_samples;
        let run_to = run_from + samples_to_ms(run_samples, a.sample_rate());
        if ms >= run_from && (ms < run_to || (ms == run_to && a.streaming.is_some())) {
            player.seek(ms_to_samples(ms - run_from, a.sample_rate()));
        } else {
            let (k, within) = a.chunk_at(ms);
            rebuild_run(a, player, k, within);
        }
        a.finished_at = None;
        // La generación se reprograma en el siguiente `pump`.
    }
    fn seek_text(&mut self, byte: usize) {
        let Some(a) = &self.active else {
            return;
        };
        let per_char = a.ms_per_char();
        let mut target = None;
        for (k, chunk) in a.version().chunks.iter().enumerate() {
            let span = a.prepared.chunks[k].span;
            if byte > span.end {
                continue;
            }
            let offset = a.offset_ms(k);
            // Palabra generada que contiene el byte: salto exacto. Sin audio, al
            // inicio del fragmento, que es desde donde se genera.
            target = Some(match chunk {
                Some(c) => c
                    .words
                    .iter()
                    .find(|w| byte < w.span.end)
                    .or(c.words.last())
                    .map_or_else(
                        || {
                            let text = a.prepared.chunk_text(k);
                            let local = byte.saturating_sub(span.start).min(text.len());
                            let local = (0..=local)
                                .rev()
                                .find(|i| text.is_char_boundary(*i))
                                .unwrap_or(0);
                            offset + (text[..local].chars().count() as f64 * per_char) as u64
                        },
                        |w| offset + w.start_ms,
                    ),
                None => offset,
            });
            break;
        }
        if let Some(target) = target {
            self.seek(target);
        }
    }
    fn step_sentence(&mut self, delta: i32) {
        let Some(a) = &self.active else {
            return;
        };
        let position = self.position_ms();
        let current = self.current_sentence(position).unwrap_or(0);
        let sentences = a.prepared.sentences.len();
        let start_of = |s: usize| -> u64 { self.sentence_start_ms(s) };
        let target = if delta < 0 {
            // Como en un reproductor: primero vuelve al inicio de la frase actual.
            let here = start_of(current);
            if position > here + 1500 && delta == -1 {
                current
            } else {
                current.saturating_sub(delta.unsigned_abs() as usize)
            }
        } else {
            (current + delta as usize).min(sentences.saturating_sub(1))
        };
        let ms = start_of(target);
        self.seek(ms);
    }
    fn sentence_start_ms(&self, sentence: usize) -> u64 {
        let Some(a) = &self.active else {
            return 0;
        };
        let Some(span) = a.prepared.sentences.get(sentence) else {
            return 0;
        };
        let Some(k) = a.prepared.chunk_of_sentence(sentence) else {
            return 0;
        };
        let offset = a.offset_ms(k);
        if let Some(c) = &a.version().chunks[k] {
            if let Some(w) = c.words.iter().find(|w| w.span.start >= span.start) {
                return offset + w.start_ms;
            }
        }
        let chunk = a.prepared.chunks[k].span;
        let before = a.prepared.text[chunk.start..span.start].chars().count();
        offset + (before as f64 * a.ms_per_char()) as u64
    }
    fn current_sentence(&self, position: u64) -> Option<usize> {
        let a = self.active.as_ref()?;
        let (k, within) = a.chunk_at(position);
        let chunk = a.prepared.chunks[k];
        if let Some(c) = &a.version().chunks[k] {
            if let Some(i) = tts::word_at(&c.words, within) {
                return tts::sentence_at(&a.prepared.sentences, c.words[i].span.start);
            }
        }
        // Sin palabras: proporcional dentro del fragmento.
        let len = a.chunk_ms(k, a.ms_per_char()).max(1);
        let text = a.prepared.chunk_text(k);
        let chars = text.chars().count();
        let at_char = (chars as u64 * within / len) as usize;
        let byte = text
            .char_indices()
            .nth(at_char)
            .map_or(text.len(), |(i, _)| i);
        tts::sentence_at(&a.prepared.sentences, chunk.span.start + byte)
            .map(|s| s.clamp(chunk.first_sentence, chunk.end_sentence - 1))
    }
    fn regenerate(&mut self, voice: String, snap: &mut AppSnapshot) {
        let mut settings = snap.config.tts.voice_settings();
        settings.voice = voice;
        let mut probe = snap.config.tts.clone();
        probe.voice = settings.voice.clone();
        if let Err(e) = probe.validate() {
            snap.tts.notice = Some(e.to_string());
            return;
        }
        let position = self.position_ms();
        let label = self.voice_label(&settings.voice, snap);
        let config = snap.config.clone();
        let Some(a) = self.active.as_ref() else {
            return;
        };
        // Todas las versiones comparten la frecuencia del reproductor de esta lectura.
        settings.sample_rate = a.player_rate;
        let existing = a
            .versions
            .iter()
            .find(|v| v.settings == settings)
            .map(|v| v.id);
        let (k, _) = a.chunk_at(position);
        if let Some(id) = existing {
            if let Some(a) = self.active.as_mut() {
                if a.error.take().is_some() {
                    snap.tts.error = None;
                }
            }
            self.use_version(id);
            self.pump(&config);
            self.sync_versions(snap);
            return;
        }
        let id = self.id();
        let was_playing = self.player.as_ref().is_some_and(|p| p.status().playing);
        let Some(a) = self.active.as_mut() else {
            return;
        };
        if a.versions.len() >= MAX_VERSIONS {
            // Libera la versión más vieja que no está en uso.
            let drop_index = (0..a.versions.len()).find(|i| *i != a.active).unwrap_or(0);
            a.versions.remove(drop_index);
            if drop_index < a.active {
                a.active -= 1;
            }
        }
        let n = a.prepared.chunks.len();
        a.versions.push(Version {
            id,
            settings,
            label,
            chunks: (0..n).map(|_| None).collect(),
            rest: false,
        });
        a.active = a.versions.len() - 1;
        a.error = None;
        snap.tts.error = None;
        drop_session(a, true);
        a.layout_dirty = true;
        a.finished_at = None;
        if let (Some(a), Some(player)) = (self.active.as_mut(), self.player.as_deref()) {
            rebuild_run(a, player, k, 0);
            player.set_rate(snap.tts.rate);
            if was_playing || config.tts.autoplay {
                player.play();
            }
        }
        self.pump(&config);
        self.sync_versions(snap);
    }
    fn use_version(&mut self, id: u64) {
        let position = self.position_ms();
        let Some(a) = self.active.as_mut() else {
            return;
        };
        let Some(index) = a.versions.iter().position(|v| v.id == id) else {
            return;
        };
        if index == a.active {
            return;
        }
        let (k, within) = a.chunk_at(position);
        let old_len = a.chunk_ms(k, a.ms_per_char()).max(1);
        drop_session(a, true);
        a.active = index;
        a.layout_dirty = true;
        let new_len = a.chunk_ms(k, a.ms_per_char());
        let within = within * new_len / old_len;
        if let Some(player) = self.player.as_deref() {
            rebuild_run(a, player, k, within);
        }
    }

    // ---- generación -------------------------------------------------------

    /// Programa locuciones según la posición y la ventana de anticipación.
    /// Solo se envía texto que va a sonar dentro de la ventana: xAI cobra por
    /// caracteres enviados, y detener la lectura no debe pagar el resto.
    fn pump(&mut self, config: &AppConfig) {
        let position = self.position_ms();
        let Some(a) = self.active.as_mut() else {
            return;
        };
        let n = a.prepared.chunks.len();
        let need = {
            let v = a.version();
            let from = a.streaming.map_or(a.run_end, |(k, _)| k);
            (from..n).find(|k| v.chunks[*k].is_none())
        };
        let Some(need) = need else {
            // Nada más que generar en esta versión: cerrar el socket sin esperar.
            if a.session.as_ref().is_some_and(|s| s.queued.is_empty()) {
                a.session = None;
            }
            return;
        };
        let per_char = a.ms_per_char();
        let mut offsets = Vec::with_capacity(n);
        let mut acc = 0;
        for k in 0..n {
            offsets.push(acc);
            acc += a.chunk_ms(k, per_char);
        }
        let window_ms = u64::from(config.tts.lookahead_seconds) * 1000;
        let all = config.tts.lookahead == Lookahead::All;
        let in_window = |k: usize| all || offsets[k] < position + window_ms;
        let version_id = a.version().id;
        if let Some(s) = &a.session {
            let effective_next = {
                let v = a.version();
                (s.next_push..n)
                    .find(|k| v.chunks[*k].is_none())
                    .unwrap_or(n)
            };
            let covers =
                s.version == version_id && (s.queued.contains(&need) || effective_next == need);
            if !covers {
                drop_session(a, true);
            }
        }
        if a.session.is_none() {
            // Un error no se reintenta solo: cada intento puede cobrar.
            if a.error.is_some() || !in_window(need) {
                return;
            }
            self.next_id += 1;
            let session_id = self.next_id;
            let a = self.active.as_mut().expect("lectura activa");
            let rest = a.version().rest;
            let cancel = CancellationToken::new();
            let tx = if rest {
                None
            } else {
                let (tx, rx) = tokio::sync::mpsc::channel(QUEUE_AHEAD);
                spawn_session(
                    &self.runtime,
                    self.backend.clone(),
                    self.events_tx.clone(),
                    a.id,
                    version_id,
                    session_id,
                    TtsRequest {
                        settings: a.version().settings.clone(),
                        with_timestamps: true,
                    },
                    config.clone(),
                    a.provider,
                    rx,
                    cancel.clone(),
                );
                Some(tx)
            };
            a.session = Some(Session {
                id: session_id,
                version: version_id,
                cancel,
                tx,
                rest,
                queued: VecDeque::new(),
                next_push: need,
                ready: false,
                got_audio: false,
            });
        }
        let a = self.active.as_mut().expect("lectura activa");
        loop {
            let s = a.session.as_ref().expect("sesión");
            let next = s.next_push;
            if next >= n || s.queued.len() >= if s.rest { 1 } else { QUEUE_AHEAD } {
                break;
            }
            if a.version().chunks[next].is_some() {
                a.session.as_mut().expect("sesión").next_push += 1;
                continue;
            }
            if !in_window(next) {
                break;
            }
            let text = a.prepared.chunk_text(next).to_string();
            let chars = text.chars().count() as u64;
            if s.rest {
                spawn_rest(
                    &self.runtime,
                    self.backend.clone(),
                    self.events_tx.clone(),
                    a.id,
                    s.version,
                    s.id,
                    next,
                    TtsRequest {
                        settings: a.version().settings.clone(),
                        with_timestamps: true,
                    },
                    config.clone(),
                    a.provider,
                    text,
                    s.cancel.clone(),
                );
            } else {
                let Some(tx) = &s.tx else { break };
                match tx.try_send((next as u64, text)) {
                    Ok(()) => {}
                    Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => break,
                    Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                        a.session = None;
                        return;
                    }
                }
            }
            let s = a.session.as_mut().expect("sesión");
            s.queued.push_back(next);
            s.next_push += 1;
            a.chars_sent += chars;
        }
        // Sin trabajo dentro de la ventana: cerrar en vez de dejar el socket ocioso.
        if a.session.as_ref().is_some_and(|s| s.queued.is_empty()) {
            a.session = None;
        }
    }
    fn on_wire(&mut self, reading: u64, version: u64, session: u64, event: TtsEvent) {
        let Some(a) = self.active.as_mut() else {
            return;
        };
        if a.id != reading {
            return;
        }
        let current = a.session.as_ref().is_some_and(|s| s.id == session);
        match event {
            TtsEvent::Ready => {
                if current {
                    a.session.as_mut().unwrap().ready = true;
                }
            }
            TtsEvent::Audio { id, pcm } => {
                let chunk = id as usize;
                if current {
                    a.session.as_mut().unwrap().got_audio = true;
                }
                // Se agrega al reproductor mientras llega si es justo lo que sigue.
                let streamable = a.versions[a.active].id == version
                    && chunk == a.run_end
                    && chunk < a.prepared.chunks.len()
                    && a.versions[a.active].chunks[chunk].is_none()
                    && a.streaming.is_none_or(|(k, _)| k == chunk);
                let partial = a.partials.entry(session).or_insert_with(|| Partial {
                    chunk,
                    bytes: vec![],
                    marks: vec![],
                });
                if partial.chunk != chunk {
                    *partial = Partial {
                        chunk,
                        bytes: vec![],
                        marks: vec![],
                    };
                }
                partial.bytes.extend_from_slice(&pcm);
                if streamable {
                    if let Some(player) = self.player.as_deref() {
                        let appended = a.streaming.map_or(0, |(_, n)| n);
                        let total = partial.bytes.len() / 2;
                        if total > appended {
                            player.append(&bytes_to_i16(&partial.bytes[appended * 2..total * 2]));
                        }
                        a.streaming = Some((chunk, total));
                        player.set_finished(false);
                    }
                }
            }
            TtsEvent::Marks { id, marks } => {
                if let Some(p) = a.partials.get_mut(&session) {
                    if p.chunk == id as usize {
                        p.marks.extend(marks);
                    }
                }
            }
            TtsEvent::Done { id } => {
                let chunk = id as usize;
                let partial = a.partials.remove(&session);
                if current {
                    let s = a.session.as_mut().unwrap();
                    s.queued.retain(|k| *k != chunk);
                }
                let Some(partial) = partial.filter(|p| p.chunk == chunk) else {
                    return;
                };
                let audio = TtsAudio {
                    pcm: partial.bytes,
                    marks: partial.marks,
                };
                self.store_chunk(version, chunk, audio);
            }
        }
    }
    /// Guarda el audio completo de un fragmento y extiende el tramo del reproductor.
    fn store_chunk(&mut self, version: u64, chunk: usize, audio: TtsAudio) {
        let Some(a) = self.active.as_mut() else {
            return;
        };
        let Some(vi) = a.versions.iter().position(|v| v.id == version) else {
            return;
        };
        if chunk >= a.prepared.chunks.len() || a.versions[vi].chunks[chunk].is_some() {
            return;
        }
        let pcm = bytes_to_i16(&audio.pcm);
        let rate = a.versions[vi].settings.sample_rate;
        let duration_ms = samples_to_ms(pcm.len() as u64, rate);
        let span = a.prepared.chunks[chunk].span;
        let text = a.prepared.chunk_text(chunk);
        let words = tts::align_words(text, &audio.marks, duration_ms)
            .into_iter()
            .map(|mut w| {
                w.span.start += span.start;
                w.span.end += span.start;
                w
            })
            .collect();
        let pcm = Arc::new(pcm);
        a.versions[vi].chunks[chunk] = Some(ChunkAudio {
            pcm: pcm.clone(),
            words,
            duration_ms,
        });
        a.layout_dirty = true;
        if vi != a.active {
            return;
        }
        let Some(player) = self.player.as_deref() else {
            return;
        };
        if chunk == a.run_end {
            let appended = match a.streaming {
                Some((k, n)) if k == chunk => n,
                _ => 0,
            };
            if appended < pcm.len() {
                player.append(&pcm[appended..]);
            }
            a.streaming = None;
            a.run_end += 1;
            extend_run(a, player);
        }
    }
    fn on_session_end(
        &mut self,
        reading: u64,
        session: u64,
        result: Result<TtsSessionEnd, ProviderError>,
    ) {
        let Some(a) = self.active.as_mut() else {
            return;
        };
        a.partials.remove(&session);
        if a.id != reading || a.session.as_ref().is_none_or(|s| s.id != session) {
            return;
        }
        let s = a.session.take().expect("sesión actual");
        uncount_unsent(a, &s);
        match result {
            Ok(_) | Err(ProviderError::Cancelled) => {}
            Err(error) => {
                let fallback = !s.ready
                    && !s.got_audio
                    && matches!(
                        error,
                        ProviderError::Network
                            | ProviderError::Timeout(_)
                            | ProviderError::Protocol(_)
                            | ProviderError::Http(_)
                            | ProviderError::Disconnected
                    );
                if fallback {
                    let v = a.active;
                    a.versions[v].rest = true;
                } else {
                    a.error = Some(error.to_string());
                }
            }
        }
    }
    fn on_rest(
        &mut self,
        reading: u64,
        version: u64,
        session: u64,
        chunk: usize,
        result: Result<TtsAudio, ProviderError>,
    ) {
        let Some(a) = self.active.as_mut() else {
            return;
        };
        if a.id != reading {
            return;
        }
        if let Some(s) = a.session.as_mut().filter(|s| s.id == session) {
            s.queued.retain(|k| *k != chunk);
        }
        match result {
            Ok(audio) => self.store_chunk(version, chunk, audio),
            Err(ProviderError::Cancelled) => {}
            Err(e) => {
                if let Some(a) = self.active.as_mut() {
                    a.error = Some(e.to_string());
                    if a.session.as_ref().is_some_and(|s| s.id == session) {
                        a.session = None;
                    }
                }
            }
        }
    }

    // ---- voces, salida, vista previa ----------------------------------------

    pub(crate) fn request_voices(&mut self, config: &AppConfig, explicit: bool) {
        self.voices_loaded = true;
        let tx = self.events_tx.clone();
        match self.backend.clone() {
            Backend::Demo { .. } => {
                let _ = tx.send(ReaderEvent::Voices {
                    explicit,
                    result: Ok(tts::builtin_voices()),
                });
            }
            Backend::Real {
                provider,
                credentials,
                allow_env,
            } => {
                let config = config.clone();
                let mode = config.tts.provider;
                self.runtime.spawn(async move {
                    let cancel = CancellationToken::new();
                    let result = async {
                        let bearer = credentials
                            .bearer_for(&config, mode, allow_env, &cancel)
                            .await?;
                        provider.list_voices(&bearer, &cancel).await
                    }
                    .await
                    .map_err(|e| e.to_string());
                    let _ = tx.send(ReaderEvent::Voices { explicit, result });
                });
            }
        }
    }
    pub(crate) fn ensure_voices(&mut self, config: &AppConfig) {
        if !self.voices_loaded {
            self.request_voices(config, false);
        }
    }
    /// Ajustes guardados: aplica volumen al instante. Frecuencia y salida se
    /// aplican a la próxima lectura, para no cortar la actual.
    pub(crate) fn on_config(&mut self, config: &AppConfig) {
        if let Some(p) = &self.player {
            p.set_volume(config.tts.volume);
        }
        if self.active.is_none() {
            let key = (config.tts.sample_rate, config.tts.output_device.clone());
            if self.player_key.as_ref() != Some(&key) {
                self.player = None;
                self.player_key = None;
            }
        }
    }
    pub(crate) fn refresh_outputs(&self) {
        if self.demo {
            return;
        }
        let tx = self.events_tx.clone();
        self.runtime.spawn_blocking(move || {
            let _ = tx.send(ReaderEvent::Outputs(
                dicta_platform::playback::enumerate_outputs().map_err(|e| e.to_string()),
            ));
        });
    }
    fn stop_preview(&mut self) {
        self.preview = None;
    }
    fn preview(&mut self, voice: String, snap: &mut AppSnapshot) {
        if self.preview.as_ref().is_some_and(|p| p.voice == voice) {
            self.stop_preview();
            snap.tts.previewing_voice = None;
            return;
        }
        self.stop_preview();
        if let Some(p) = &self.player {
            p.pause();
        }
        let mut settings = snap.config.tts.voice_settings();
        settings.voice = voice.clone();
        let key = settings.key();
        let request = self.id();
        snap.tts.previewing_voice = Some(voice.clone());
        self.preview = Some(Preview {
            request,
            voice: voice.clone(),
            player: None,
        });
        if let Some((pcm, rate)) = self.preview_cache.get(&(voice.clone(), key)).cloned() {
            self.play_preview(request, &pcm, rate, snap);
            return;
        }
        let name = snap
            .tts
            .voices
            .iter()
            .find(|v| v.id == voice)
            .map_or(voice.clone(), |v| v.name.clone());
        let text = format!("Hola, soy {name}. Así suena esta voz en Mars voice.");
        let tx = self.events_tx.clone();
        let backend = self.backend.clone();
        let config = snap.config.clone();
        self.runtime.spawn(async move {
            let cancel = CancellationToken::new();
            let rate = settings.sample_rate;
            let request_body = TtsRequest {
                settings,
                with_timestamps: false,
            };
            let result = synthesize(
                backend,
                &request_body,
                &config,
                config.tts.provider,
                &text,
                &cancel,
            )
            .await
            .map(|audio| (bytes_to_i16(&audio.pcm), rate))
            .map_err(|e| e.to_string());
            let _ = tx.send(ReaderEvent::Preview {
                request,
                voice,
                key,
                result,
            });
        });
    }
    fn play_preview(&mut self, request: u64, pcm: &[i16], rate: u32, snap: &mut AppSnapshot) {
        let player: Result<Box<dyn AudioOut>, String> = if self.demo {
            Ok(Box::new(VirtualPlayer::new(rate, self.clock.clone())))
        } else {
            Player::new(PlayerConfig {
                sample_rate: rate,
                device: snap.config.tts.output_device.clone(),
                volume: snap.config.tts.volume,
                rate: 1.0,
            })
            .map(|p| Box::new(p) as Box<dyn AudioOut>)
            .map_err(|e| format!("No se pudo abrir la salida de audio: {e}"))
        };
        match player {
            Ok(player) => {
                player.append(pcm);
                player.set_finished(true);
                player.play();
                if let Some(p) = self.preview.as_mut().filter(|p| p.request == request) {
                    p.player = Some(player);
                }
            }
            Err(e) => {
                self.preview = None;
                snap.tts.previewing_voice = None;
                snap.tts.notice = Some(e);
            }
        }
    }
    fn voice_label(&self, voice: &str, snap: &AppSnapshot) -> String {
        snap.tts
            .voices
            .iter()
            .find(|v| v.id == voice)
            .map_or_else(|| voice.to_string(), |v| v.name.clone())
    }

    // ---- historial y exportación --------------------------------------------

    fn replay(&mut self, id: i64, snap: &mut AppSnapshot, store: &Store, paths: &AppPaths) {
        let reading = match store.reading(id) {
            Ok(Some(r)) => r,
            Ok(None) => {
                self.show_error(snap, "La lectura ya no está en el historial");
                return;
            }
            Err(e) => {
                self.show_error(snap, &e.to_string());
                return;
            }
        };
        let layout = store.reading_audio_layout(id).ok().flatten();
        if let (Some(name), Some(layout), false) =
            (reading.audio_filename.clone(), layout, self.demo)
        {
            let Ok(layout) = serde_json::from_str::<AudioLayout>(&layout) else {
                self.regenerate_from_history(reading, snap, store, paths);
                return;
            };
            self.finish_active(snap, store, paths);
            let request = self.id();
            self.replay_request = Some(request);
            snap.tts.show = true;
            let tx = self.events_tx.clone();
            let dir = paths.data.clone();
            self.runtime.spawn_blocking(move || {
                let result = read_history_wav(&dir, &name).and_then(|(pcm, rate)| {
                    if rate == layout.sample_rate
                        && layout.chunks.iter().sum::<u64>() <= pcm.len() as u64
                    {
                        Ok((pcm, rate, layout))
                    } else {
                        Err("El audio guardado no coincide con su lectura".into())
                    }
                });
                let _ = tx.send(ReaderEvent::ReplayAudio {
                    request,
                    history_id: id,
                    text: reading.text,
                    voice: reading.voice,
                    chars_sent: reading.chars_sent,
                    result,
                });
            });
        } else {
            self.regenerate_from_history(reading, snap, store, paths);
        }
    }
    fn regenerate_from_history(
        &mut self,
        reading: dicta_core::Reading,
        snap: &mut AppSnapshot,
        store: &Store,
        paths: &AppPaths,
    ) {
        let prepared = prepared_from_clean(&reading.text);
        let Some(prepared) = prepared else {
            self.show_error(snap, "La lectura guardada no tiene texto legible");
            return;
        };
        self.finish_active(snap, store, paths);
        let prepared = Arc::new(prepared);
        let history = HistoryTarget::Existing {
            id: reading.id,
            chars_sent: reading.chars_sent,
        };
        let confirm = snap.config.tts.confirm_chars as usize;
        if confirm > 0 && prepared.chars() >= confirm {
            snap.tts.confirm = Some(TtsConfirm {
                chars: prepared.chars() as u64,
                estimated_usd: tts::estimate_usd(prepared.chars() as u64),
                truncated: false,
            });
            snap.tts.show = true;
            snap.tts.text = Arc::new(prepared.text.clone());
            snap.tts.sentences = Arc::new(prepared.sentences.clone());
            self.pending = Some(Pending { prepared, history });
            return;
        }
        self.start_reading(prepared, history, snap, store, None);
    }
    /// Carga en la versión inicial el audio guardado, sin volver a generarlo.
    fn load_saved(&mut self, saved: Prefill) {
        let Some(a) = self.active.as_mut() else {
            return;
        };
        let layout = saved.layout;
        if layout.text_key != tts::fnv1a(a.prepared.text.as_bytes())
            || layout.chunks.len() > a.prepared.chunks.len()
            || layout.chunks.iter().sum::<u64>() > saved.pcm.len() as u64
        {
            return;
        }
        a.history_saved = true;
        a.versions[0].label = format!("Guardada · {}", a.versions[0].label);
        let mut offset = 0usize;
        for (k, len) in layout.chunks.iter().enumerate() {
            let len = *len as usize;
            let duration_ms = samples_to_ms(len as u64, saved.rate);
            let span = a.prepared.chunks[k].span;
            let words = tts::proportional_words(a.prepared.chunk_text(k), duration_ms)
                .into_iter()
                .map(|mut w| {
                    w.span.start += span.start;
                    w.span.end += span.start;
                    w
                })
                .collect();
            a.versions[0].chunks[k] = Some(ChunkAudio {
                pcm: Arc::new(saved.pcm[offset..offset + len].to_vec()),
                words,
                duration_ms,
            });
            offset += len;
        }
        a.layout_dirty = true;
        if let Some(player) = self.player.as_deref() {
            rebuild_run(a, player, 0, 0);
        }
    }
    fn export(&mut self, path: PathBuf, snap: &mut AppSnapshot) {
        let Some(a) = &self.active else {
            snap.tts.notice = Some("No hay una lectura para guardar".into());
            return;
        };
        let v = a.version();
        let parts: Vec<Arc<Vec<i16>>> = v.chunks.iter().flatten().map(|c| c.pcm.clone()).collect();
        if parts.is_empty() {
            snap.tts.notice = Some("Todavía no hay audio generado para guardar".into());
            return;
        }
        let complete = v.chunks.iter().all(Option::is_some);
        let rate = a.player_rate;
        let tx = self.events_tx.clone();
        let demo = self.demo;
        self.runtime.spawn_blocking(move || {
            let result = if demo {
                Ok("Demo: guardado simulado".to_string())
            } else {
                write_wav_file(&path, &parts, rate, 0o644)
                    .map(|_| {
                        let name = path
                            .file_name()
                            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                        if complete {
                            format!("Audio guardado en {name}")
                        } else {
                            format!("Se guardó la parte ya generada en {name}")
                        }
                    })
                    .map_err(|e| format!("No se pudo guardar el audio: {e}"))
            };
            let _ = tx.send(ReaderEvent::Notice(result));
        });
    }

    // ---- ciclo ---------------------------------------------------------------

    /// Procesa eventos, programa generación y actualiza `snapshot.tts`.
    /// Devuelve true si cambió algo visible.
    pub(crate) fn poll(&mut self, snap: &mut AppSnapshot, store: &Store, paths: &AppPaths) -> bool {
        let mut dirty = false;
        while let Ok(event) = self.events.try_recv() {
            dirty = true;
            match event {
                ReaderEvent::Text {
                    request,
                    empty,
                    result,
                } => {
                    if self.read_request != Some(request) {
                        continue;
                    }
                    self.read_request = None;
                    match result {
                        Ok(Some((text, origin))) => {
                            self.accept_text(text, HistoryTarget::New, snap, store, paths);
                            if origin == TextOrigin::ClipboardFallback && self.active.is_some() {
                                snap.tts.notice = Some(
                                    "No había selección primaria: se leyó el portapapeles".into(),
                                );
                            }
                        }
                        Ok(None) => self.show_error(snap, empty),
                        Err(e) => self.show_error(snap, &e),
                    }
                }
                ReaderEvent::Wire {
                    reading,
                    version,
                    session,
                    event,
                } => self.on_wire(reading, version, session, event),
                ReaderEvent::SessionEnd {
                    reading,
                    session,
                    result,
                } => self.on_session_end(reading, session, result),
                ReaderEvent::Rest {
                    reading,
                    version,
                    session,
                    chunk,
                    result,
                } => self.on_rest(reading, version, session, chunk, result),
                ReaderEvent::Voices { explicit, result } => match result {
                    Ok(mut voices) if !voices.is_empty() => {
                        // xAI marca sus voces como "multilingual" (o "en" en la
                        // documentación); todas hablan los idiomas documentados.
                        for v in voices.iter_mut().filter(|v| !v.custom) {
                            if v.language.is_empty()
                                || v.language.eq_ignore_ascii_case("en")
                                || v.language.eq_ignore_ascii_case("multilingual")
                            {
                                v.language = "multilingüe".into();
                            }
                        }
                        snap.tts.voices = Arc::new(voices);
                        if explicit {
                            snap.tts.notice = Some("Lista de voces actualizada".into());
                        }
                        self.sync_versions(snap);
                    }
                    Ok(_) => {}
                    Err(e) => {
                        self.voices_loaded = false;
                        if explicit {
                            snap.tts.notice = Some(e);
                        }
                    }
                },
                ReaderEvent::Outputs(result) => match result {
                    Ok(outputs) => snap.tts.outputs = outputs,
                    Err(e) => snap.tts.notice = Some(e),
                },
                ReaderEvent::Preview {
                    request,
                    voice,
                    key,
                    result,
                } => {
                    let current = self.preview.as_ref().is_some_and(|p| p.request == request);
                    match result {
                        Ok((pcm, rate)) => {
                            let pcm = Arc::new(pcm);
                            self.preview_cache.insert((voice, key), (pcm.clone(), rate));
                            if current {
                                self.play_preview(request, &pcm, rate, snap);
                            }
                        }
                        Err(e) => {
                            if current {
                                self.preview = None;
                                snap.tts.previewing_voice = None;
                                snap.tts.notice = Some(e);
                            }
                        }
                    }
                }
                ReaderEvent::Notice(result) => match result {
                    Ok(message) => snap.tts.notice = Some(message),
                    Err(e) => snap.tts.notice = Some(e),
                },
                ReaderEvent::SavedAudio { history_id, result } => match result {
                    Ok((name, bytes, layout)) => {
                        match store.set_reading_audio(history_id, &name, bytes, &layout) {
                            Ok(previous) => {
                                if let Some(previous) = previous {
                                    let _ = history_ops::remove_audio(paths, &previous);
                                }
                                let _ = history_ops::refresh(store, snap);
                            }
                            Err(e) => {
                                let _ = history_ops::remove_audio(paths, &name);
                                snap.tts.notice = Some(e.to_string());
                            }
                        }
                    }
                    Err(e) => snap.tts.notice = Some(format!("No se guardó el audio: {e}")),
                },
                ReaderEvent::ReplayAudio {
                    request,
                    history_id,
                    text,
                    voice,
                    chars_sent,
                    result,
                } => {
                    if self.replay_request != Some(request) {
                        continue;
                    }
                    self.replay_request = None;
                    match result {
                        Ok((pcm, rate, layout)) => match prepared_from_clean(&text) {
                            Some(prepared) => self.start_reading(
                                Arc::new(prepared),
                                HistoryTarget::Existing {
                                    id: history_id,
                                    chars_sent,
                                },
                                snap,
                                store,
                                Some(Prefill {
                                    voice,
                                    pcm,
                                    rate,
                                    layout,
                                }),
                            ),
                            None => {
                                self.show_error(snap, "La lectura guardada no tiene texto legible")
                            }
                        },
                        Err(e) => self.show_error(snap, &e),
                    }
                }
            }
        }
        let config = snap.config.clone();
        if self.active.is_some() {
            self.pump(&config);
        }
        if let Some(preview) = &self.preview {
            if preview
                .player
                .as_ref()
                .is_some_and(|p| p.status().ended || p.status().error.is_some())
            {
                self.preview = None;
                snap.tts.previewing_voice = None;
                dirty = true;
            }
        }
        dirty |= self.sync_view(snap);
        // Fin de la lectura: cola, cierre automático e historial.
        let phase = snap.tts.phase;
        if phase == TtsPhase::Finished {
            let first = match self.active.as_mut() {
                Some(a) if a.finished_at.is_none() => {
                    a.finished_at = Some(Instant::now());
                    true
                }
                _ => false,
            };
            if first {
                if let Some(mut a) = self.active.take() {
                    self.save_history(&mut a, snap, store, paths);
                    self.active = Some(a);
                }
                if let Some(next) = self.queue.pop_front() {
                    self.accept_text(next, HistoryTarget::New, snap, store, paths);
                    return true;
                }
            }
            let close_after = u64::from(config.tts.auto_close_seconds);
            if close_after > 0
                && self
                    .active
                    .as_ref()
                    .and_then(|a| a.finished_at)
                    .is_some_and(|t| t.elapsed() >= Duration::from_secs(close_after))
            {
                self.stop(snap, store, paths);
                return true;
            }
        } else if let Some(a) = self.active.as_mut() {
            if phase != TtsPhase::Finished {
                a.finished_at = None;
            }
        }
        dirty
    }
    fn sync_versions(&self, snap: &mut AppSnapshot) {
        let Some(a) = &self.active else {
            return;
        };
        snap.tts.versions = a
            .versions
            .iter()
            .map(|v| TtsVersion {
                id: v.id,
                voice: v.settings.voice.clone(),
                label: v.label.clone(),
            })
            .collect();
        snap.tts.active_version = a.version().id;
        snap.tts.voice = a.version().settings.voice.clone();
    }
    /// Copia al snapshot el estado derivado del reproductor y la caché.
    fn sync_view(&mut self, snap: &mut AppSnapshot) -> bool {
        let mut dirty = false;
        let phase = self.phase();
        if phase != self.last_phase || snap.tts.phase != phase {
            snap.tts.phase = phase;
            snap.tts.status = phase.label().into();
            self.last_phase = phase;
            dirty = true;
        }
        let status = self.player.as_ref().map(|p| p.status());
        if let Some(st) = &status {
            if let Some(error) = &st.error {
                if snap.tts.error.as_deref() != Some(error) {
                    snap.tts.error = Some(error.clone());
                    dirty = true;
                }
            }
            if st.level_seq != self.last_level_seq {
                self.last_level_seq = st.level_seq;
                snap.tts.level = st.level;
                snap.tts.level_seq = snap.tts.level_seq.wrapping_add(1);
                dirty = true;
            }
        }
        if let Some(error) = self.active.as_ref().and_then(|a| a.error.clone()) {
            if snap.tts.error.as_ref() != Some(&error) {
                snap.tts.error = Some(error);
                dirty = true;
            }
        } else if self.active.is_none() {
            let error = self.idle_error.clone();
            if snap.tts.error != error && self.pending.is_none() {
                snap.tts.error = error;
                dirty = true;
            }
        }
        let position = self.position_ms();
        let Some(a) = self.active.as_mut() else {
            return dirty;
        };
        if a.layout_dirty {
            a.layout_dirty = false;
            let mut words = vec![];
            let mut generated: Vec<(u64, u64)> = vec![];
            let per_char = a.ms_per_char();
            let mut offset = 0;
            for k in 0..a.prepared.chunks.len() {
                let len = a.chunk_ms(k, per_char);
                if let Some(c) = &a.version().chunks[k] {
                    words.extend(c.words.iter().map(|w| WordTiming {
                        span: w.span,
                        start_ms: w.start_ms + offset,
                        end_ms: w.end_ms + offset,
                    }));
                    match generated.last_mut() {
                        Some(last) if last.1 == offset => last.1 = offset + len,
                        _ => generated.push((offset, offset + len)),
                    }
                }
                offset += len;
            }
            snap.tts.words = Arc::new(words);
            snap.tts.generated = Arc::new(generated);
            snap.tts.total_ms = offset;
            snap.tts.total_exact = a.version().chunks.iter().all(Option::is_some);
            dirty = true;
        }
        let generating = a.generating();
        if snap.tts.generating != generating {
            snap.tts.generating = generating;
            dirty = true;
        }
        if snap.tts.chars_sent != a.chars_sent {
            snap.tts.chars_sent = a.chars_sent;
            snap.tts.estimated_usd = tts::estimate_usd(a.chars_sent);
            dirty = true;
        }
        if snap.tts.queued != self.queue.len() {
            snap.tts.queued = self.queue.len();
            dirty = true;
        }
        if position.abs_diff(self.last_position) >= 40 || position == 0 && self.last_position != 0 {
            self.last_position = position;
            snap.tts.position_ms = position;
            dirty = true;
        }
        let word = tts::word_at(&snap.tts.words, position).filter(|i| {
            let w = snap.tts.words[*i];
            position <= w.end_ms + 1500
        });
        if word != snap.tts.current_word {
            snap.tts.current_word = word;
            dirty = true;
        }
        let sentence = match word {
            Some(i) => tts::sentence_at(&snap.tts.sentences, snap.tts.words[i].span.start),
            None => self.current_sentence(position),
        };
        if sentence != snap.tts.current_sentence {
            snap.tts.current_sentence = sentence;
            dirty = true;
        }
        dirty
    }
    /// Quit: cancela todo y guarda el historial.
    pub(crate) fn shutdown(&mut self, snap: &mut AppSnapshot, store: &Store, paths: &AppPaths) {
        self.finish_active(snap, store, paths);
        self.player = None;
        self.preview = None;
    }
}

// ---------------------------------------------------------------------------
// Funciones auxiliares
// ---------------------------------------------------------------------------

fn samples_to_ms(samples: u64, rate: u32) -> u64 {
    samples * 1000 / u64::from(rate.max(1))
}
fn ms_to_samples(ms: u64, rate: u32) -> u64 {
    ms * u64::from(rate) / 1000
}
fn bytes_to_i16(bytes: &[u8]) -> Vec<i16> {
    bytes
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect()
}
/// Cierra la sesión actual. Con `cancel`, el proveedor deja de generar.
fn drop_session(a: &mut Active, cancel: bool) {
    if let Some(s) = a.session.take() {
        if cancel {
            s.cancel.cancel();
        }
        uncount_unsent(a, &s);
    }
}
/// El proveedor toma una locución por vez: solo la primera de la cola pudo
/// llegar a xAI. Las demás no se enviaron y no cuentan como coste estimado.
fn uncount_unsent(a: &mut Active, s: &Session) {
    for k in s.queued.iter().skip(1) {
        a.chars_sent = a.chars_sent.saturating_sub(a.chunk_chars(*k) as u64);
    }
}
/// Recarga el reproductor desde el fragmento `k`, con los fragmentos ya generados.
fn rebuild_run(a: &mut Active, player: &dyn AudioOut, k: usize, within_ms: u64) {
    player.reset();
    a.run_start = k;
    a.run_end = k;
    a.streaming = None;
    extend_run(a, player);
    if a.run_end > k {
        player.seek(ms_to_samples(within_ms, a.player_rate));
    }
}
/// Agrega fragmentos contiguos ya generados al final del tramo.
fn extend_run(a: &mut Active, player: &dyn AudioOut) {
    let n = a.prepared.chunks.len();
    while a.run_end < n {
        let Some(c) = &a.versions[a.active].chunks[a.run_end] else {
            break;
        };
        player.append(&c.pcm);
        a.run_end += 1;
    }
    player.set_finished(a.run_end == n);
}
/// Frases y fragmentos de un texto ya limpio (historial), sin volver a limpiarlo.
fn prepared_from_clean(text: &str) -> Option<PreparedText> {
    let sentences = tts::split_sentences(text);
    if sentences.is_empty() {
        return None;
    }
    let chunks = tts::plan_chunks(text, &sentences);
    Some(PreparedText {
        text: text.to_string(),
        original_chars: text.chars().count(),
        sentences,
        chunks,
        truncated: false,
    })
}
#[allow(clippy::too_many_arguments)]
fn spawn_session(
    runtime: &Handle,
    backend: Backend,
    events: Sender<ReaderEvent>,
    reading: u64,
    version: u64,
    session: u64,
    request: TtsRequest,
    config: AppConfig,
    mode: ProviderMode,
    utterances: tokio::sync::mpsc::Receiver<(u64, String)>,
    cancel: CancellationToken,
) {
    runtime.spawn(async move {
        let (wire_tx, mut wire_rx) = tokio::sync::mpsc::channel(64);
        let forward_events = events.clone();
        let forward = tokio::spawn(async move {
            while let Some(event) = wire_rx.recv().await {
                if forward_events
                    .send(ReaderEvent::Wire {
                        reading,
                        version,
                        session,
                        event,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        let result = {
            let wire_tx = wire_tx;
            match backend {
                Backend::Real {
                    provider,
                    credentials,
                    allow_env,
                } => match credentials
                    .bearer_for(&config, mode, allow_env, &cancel)
                    .await
                {
                    Ok(bearer) => {
                        provider
                            .tts_session(&request, &bearer, utterances, wire_tx, cancel)
                            .await
                    }
                    Err(e) => Err(e),
                },
                Backend::Demo { chunk_delay } => {
                    demo_session(&request, chunk_delay, utterances, wire_tx, cancel).await
                }
            }
        };
        // Todo el audio llega antes del fin de sesión.
        let _ = forward.await;
        let _ = events.send(ReaderEvent::SessionEnd {
            reading,
            session,
            result,
        });
    });
}
#[allow(clippy::too_many_arguments)]
fn spawn_rest(
    runtime: &Handle,
    backend: Backend,
    events: Sender<ReaderEvent>,
    reading: u64,
    version: u64,
    session: u64,
    chunk: usize,
    request: TtsRequest,
    config: AppConfig,
    mode: ProviderMode,
    text: String,
    cancel: CancellationToken,
) {
    runtime.spawn(async move {
        let result = synthesize(backend, &request, &config, mode, &text, &cancel).await;
        let _ = events.send(ReaderEvent::Rest {
            reading,
            version,
            session,
            chunk,
            result,
        });
    });
}
async fn synthesize(
    backend: Backend,
    request: &TtsRequest,
    config: &AppConfig,
    mode: ProviderMode,
    text: &str,
    cancel: &CancellationToken,
) -> Result<TtsAudio, ProviderError> {
    match backend {
        Backend::Real {
            provider,
            credentials,
            allow_env,
        } => {
            let bearer = credentials
                .bearer_for(config, mode, allow_env, cancel)
                .await?;
            provider.synthesize(request, &bearer, text, cancel).await
        }
        Backend::Demo { chunk_delay } => {
            tokio::select! {
                _ = cancel.cancelled() => return Err(ProviderError::Cancelled),
                _ = tokio::time::sleep(chunk_delay) => {}
            }
            let (pcm, marks) =
                demo_audio(text, request.settings.sample_rate, request.settings.speed);
            Ok(TtsAudio { pcm, marks })
        }
    }
}
/// Sesión simulada con el mismo contrato que `tts_session`.
async fn demo_session(
    request: &TtsRequest,
    chunk_delay: Duration,
    mut utterances: tokio::sync::mpsc::Receiver<(u64, String)>,
    events: tokio::sync::mpsc::Sender<TtsEvent>,
    cancel: CancellationToken,
) -> Result<TtsSessionEnd, ProviderError> {
    let send = |event| {
        let events = events.clone();
        async move {
            events
                .send(event)
                .await
                .map_err(|_| ProviderError::ConsumerClosed)
        }
    };
    send(TtsEvent::Ready).await?;
    loop {
        let next = tokio::select! {
            _ = cancel.cancelled() => return Err(ProviderError::Cancelled),
            next = utterances.recv() => next,
        };
        let Some((id, text)) = next else {
            return Ok(TtsSessionEnd::Finished);
        };
        tokio::select! {
            _ = cancel.cancelled() => return Err(ProviderError::Cancelled),
            _ = tokio::time::sleep(chunk_delay) => {}
        }
        let (pcm, marks) = demo_audio(&text, request.settings.sample_rate, request.settings.speed);
        // Cuatro entregas para ejercitar el streaming del reproductor.
        let part = (pcm.len() / 4 / 2).max(1) * 2;
        for piece in pcm.chunks(part) {
            send(TtsEvent::Audio {
                id,
                pcm: piece.to_vec(),
            })
            .await?;
        }
        send(TtsEvent::Marks { id, marks }).await?;
        send(TtsEvent::Done { id }).await?;
    }
}
/// Audio sintético de la demo: pulsos suaves por palabra con marcas por carácter.
fn demo_audio(text: &str, sample_rate: u32, speed: f32) -> (Vec<u8>, Vec<CharTime>) {
    let duration = tts::estimate_ms(text.chars().count(), speed).max(200);
    let words = tts::proportional_words(text, duration);
    let total = ms_to_samples(duration, sample_rate) as usize;
    let mut pcm = vec![0i16; total];
    let mut marks = vec![];
    for (i, w) in words.iter().enumerate() {
        let start = ms_to_samples(w.start_ms, sample_rate) as usize;
        let end = (ms_to_samples(w.end_ms, sample_rate) as usize).min(total);
        let len = end.saturating_sub(start).max(1);
        let freq = 170.0 + 15.0 * (i % 5) as f32;
        for (j, sample) in pcm[start..end].iter_mut().enumerate() {
            let t = j as f32 / sample_rate as f32;
            let envelope = (std::f32::consts::PI * j as f32 / len as f32).sin();
            *sample = (envelope
                * 0.2
                * (2.0 * std::f32::consts::PI * freq * t).sin()
                * f32::from(i16::MAX)) as i16;
        }
        let chars: Vec<char> = text[w.span.start..w.span.end].chars().collect();
        let per = (w.end_ms - w.start_ms) / chars.len().max(1) as u64;
        for (c_index, c) in chars.iter().enumerate() {
            let start_ms = w.start_ms + per * c_index as u64;
            marks.push(CharTime {
                ch: c.to_string(),
                start_ms,
                end_ms: start_ms + per,
            });
        }
    }
    let bytes = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
    (bytes, marks)
}

// ---------------------------------------------------------------------------
// WAV
// ---------------------------------------------------------------------------

/// Escribe un WAV privado en `data/audio`, con nombre generado.
fn write_history_wav(
    data_dir: &Path,
    parts: &[Arc<Vec<i16>>],
    rate: u32,
) -> Result<(String, u64), String> {
    use dicta_core::fsx;
    let dir = data_dir.join("audio");
    match std::fs::symlink_metadata(&dir) {
        Ok(meta) if meta.is_dir() && !fsx::is_link(&meta) => {}
        Ok(_) => return Err("La carpeta de audio no es válida".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            fsx::ensure_private_dir(&dir).map_err(|e| e.to_string())?;
        }
        Err(e) => return Err(e.to_string()),
    }
    let name = format!(
        "tts-{}-{}-{}.wav",
        history_ops::now_ms(),
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    );
    let path = dir.join(&name);
    match write_wav_file(&path, parts, rate, 0o600) {
        Ok(bytes) => Ok((name, bytes)),
        Err(e) => {
            let _ = std::fs::remove_file(&path);
            Err(e.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dicta_core::AppConfig;

    struct Harness {
        reader: Reader,
        snap: AppSnapshot,
        store: Store,
        paths: AppPaths,
        clock: Arc<AtomicU64>,
        _runtime: tokio::runtime::Runtime,
        root: PathBuf,
    }
    impl Harness {
        fn new(config: AppConfig) -> Self {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();
            let clock = Arc::new(AtomicU64::new(0));
            let reader = Reader::with_backend(
                runtime.handle().clone(),
                Backend::Demo {
                    chunk_delay: Duration::from_millis(5),
                },
                true,
                Clock::Manual(clock.clone()),
            );
            let root = std::env::temp_dir().join(format!(
                "mars-reader-{}-{}",
                std::process::id(),
                SERIAL.fetch_add(1, Ordering::Relaxed)
            ));
            let paths = AppPaths {
                config: root.join("config"),
                data: root.join("data"),
                runtime: root.join("run"),
            };
            let mut snap = AppSnapshot::new(config);
            snap.demo = true;
            Self {
                reader,
                snap,
                store: Store::in_memory().unwrap(),
                paths,
                clock,
                _runtime: runtime,
                root,
            }
        }
        fn send(&mut self, command: Command) -> Outcome {
            self.send_busy(command, false)
        }
        fn send_busy(&mut self, command: Command, busy: bool) -> Outcome {
            match self
                .reader
                .handle(command, &mut self.snap, &self.store, &self.paths, busy)
            {
                Ok(outcome) => outcome,
                Err(_) => panic!("comando no es de lectura"),
            }
        }
        fn poll(&mut self) {
            self.reader.poll(&mut self.snap, &self.store, &self.paths);
        }
        fn advance(&mut self, ms: u64) {
            self.clock.fetch_add(ms, Ordering::AcqRel);
            self.poll();
        }
        /// Espera tareas reales (demo) sin avanzar el reloj de reproducción.
        fn until(&mut self, what: &str, predicate: impl Fn(&AppSnapshot, &Reader) -> bool) {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                self.poll();
                if predicate(&self.snap, &self.reader) {
                    return;
                }
                assert!(Instant::now() < deadline, "timeout esperando {what}");
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
    impl Drop for Harness {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
    fn long_text(sentences: usize) -> String {
        (0..sentences)
            .map(|i| {
                format!("Esta es la frase número {i} de una lectura de prueba bastante larga.")
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
    fn generated_chunks(r: &Reader) -> usize {
        r.active
            .as_ref()
            .map_or(0, |a| a.version().chunks.iter().flatten().count())
    }

    #[test]
    fn demo_selection_streams_plays_and_finishes_with_history() {
        let mut h = Harness::new(AppConfig::default());
        let outcome = h.send(Command::ReadSelection);
        assert!(outcome.stop_note_audio);
        h.until("primer audio", |s, _| {
            s.tts.phase == TtsPhase::Playing && !s.tts.words.is_empty()
        });
        assert!(h.snap.tts.show);
        assert_eq!(&*h.snap.tts.text, DEMO_TEXT);
        assert!(h.snap.tts.chars_sent > 0);
        h.until("todo generado", |s, _| s.tts.total_exact);
        let total = h.snap.tts.total_ms;
        h.advance(1000);
        assert!(h.snap.tts.position_ms >= 900);
        assert!(h.snap.tts.current_word.is_some());
        h.advance(total);
        assert_eq!(h.snap.tts.phase, TtsPhase::Finished);
        let readings = h.store.readings("", 10).unwrap();
        assert_eq!(readings.len(), 1);
        assert_eq!(readings[0].chars_sent, h.snap.tts.chars_sent);
        assert!(readings[0].duration_ms > 0);
        // Toggle con la lectura terminada vuelve a empezar.
        h.send(Command::TtsToggle);
        h.poll();
        assert_eq!(h.snap.tts.phase, TtsPhase::Playing);
        assert!(h.snap.tts.position_ms < 100);
    }
    #[test]
    fn lookahead_window_does_not_send_text_far_ahead_of_playback() {
        let mut config = AppConfig::default();
        config.tts.lookahead_seconds = 30;
        config.tts.confirm_chars = 0;
        let mut h = Harness::new(config);
        h.send(Command::ReadText(long_text(200)));
        h.until("ventana generada", |_, r| {
            r.active.as_ref().is_some_and(|a| !a.generating()) && generated_chunks(r) > 0
        });
        let total_chars = h.snap.tts.chars;
        assert!(
            h.snap.tts.chars_sent < total_chars / 3,
            "envió {} de {}",
            h.snap.tts.chars_sent,
            total_chars
        );
        let sent = h.snap.tts.chars_sent;
        // Al avanzar la reproducción se genera más.
        h.advance(40_000);
        h.until("más generación", |s, _| s.tts.chars_sent > sent);
        // Detener a la mitad no envía el resto.
        h.send(Command::TtsStop);
        assert!(!h.snap.tts.show);
        let saved = h.store.readings("", 1).unwrap();
        assert!(saved[0].chars_sent < total_chars);
    }
    #[test]
    fn long_text_requires_confirmation_with_estimated_cost() {
        let mut config = AppConfig::default();
        config.tts.confirm_chars = 1000;
        let mut h = Harness::new(config);
        h.send(Command::ReadText(long_text(40)));
        h.poll();
        assert_eq!(h.snap.tts.phase, TtsPhase::Confirm);
        let confirm = h.snap.tts.confirm.unwrap();
        assert!(confirm.chars >= 1000 && confirm.estimated_usd > 0.0);
        assert_eq!(h.snap.tts.chars_sent, 0);
        assert!(h.store.readings("", 10).unwrap().is_empty());
        h.send(Command::TtsStop);
        h.poll();
        assert_eq!(h.snap.tts.phase, TtsPhase::Idle);
        h.send(Command::ReadText(long_text(40)));
        h.send(Command::ConfirmReading);
        h.until("audio tras confirmar", |s, _| s.tts.chars_sent > 0);
    }
    #[test]
    fn same_text_toggles_and_new_text_replaces_or_queues() {
        let mut h = Harness::new(AppConfig::default());
        h.send(Command::ReadText("Primera lectura breve.".into()));
        h.until("reproduciendo", |s, _| s.tts.phase == TtsPhase::Playing);
        h.send(Command::ReadText("Primera lectura breve.".into()));
        h.poll();
        assert_eq!(h.snap.tts.phase, TtsPhase::Paused);
        h.send(Command::ReadText("Primera   lectura breve.".into()));
        h.poll();
        assert_eq!(
            h.snap.tts.phase,
            TtsPhase::Playing,
            "el texto limpio es el mismo"
        );
        let first = h.snap.tts.reading;
        h.send(Command::ReadText("Otra lectura distinta.".into()));
        h.poll();
        assert_ne!(h.snap.tts.reading, first);
        assert_eq!(&*h.snap.tts.text, "Otra lectura distinta.");

        let mut config = AppConfig::default();
        config.tts.new_text = NewTextAction::Queue;
        let mut q = Harness::new(config);
        q.send(Command::ReadText("Uno.".into()));
        q.until("reproduciendo", |s, _| s.tts.phase == TtsPhase::Playing);
        q.send(Command::ReadText("Dos.".into()));
        q.poll();
        assert_eq!(q.snap.tts.queued, 1);
        q.until("generado", |s, _| s.tts.total_exact);
        let total = q.snap.tts.total_ms;
        q.advance(total + 10);
        q.until("siguiente de la cola", |s, _| &*s.tts.text == "Dos.");
        assert_eq!(q.snap.tts.queued, 0);
    }
    #[test]
    fn seeking_and_sentence_steps_move_within_generated_audio() {
        let mut config = AppConfig::default();
        config.tts.lookahead = Lookahead::All;
        config.tts.confirm_chars = 0;
        let mut h = Harness::new(config);
        h.send(Command::ReadText(long_text(12)));
        h.until("todo generado", |s, _| s.tts.total_exact);
        h.send(Command::TtsSeek(10_000));
        h.poll();
        assert!(h.snap.tts.position_ms.abs_diff(10_000) < 50);
        h.send(Command::TtsSkip(-5));
        h.poll();
        assert!(h.snap.tts.position_ms.abs_diff(5_000) < 50);
        let sentence = h.snap.tts.current_sentence.unwrap();
        h.send(Command::TtsSentence(1));
        h.poll();
        assert_eq!(h.snap.tts.current_sentence, Some(sentence + 1));
        // Clic en una palabra de la última frase.
        let last = *h.snap.tts.sentences.last().unwrap();
        h.send(Command::TtsSeekText(last.start + 2));
        h.poll();
        assert_eq!(
            h.snap.tts.current_sentence,
            Some(h.snap.tts.sentences.len() - 1)
        );
        h.send(Command::TtsSkip(-1000));
        h.poll();
        assert_eq!(h.snap.tts.position_ms, 0);
    }
    #[test]
    fn regenerate_creates_a_version_from_the_current_sentence_and_can_switch_back() {
        let mut config = AppConfig::default();
        config.tts.lookahead = Lookahead::All;
        config.tts.confirm_chars = 0;
        let mut h = Harness::new(config);
        h.send(Command::ReadText(long_text(30)));
        h.until("todo generado", |s, _| s.tts.total_exact);
        let first_version = h.snap.tts.active_version;
        let first_sent = h.snap.tts.chars_sent;
        h.send(Command::TtsSeek(h.snap.tts.total_ms / 2));
        h.poll();
        h.send(Command::TtsRegenerate {
            voice: "eve".into(),
        });
        h.poll();
        assert_eq!(h.snap.tts.voice, "eve");
        assert_eq!(h.snap.tts.versions.len(), 2);
        h.until("nueva versión generada", |_, r| {
            r.active.as_ref().is_some_and(|a| !a.generating())
        });
        let regenerated = h.snap.tts.chars_sent - first_sent;
        assert!(
            regenerated > 0 && regenerated < first_sent * 3 / 4,
            "regenera desde la mitad, no todo: {regenerated} de {first_sent}"
        );
        // Volver a la primera versión es gratis.
        h.send(Command::TtsUseVersion(first_version));
        h.poll();
        assert_eq!(h.snap.tts.voice, "carina");
        assert!(h.snap.tts.total_exact);
        let sent = h.snap.tts.chars_sent;
        h.send(Command::TtsRegenerate {
            voice: "eve".into(),
        });
        h.poll();
        assert_eq!(
            h.snap.tts.versions.len(),
            2,
            "reutiliza la versión existente"
        );
        h.poll();
        assert_eq!(h.snap.tts.chars_sent, sent);
        assert!(
            h.send(Command::TtsRegenerate {
                voice: "../x".into()
            }) == Outcome::default()
        );
        assert!(h.snap.tts.notice.is_some());
    }
    #[test]
    fn dictation_pauses_or_stops_and_busy_dictation_refuses_reading() {
        let mut h = Harness::new(AppConfig::default());
        h.send_busy(Command::ReadSelection, true);
        h.poll();
        assert_eq!(h.snap.tts.phase, TtsPhase::Error);
        assert!(h.snap.tts.error.as_deref().unwrap().contains("dictado"));
        h.send(Command::ReadSelection);
        h.until("reproduciendo", |s, _| s.tts.phase == TtsPhase::Playing);
        assert!(h.reader.audible());
        h.reader.on_dictation_start(&mut h.snap, &h.store, &h.paths);
        h.poll();
        assert_eq!(h.snap.tts.phase, TtsPhase::Paused);
        assert!(!h.reader.audible());
        h.snap.config.tts.on_dictation = OnDictation::Stop;
        h.reader.on_dictation_start(&mut h.snap, &h.store, &h.paths);
        h.poll();
        assert_eq!(h.snap.tts.phase, TtsPhase::Idle);
        assert!(!h.snap.tts.show);
    }
    #[test]
    fn stale_events_after_stop_cannot_resurrect_a_reading() {
        let mut h = Harness::new(AppConfig::default());
        h.send(Command::ReadText(long_text(5)));
        h.send(Command::TtsStop);
        std::thread::sleep(Duration::from_millis(100));
        h.poll();
        assert_eq!(h.snap.tts.phase, TtsPhase::Idle);
        assert!(h.snap.tts.words.is_empty());
        assert!(!h.snap.tts.show);
        // Una lectura de selección pendiente también se descarta.
        h.send(Command::ReadSelection);
        h.send(Command::TtsStop);
        std::thread::sleep(Duration::from_millis(30));
        h.poll();
        assert_eq!(h.snap.tts.phase, TtsPhase::Idle);
    }
    #[test]
    fn rate_and_volume_are_clamped_and_remembered() {
        let mut h = Harness::new(AppConfig::default());
        let outcome = h.send(Command::TtsSetRate(9.0));
        assert!(outcome.save_config);
        assert_eq!(h.snap.config.tts.playback_rate, tts::RATE_MAX);
        assert!(h.send(Command::TtsSetVolume(f32::NAN)) == Outcome::default());
        h.snap.config.tts.remember_rate = false;
        assert!(!h.send(Command::TtsSetRate(1.25)).save_config);
        assert_eq!(h.snap.tts.rate, 1.25);
        h.snap.config.validate().unwrap();
    }
    #[test]
    fn untrusted_tags_are_not_sent_and_empty_text_is_actionable() {
        let mut h = Harness::new(AppConfig::default());
        h.send(Command::ReadText("🚀 — ".into()));
        h.poll();
        assert_eq!(h.snap.tts.phase, TtsPhase::Error);
        h.send(Command::ReadText(
            "Hola [laugh] <whisper>mundo</whisper>.".into(),
        ));
        h.poll();
        assert_eq!(&*h.snap.tts.text, "Hola (laugh) mundo.");
    }
    #[test]
    fn wav_roundtrip_and_layout_validation() {
        let dir = std::env::temp_dir().join(format!(
            "mars-wav-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let parts = vec![
            Arc::new(vec![1i16, -2, 3]),
            Arc::new(vec![i16::MAX, i16::MIN]),
        ];
        let (name, bytes) = write_history_wav(&dir, &parts, 24_000).unwrap();
        assert_eq!(bytes, 44 + 10);
        let (pcm, rate) = read_history_wav(&dir, &name).unwrap();
        assert_eq!(pcm, vec![1, -2, 3, i16::MAX, i16::MIN]);
        assert_eq!(rate, 24_000);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join("audio").join(&name))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        assert!(read_history_wav(&dir, "../etc.wav").is_err());
        assert!(crate::wav::parse_wav(b"RIFF....WAVEjunk").is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn alignment_from_demo_marks_highlights_words_in_order() {
        let (pcm, marks) = demo_audio("Hola mundo, qué tal.", 24_000, 1.0);
        let duration = samples_to_ms(pcm.len() as u64 / 2, 24_000);
        let words = tts::align_words("Hola mundo, qué tal.", &marks, duration);
        assert_eq!(words.len(), 4);
        assert!(words.windows(2).all(|w| w[0].end_ms <= w[1].start_ms + 1));
    }
}
