pub mod config;
pub mod fsx;
pub mod history;
pub mod readings;
pub mod store;
pub mod text;
pub mod transcript;
pub mod tts;
pub use config::{
    AppConfig, AppPaths, AudioRetention, ColorMode, ProviderMode, ShortcutMode, Theme,
};
pub use history::{HistoryMetrics, NewNote, Note, NoteStatus, SessionArtifacts, UsagePeriod};
pub use readings::{HistoryKind, NewReading, Reading, ReadingMetrics};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
pub use tts::{TtsConfig, TtsPhase, TtsSnapshot};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    Idle,
    Connecting,
    Recording,
    Finalizing,
    Polishing,
    Ready,
    Error,
}
impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "En espera",
            Self::Connecting => "Conectando",
            Self::Recording => "Grabando",
            Self::Finalizing => "Finalizando",
            Self::Polishing => "Puliendo",
            Self::Ready => "Listo",
            Self::Error => "Error",
        }
    }
    pub fn busy(self) -> bool {
        matches!(
            self,
            Self::Connecting | Self::Recording | Self::Finalizing | Self::Polishing
        )
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioDevice {
    pub id: String,
    pub name: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Correction {
    pub id: i64,
    pub from: String,
    pub to: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginPrompt {
    pub verification_uri: String,
    pub user_code: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSnapshot {
    pub config: AppConfig,
    pub phase: Phase,
    pub confirmed: String,
    pub partial: String,
    pub final_text: String,
    pub original_text: String,
    pub level: f32,
    pub level_seq: u64,
    /// History changes replace this collection. Frequent level/text snapshots
    /// share the same notes instead of copying every stored transcript.
    pub notes: Arc<Vec<Note>>,
    pub notes_limit: usize,
    pub notes_has_more: bool,
    pub metrics: HistoryMetrics,
    pub usage_period: UsagePeriod,
    pub history_query: String,
    pub active_note_id: Option<i64>,
    pub selected_note: Option<Note>,
    pub audio_playing_note_id: Option<i64>,
    pub elapsed_ms: u64,
    pub status: String,
    pub error: Option<String>,
    pub login: Option<LoginPrompt>,
    pub authenticated: bool,
    pub devices: Vec<AudioDevice>,
    pub corrections: Vec<Correction>,
    pub show_settings: bool,
    pub show_overlay: bool,
    pub demo: bool,
    pub revision: u64,
    /// Lectura en voz alta: estado de la ventana de reproducción.
    #[serde(default)]
    pub tts: TtsSnapshot,
    /// Qué lista muestra Historial. Búsqueda y límite son compartidos.
    #[serde(default)]
    pub history_kind: HistoryKind,
    #[serde(default)]
    pub readings: Arc<Vec<Reading>>,
    #[serde(default)]
    pub readings_has_more: bool,
    #[serde(default)]
    pub active_reading_id: Option<i64>,
    #[serde(default)]
    pub selected_reading: Option<Reading>,
    #[serde(default)]
    pub reading_metrics: ReadingMetrics,
}
impl AppSnapshot {
    pub fn new(config: AppConfig) -> Self {
        Self {
            config,
            phase: Phase::Idle,
            confirmed: String::new(),
            partial: String::new(),
            final_text: String::new(),
            original_text: String::new(),
            level: 0.0,
            level_seq: 0,
            notes: Arc::new(vec![]),
            notes_limit: 100,
            notes_has_more: false,
            metrics: HistoryMetrics::default(),
            usage_period: UsagePeriod::default(),
            history_query: String::new(),
            active_note_id: None,
            selected_note: None,
            audio_playing_note_id: None,
            elapsed_ms: 0,
            status: "Listo para conectar Grok".into(),
            error: None,
            login: None,
            authenticated: false,
            devices: vec![],
            corrections: vec![],
            show_settings: true,
            show_overlay: true,
            demo: false,
            revision: 0,
            tts: TtsSnapshot::default(),
            history_kind: HistoryKind::default(),
            readings: Arc::new(vec![]),
            readings_has_more: false,
            active_reading_id: None,
            selected_reading: None,
            reading_metrics: ReadingMetrics::default(),
        }
    }
}
// Intentionally not Debug: SaveApiKey contains secret material.
pub enum Command {
    Start,
    Stop,
    Toggle,
    /// Monotonic nanoseconds from the input launcher or portal.
    ShortcutPressed {
        at: u64,
        token: Option<u64>,
    },
    ShortcutReleased {
        at: u64,
        token: Option<u64>,
    },
    Cancel,
    Copy,
    Paste,
    ShowSettings,
    HideSettings,
    ShowOverlay,
    HideOverlay,
    Quit,
    SetTheme(Theme),
    SetColorMode(ColorMode),
    SetUsagePeriod(UsagePeriod),
    SearchNotes(String),
    LoadMoreNotes,
    OpenNote(i64),
    CopyNote(i64),
    DeleteNote(i64),
    DeleteNoteAudio(i64),
    PlayNoteAudio(i64),
    StopAudioPlayback,
    ClearHistory,
    UpdateConfig(AppConfig),
    EditText(String),
    RememberCorrection {
        from: String,
        to: String,
    },
    ForgetCorrection(i64),
    StartLogin,
    CancelLogin,
    SaveApiKey(String),
    Disconnect,
    RefreshDevices,
    // --- Lectura en voz alta ---------------------------------------------
    /// Atajo o `ctl read`: lee la selección o el portapapeles según los ajustes.
    /// El IPC nunca transporta el texto; lo lee el controlador.
    ReadSelection,
    ReadClipboard,
    /// Texto pegado en la UI o una nota del historial.
    ReadText(String),
    /// Acepta el coste estimado de un texto largo.
    ConfirmReading,
    /// Reproducir/pausar; con la lectura terminada, vuelve a empezar.
    TtsToggle,
    TtsPlay,
    TtsPause,
    /// Detiene, cancela la generación y cierra la ventana de lectura.
    TtsStop,
    /// Posición en ms dentro de la versión activa.
    TtsSeek(u64),
    /// Byte de `TtsSnapshot::text` (clic en una palabra).
    TtsSeekText(usize),
    /// Segundos hacia adelante (positivo) o atrás (negativo).
    TtsSkip(i32),
    /// Frases hacia adelante o atrás.
    TtsSentence(i32),
    TtsSetRate(f32),
    TtsSetVolume(f32),
    /// Genera con otra voz desde la frase actual y continúa en el mismo punto.
    TtsRegenerate {
        voice: String,
    },
    /// Vuelve a una versión ya generada sin costo.
    TtsUseVersion(u64),
    TtsCopyText,
    /// Escribe la versión activa como WAV en la ruta elegida por el usuario.
    TtsExport(std::path::PathBuf),
    RefreshVoices,
    /// Muestra corta de una voz. Repetirla con la misma voz la detiene.
    PreviewVoice(String),
    SetHistoryKind(HistoryKind),
    OpenReading(i64),
    CopyReading(i64),
    DeleteReading(i64),
    DeleteReadingAudio(i64),
    /// Escucha una lectura del historial: su audio guardado o, si no hay, la regenera.
    ReplayReading(i64),
    ClearReadings,
}
