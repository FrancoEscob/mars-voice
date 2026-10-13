//! Lectura en voz alta: configuración, DTOs compartidos y lógica de texto pura.
//! Este módulo no hace red, audio ni lee la selección; el controlador lo orquesta.
use crate::{AudioRetention, ProviderMode};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Precio público de xAI TTS por millón de caracteres de entrada, verificado el
/// 12-10-2026 en https://docs.x.ai/developers/pricing. Es una estimación, no un cobro.
pub const TTS_USD_PER_MILLION_CHARS: f64 = 15.0;
/// Máximo documentado por pedido REST o por delta del WebSocket.
pub const API_MAX_CHARS: usize = 60_000;
/// Límites documentados del campo `replace`.
pub const MAX_PRONUNCIATIONS: usize = 200;
pub const RATE_MIN: f32 = 0.5;
pub const RATE_MAX: f32 = 2.5;
pub const GENERATION_SPEED_MIN: f32 = 0.7;
pub const GENERATION_SPEED_MAX: f32 = 1.5;
pub const SAMPLE_RATES: [u32; 2] = [24_000, 48_000];
/// Frases más largas se parten para que resaltar y saltar sigan siendo útiles.
pub const MAX_SENTENCE_CHARS: usize = 300;
/// Tamaño objetivo de cada locución enviada al proveedor.
pub const CHUNK_TARGET_CHARS: usize = 600;
/// Ritmo medio aproximado del habla en español a velocidad 1, solo para estimar.
const CHARS_PER_SECOND: f64 = 14.5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TtsSource {
    /// Copia la selección de la ventana activa (Ctrl+C, o Ctrl+Shift+C en
    /// terminales), la lee y restaura el portapapeles. Funciona aunque el
    /// compositor tenga apagada la selección primaria.
    #[default]
    CopySelection,
    /// Selección primaria y, si está vacía, el portapapeles.
    SelectionThenClipboard,
    Selection,
    Clipboard,
}
impl TtsSource {
    pub const ALL: [Self; 4] = [
        Self::CopySelection,
        Self::SelectionThenClipboard,
        Self::Selection,
        Self::Clipboard,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::CopySelection => "Copiar la selección automáticamente",
            Self::SelectionThenClipboard => "Selección primaria; si está vacía, portapapeles",
            Self::Selection => "Solo la selección primaria",
            Self::Clipboard => "Solo el portapapeles",
        }
    }
}
/// Qué hace el atajo si el texto seleccionado es el que ya se está leyendo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SameTextAction {
    #[default]
    TogglePause,
    Restart,
}
impl SameTextAction {
    pub const ALL: [Self; 2] = [Self::TogglePause, Self::Restart];
    pub fn label(self) -> &'static str {
        match self {
            Self::TogglePause => "Pausar o reanudar",
            Self::Restart => "Empezar de nuevo",
        }
    }
}
/// Qué hace el atajo con un texto distinto mientras hay una lectura activa.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum NewTextAction {
    #[default]
    Replace,
    Queue,
}
impl NewTextAction {
    pub const ALL: [Self; 2] = [Self::Replace, Self::Queue];
    pub fn label(self) -> &'static str {
        match self {
            Self::Replace => "Reemplazar la lectura",
            Self::Queue => "Agregar a la cola",
        }
    }
}
/// Estilo aplicado con etiquetas de xAI. El texto seleccionado no puede elegirlo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TtsStyle {
    #[default]
    Normal,
    Soft,
    Whisper,
    Slow,
    Fast,
    SlowSoft,
}
impl TtsStyle {
    pub const ALL: [Self; 6] = [
        Self::Normal,
        Self::Soft,
        Self::Whisper,
        Self::Slow,
        Self::Fast,
        Self::SlowSoft,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Normal => "Normal",
            Self::Soft => "Suave",
            Self::Whisper => "Susurro",
            Self::Slow => "Lento",
            Self::Fast => "Rápido",
            Self::SlowSoft => "Lento y suave",
        }
    }
    /// Texto que se envía al proveedor. La UI muestra el texto sin etiquetas.
    pub fn wrap(self, text: &str) -> String {
        let tags: &[&str] = match self {
            Self::Normal => &[],
            Self::Soft => &["soft"],
            Self::Whisper => &["whisper"],
            Self::Slow => &["slow"],
            Self::Fast => &["fast"],
            Self::SlowSoft => &["slow", "soft"],
        };
        let mut out = String::with_capacity(text.len() + 32);
        for tag in tags {
            out.push_str(&format!("<{tag}>"));
        }
        out.push_str(text);
        for tag in tags.iter().rev() {
            out.push_str(&format!("</{tag}>"));
        }
        out
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Lookahead {
    /// Genera solo unos minutos por delante: detener a la mitad no paga el resto.
    #[default]
    Window,
    All,
}
impl Lookahead {
    pub const ALL: [Self; 2] = [Self::Window, Self::All];
    pub fn label(self) -> &'static str {
        match self {
            Self::Window => "Solo lo próximo (ahorra si detenés)",
            Self::All => "Todo el texto de una vez",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OnDictation {
    #[default]
    Pause,
    Stop,
}
impl OnDictation {
    pub const ALL: [Self; 2] = [Self::Pause, Self::Stop];
    pub fn label(self) -> &'static str {
        match self {
            Self::Pause => "Pausar la lectura",
            Self::Stop => "Detener la lectura",
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pronunciation {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TtsConfig {
    /// Modo de cuenta propio de la lectura. Mismas credenciales que el dictado.
    pub provider: ProviderMode,
    pub read_shortcut: String,
    pub pause_shortcut: String,
    pub stop_shortcut: String,
    pub source: TtsSource,
    /// Con `CopySelection`: devolver al portapapeles lo que tenía antes.
    pub restore_clipboard: bool,
    /// Clases de ventana extra que se tratan como terminal (Ctrl+Shift+C).
    pub terminal_classes: Vec<String>,
    pub same_text: SameTextAction,
    pub new_text: NewTextAction,
    /// Caracteres máximos por lectura; el resto se descarta con aviso.
    pub max_chars: u32,
    /// Pedir confirmación desde esta cantidad de caracteres. 0 nunca pregunta.
    pub confirm_chars: u32,
    pub voice: String,
    /// `auto` o un código BCP-47 (es-ES, es-MX, en…).
    pub language: String,
    pub generation_speed: f32,
    pub style: TtsStyle,
    pub text_normalization: bool,
    pub low_latency: bool,
    pub sample_rate: u32,
    pub playback_rate: f32,
    pub remember_rate: bool,
    pub volume: f32,
    /// Nombre del nodo PipeWire; None usa el predeterminado del sistema.
    pub output_device: Option<String>,
    pub autoplay: bool,
    pub skip_seconds: u8,
    pub lookahead: Lookahead,
    pub lookahead_seconds: u16,
    pub on_dictation: OnDictation,
    pub join_hyphenated: bool,
    pub join_lines: bool,
    pub skip_urls: bool,
    pub skip_code: bool,
    pub read_emoji: bool,
    /// Si es falso, `[pause]`, `<whisper>` y similares del texto se neutralizan.
    pub interpret_tags: bool,
    pub pronunciations: Vec<Pronunciation>,
    pub window_centered: bool,
    pub window_x: f32,
    pub window_y: f32,
    pub compact: bool,
    /// Cerrar la ventana tras terminar. 0 la deja abierta.
    pub auto_close_seconds: u16,
    pub history_enabled: bool,
    pub audio_retention: AudioRetention,
    pub show_cost: bool,
}
impl Default for TtsConfig {
    fn default() -> Self {
        Self {
            provider: ProviderMode::ApiKey,
            read_shortcut: "Ctrl+Alt+Space".into(),
            pause_shortcut: String::new(),
            stop_shortcut: String::new(),
            source: TtsSource::CopySelection,
            restore_clipboard: true,
            terminal_classes: vec![],
            same_text: SameTextAction::TogglePause,
            new_text: NewTextAction::Replace,
            max_chars: 20_000,
            confirm_chars: 5_000,
            voice: "carina".into(),
            language: "auto".into(),
            generation_speed: 1.0,
            style: TtsStyle::Normal,
            text_normalization: false,
            low_latency: true,
            sample_rate: 24_000,
            playback_rate: 1.0,
            remember_rate: true,
            volume: 1.0,
            output_device: None,
            autoplay: true,
            skip_seconds: 10,
            lookahead: Lookahead::Window,
            lookahead_seconds: 120,
            on_dictation: OnDictation::Pause,
            join_hyphenated: true,
            join_lines: true,
            skip_urls: true,
            skip_code: false,
            read_emoji: false,
            interpret_tags: false,
            pronunciations: vec![],
            window_centered: true,
            window_x: 24.0,
            window_y: 24.0,
            compact: false,
            auto_close_seconds: 0,
            history_enabled: true,
            audio_retention: AudioRetention::Off,
            show_cost: true,
        }
    }
}
fn valid_voice_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}
fn valid_language(code: &str) -> bool {
    code == "auto"
        || (!code.is_empty()
            && code.len() <= 16
            && code.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            && code.bytes().next().is_some_and(|b| b.is_ascii_alphabetic()))
}
impl TtsConfig {
    pub fn saves_audio(&self) -> bool {
        self.history_enabled && self.audio_retention.enabled()
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (RATE_MIN..=RATE_MAX).contains(&self.playback_rate),
            "La velocidad de reproducción debe estar entre 0,5× y 2,5×"
        );
        ensure!(
            (GENERATION_SPEED_MIN..=GENERATION_SPEED_MAX).contains(&self.generation_speed),
            "La velocidad de generación debe estar entre 0,7 y 1,5"
        );
        ensure!(
            (0.0..=1.0).contains(&self.volume),
            "El volumen debe estar entre 0 y 100 %"
        );
        ensure!(
            SAMPLE_RATES.contains(&self.sample_rate),
            "La calidad de audio debe ser 24 kHz o 48 kHz"
        );
        ensure!(
            (1..=60).contains(&self.skip_seconds),
            "El salto debe estar entre 1 y 60 segundos"
        );
        ensure!(
            (30..=900).contains(&self.lookahead_seconds),
            "La generación por adelantado debe estar entre 30 s y 15 min"
        );
        ensure!(
            (100..=200_000).contains(&self.max_chars),
            "El límite por lectura debe estar entre 100 y 200 000 caracteres"
        );
        ensure!(
            self.confirm_chars <= self.max_chars,
            "El umbral de confirmación no puede superar el límite por lectura"
        );
        ensure!(
            valid_voice_id(&self.voice),
            "Elegí una voz válida (letras, números y guiones)"
        );
        ensure!(
            valid_language(&self.language),
            "El idioma debe ser auto o un código como es-ES"
        );
        ensure!(
            [
                &self.read_shortcut,
                &self.pause_shortcut,
                &self.stop_shortcut
            ]
            .iter()
            .all(|s| s.chars().count() <= 64 && !s.chars().any(char::is_control)),
            "Los atajos deben tener hasta 64 caracteres"
        );
        ensure!(
            self.terminal_classes.len() <= 50
                && self.terminal_classes.iter().all(|c| {
                    let c = c.trim();
                    !c.is_empty() && c.chars().count() <= 100 && !c.chars().any(char::is_control)
                }),
            "Usá hasta 50 clases de terminal de 100 caracteres"
        );
        ensure!(
            self.output_device
                .as_ref()
                .is_none_or(|d| !d.is_empty() && d.len() <= 512),
            "Dispositivo de salida inválido"
        );
        ensure!(
            self.window_x.is_finite() && self.window_y.is_finite(),
            "Posición de la ventana de lectura inválida"
        );
        ensure!(
            self.auto_close_seconds <= 3600,
            "El cierre automático debe ser de hasta una hora"
        );
        ensure!(
            self.pronunciations.len() <= MAX_PRONUNCIATIONS,
            "Usá hasta 200 reemplazos de pronunciación"
        );
        let mut seen = std::collections::HashSet::new();
        for p in &self.pronunciations {
            let from = p.from.trim();
            let to = p.to.trim();
            ensure!(
                !from.is_empty() && !to.is_empty(),
                "Cada pronunciación necesita texto y reemplazo"
            );
            ensure!(
                from.chars().count() <= 100 && to.chars().count() <= 128,
                "Pronunciación demasiado larga: hasta 100 caracteres de origen y 128 de reemplazo"
            );
            ensure!(
                !from.chars().any(char::is_control) && !to.chars().any(char::is_control),
                "La pronunciación no puede incluir caracteres de control"
            );
            // Reglas de xAI para las claves de `replace`: letras, números,
            // apóstrofos y espacios.
            ensure!(
                from.chars()
                    .all(|c| c.is_alphanumeric() || c == ' ' || c == '\'' || c == '’'),
                "«{from}»: el texto a reemplazar solo admite letras, números, apóstrofos y espacios"
            );
            let normalized = from
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase();
            ensure!(
                seen.insert(normalized),
                "Hay dos pronunciaciones para «{from}»"
            );
        }
        Ok(())
    }
    /// Parámetros que cambian el audio generado. Cambiarlos exige regenerar.
    pub fn voice_settings(&self) -> VoiceSettings {
        VoiceSettings {
            voice: self.voice.clone(),
            language: self.language.clone(),
            speed: self.generation_speed,
            style: self.style,
            text_normalization: self.text_normalization,
            low_latency: self.low_latency,
            sample_rate: self.sample_rate,
            pronunciations: self
                .pronunciations
                .iter()
                .map(|p| Pronunciation {
                    from: p.from.trim().into(),
                    to: p.to.trim().into(),
                })
                .collect(),
        }
    }
}

/// Todo lo que el proveedor necesita para sintetizar, además del texto.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VoiceSettings {
    pub voice: String,
    pub language: String,
    pub speed: f32,
    pub style: TtsStyle,
    pub text_normalization: bool,
    pub low_latency: bool,
    pub sample_rate: u32,
    pub pronunciations: Vec<Pronunciation>,
}
impl VoiceSettings {
    /// Clave estable para la caché de audio de una versión.
    pub fn key(&self) -> u64 {
        fnv1a(serde_json::to_string(self).unwrap_or_default().as_bytes())
    }
}
/// FNV-1a de 64 bits: estable entre versiones de Rust, a diferencia de DefaultHasher.
pub fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for b in bytes {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

// ---------------------------------------------------------------------------
// DTOs compartidos con la UI
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TtsPhase {
    #[default]
    Idle,
    /// Leyendo la selección o el portapapeles.
    Reading,
    /// Texto largo: espera confirmación con el coste estimado.
    Confirm,
    /// Generando, todavía sin audio.
    Connecting,
    /// La reproducción alcanzó a la generación.
    Buffering,
    Playing,
    Paused,
    Finished,
    Error,
}
impl TtsPhase {
    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "En espera",
            Self::Reading => "Leyendo la selección…",
            Self::Confirm => "Confirmá la lectura",
            Self::Connecting => "Generando…",
            Self::Buffering => "Esperando audio…",
            Self::Playing => "Leyendo",
            Self::Paused => "En pausa",
            Self::Finished => "Lectura terminada",
            Self::Error => "No se pudo leer",
        }
    }
    /// Hay una lectura cargada que se puede controlar.
    pub fn has_reading(self) -> bool {
        matches!(
            self,
            Self::Connecting | Self::Buffering | Self::Playing | Self::Paused | Self::Finished
        )
    }
    /// Suena o está por sonar: el dictado debe pausarla.
    pub fn audible(self) -> bool {
        matches!(self, Self::Connecting | Self::Buffering | Self::Playing)
    }
}
/// Rango en bytes dentro de `TtsSnapshot::text` (o del texto de un fragmento).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TextSpan {
    pub start: usize,
    pub end: usize,
}
impl TextSpan {
    pub fn len(self) -> usize {
        self.end - self.start
    }
    pub fn is_empty(self) -> bool {
        self.end <= self.start
    }
    pub fn contains(self, byte: usize) -> bool {
        (self.start..self.end).contains(&byte)
    }
}
/// Palabra con su tramo de audio en la línea de tiempo de la versión activa.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WordTiming {
    pub span: TextSpan,
    pub start_ms: u64,
    pub end_ms: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TtsVoice {
    pub id: String,
    pub name: String,
    pub language: String,
    /// Voz clonada en la consola de xAI.
    pub custom: bool,
}
/// Voces documentadas por xAI; se usan si la lista remota todavía no llegó.
pub fn builtin_voices() -> Vec<TtsVoice> {
    [
        ("carina", "Carina"),
        ("eve", "Eve"),
        ("ara", "Ara"),
        ("rex", "Rex"),
        ("leo", "Leo"),
        ("sal", "Sal"),
    ]
    .into_iter()
    .map(|(id, name)| TtsVoice {
        id: id.into(),
        name: name.into(),
        language: "multilingüe".into(),
        custom: false,
    })
    .collect()
}
/// Una versión de la lectura: los mismos textos con otra voz o ajustes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TtsVersion {
    pub id: u64,
    pub voice: String,
    pub label: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TtsConfirm {
    pub chars: u64,
    pub estimated_usd: f64,
    pub truncated: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TtsSnapshot {
    pub phase: TtsPhase,
    /// Mostrar la ventana de lectura.
    pub show: bool,
    /// Cambia con cada lectura nueva; la UI reinicia scroll y selección.
    pub reading: u64,
    /// Texto que se muestra y se lee (ya limpio), compartido entre snapshots.
    pub text: Arc<String>,
    pub sentences: Arc<Vec<TextSpan>>,
    /// Palabras con tiempo conocido o estimado en la versión activa.
    pub words: Arc<Vec<WordTiming>>,
    /// Tramos en ms que ya tienen audio en la versión activa.
    pub generated: Arc<Vec<(u64, u64)>>,
    pub position_ms: u64,
    pub total_ms: u64,
    /// Falso mientras la duración total sea una estimación.
    pub total_exact: bool,
    pub generating: bool,
    pub current_sentence: Option<usize>,
    /// Índice en `words`.
    pub current_word: Option<usize>,
    pub rate: f32,
    pub volume: f32,
    pub voice: String,
    pub versions: Vec<TtsVersion>,
    pub active_version: u64,
    pub voices: Arc<Vec<TtsVoice>>,
    pub level: f32,
    pub level_seq: u64,
    pub chars: u64,
    /// Caracteres ya enviados al proveedor en esta lectura, incluidas regeneraciones.
    pub chars_sent: u64,
    pub estimated_usd: f64,
    pub confirm: Option<TtsConfirm>,
    pub queued: usize,
    pub status: String,
    pub error: Option<String>,
    pub notice: Option<String>,
    pub previewing_voice: Option<String>,
    /// Hay credencial guardada para el modo de lectura. No confirma cobertura.
    pub account_ready: bool,
    /// Salidas PipeWire para elegir en Ajustes → Lectura.
    pub outputs: Vec<crate::AudioDevice>,
}
impl Default for TtsSnapshot {
    fn default() -> Self {
        Self {
            phase: TtsPhase::Idle,
            show: false,
            reading: 0,
            text: Arc::new(String::new()),
            sentences: Arc::new(vec![]),
            words: Arc::new(vec![]),
            generated: Arc::new(vec![]),
            position_ms: 0,
            total_ms: 0,
            total_exact: false,
            generating: false,
            current_sentence: None,
            current_word: None,
            rate: 1.0,
            volume: 1.0,
            voice: String::new(),
            versions: vec![],
            active_version: 0,
            voices: Arc::new(builtin_voices()),
            level: 0.0,
            level_seq: 0,
            chars: 0,
            chars_sent: 0,
            estimated_usd: 0.0,
            confirm: None,
            queued: 0,
            status: String::new(),
            error: None,
            notice: None,
            previewing_voice: None,
            account_ready: false,
            outputs: vec![],
        }
    }
}

// ---------------------------------------------------------------------------
// Estimaciones
// ---------------------------------------------------------------------------

pub fn estimate_usd(chars: u64) -> f64 {
    chars as f64 * TTS_USD_PER_MILLION_CHARS / 1_000_000.0
}
/// Duración aproximada antes de tener audio. `speed` es la de generación.
pub fn estimate_ms(chars: usize, speed: f32) -> u64 {
    let speed = f64::from(speed.clamp(GENERATION_SPEED_MIN, GENERATION_SPEED_MAX));
    (chars as f64 * 1000.0 / (CHARS_PER_SECOND * speed)).round() as u64
}

// ---------------------------------------------------------------------------
// Preparación del texto
// ---------------------------------------------------------------------------

/// Opciones de limpieza; se toman de `TtsConfig`.
#[derive(Debug, Clone, Copy)]
pub struct CleanOptions {
    pub join_hyphenated: bool,
    pub join_lines: bool,
    pub skip_urls: bool,
    pub skip_code: bool,
    pub read_emoji: bool,
    pub interpret_tags: bool,
    pub max_chars: usize,
}
impl From<&TtsConfig> for CleanOptions {
    fn from(c: &TtsConfig) -> Self {
        Self {
            join_hyphenated: c.join_hyphenated,
            join_lines: c.join_lines,
            skip_urls: c.skip_urls,
            skip_code: c.skip_code,
            read_emoji: c.read_emoji,
            interpret_tags: c.interpret_tags,
            max_chars: c.max_chars as usize,
        }
    }
}
/// Grupo de frases consecutivas que se envía como una locución.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chunk {
    pub first_sentence: usize,
    /// Exclusivo.
    pub end_sentence: usize,
    pub span: TextSpan,
}
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedText {
    pub text: String,
    pub sentences: Vec<TextSpan>,
    pub chunks: Vec<Chunk>,
    /// Se descartó texto por superar `max_chars`.
    pub truncated: bool,
    /// Caracteres de la selección limpia antes de truncar.
    pub original_chars: usize,
}
impl PreparedText {
    pub fn chars(&self) -> usize {
        self.text.chars().count()
    }
    pub fn chunk_text(&self, chunk: usize) -> &str {
        let span = self.chunks[chunk].span;
        &self.text[span.start..span.end]
    }
    /// Fragmento que contiene la frase indicada.
    pub fn chunk_of_sentence(&self, sentence: usize) -> Option<usize> {
        self.chunks
            .iter()
            .position(|c| (c.first_sentence..c.end_sentence).contains(&sentence))
    }
    pub fn sentence_at_byte(&self, byte: usize) -> Option<usize> {
        sentence_at(&self.sentences, byte)
    }
}
/// Índice de la frase que contiene `byte` o, si cae entre frases, la siguiente.
pub fn sentence_at(sentences: &[TextSpan], byte: usize) -> Option<usize> {
    sentences
        .iter()
        .position(|s| byte < s.end)
        .or_else(|| sentences.len().checked_sub(1))
}

/// Limpia la selección, la divide en frases y planifica las locuciones.
/// Devuelve None si no queda nada legible.
pub fn prepare(raw: &str, options: CleanOptions) -> Option<PreparedText> {
    let cleaned = clean(raw, options);
    let original_chars = cleaned.chars().count();
    let sentences = split_sentences(&cleaned);
    if sentences.is_empty() {
        return None;
    }
    let max_chars = options.max_chars.max(1);
    let mut kept = 0;
    let mut truncated = false;
    for (index, sentence) in sentences.iter().enumerate() {
        if cleaned[..sentence.end].chars().count() > max_chars {
            truncated = true;
            break;
        }
        kept = index + 1;
    }
    // Una primera frase mayor que el límite se corta en un borde de palabra.
    let (text, sentences) = if kept == 0 {
        let mut end = cleaned
            .char_indices()
            .nth(max_chars)
            .map_or(cleaned.len(), |(i, _)| i);
        if let Some(space) = cleaned[..end].rfind(char::is_whitespace) {
            if space > 0 {
                end = space;
            }
        }
        let text = cleaned[..end].trim_end().to_string();
        let sentences = split_sentences(&text);
        (text, sentences)
    } else {
        let end = sentences[kept - 1].end;
        (cleaned[..end].to_string(), sentences[..kept].to_vec())
    };
    if sentences.is_empty() {
        return None;
    }
    let chunks = plan_chunks(&text, &sentences);
    Some(PreparedText {
        text,
        sentences,
        chunks,
        truncated,
        original_chars,
    })
}

/// Limpieza determinista. El resultado es lo que se muestra y lo que se lee.
pub fn clean(raw: &str, o: CleanOptions) -> String {
    let mut text = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() != Some(&'\n') {
                    text.push('\n');
                }
            }
            '\n' => text.push('\n'),
            '\t' | '\u{a0}' | '\u{202f}' | '\u{2007}' => text.push(' '),
            '\u{ad}' | '\u{200b}' | '\u{200c}' | '\u{2060}' | '\u{feff}' => {}
            '\u{200d}' if o.read_emoji => text.push(c),
            '\u{200d}' => {}
            c if c.is_control() => {}
            '\u{2028}' | '\u{2029}' => text.push('\n'),
            c => text.push(c),
        }
    }
    if o.skip_code {
        text = remove_code_blocks(&text);
    }
    if o.join_hyphenated {
        text = join_hyphenated(&text);
    }
    if o.skip_urls {
        text = replace_urls(&text);
    }
    if !o.read_emoji {
        text = text.chars().filter(|c| !is_emoji(*c)).collect();
    }
    if !o.interpret_tags {
        text = neutralize_tags(&text);
    }
    normalize_layout(&text, o.join_lines)
}
fn remove_code_blocks(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_code = false;
    for line in text.split('\n') {
        if line.trim_start().starts_with("```") {
            if !in_code {
                out.push_str("\n\nBloque de código omitido.\n\n");
            }
            in_code = !in_code;
            continue;
        }
        if !in_code {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}
/// "pala-\nbra" → "palabra" cuando ambos lados son letras y la continuación es minúscula.
fn join_hyphenated(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '-' && i > 0 && chars[i - 1].is_alphabetic() {
            let mut j = i + 1;
            while j < chars.len() && chars[j] == ' ' {
                j += 1;
            }
            if j < chars.len() && chars[j] == '\n' {
                let mut k = j + 1;
                while k < chars.len() && chars[k] == ' ' {
                    k += 1;
                }
                if k < chars.len() && chars[k].is_lowercase() {
                    i = k;
                    continue;
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}
fn replace_urls(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while !rest.is_empty() {
        let token_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let token = &rest[..token_end];
        let lead = token
            .find(|c: char| !matches!(c, '(' | '[' | '<' | '«' | '"' | '“' | '\''))
            .unwrap_or(token.len());
        let body = &token[lead..];
        let lower = body.to_ascii_lowercase();
        if lower.starts_with("http://")
            || lower.starts_with("https://")
            || lower.starts_with("www.")
        {
            let trail = body
                .trim_end_matches(|c: char| ".,;:!?)]>»\"”'".contains(c))
                .len();
            out.push_str(&token[..lead]);
            out.push_str("enlace");
            out.push_str(&body[trail..]);
        } else {
            out.push_str(token);
        }
        rest = &rest[token_end..];
        let ws_end = rest
            .find(|c: char| !c.is_whitespace())
            .unwrap_or(rest.len());
        out.push_str(&rest[..ws_end]);
        rest = &rest[ws_end..];
    }
    out
}
fn is_emoji(c: char) -> bool {
    matches!(c as u32,
        0x1F000..=0x1FAFF | 0x2600..=0x27BF | 0x2B00..=0x2BFF | 0xFE00..=0xFE0F | 0xE0020..=0xE007F)
}
/// Neutraliza etiquetas de voz que traiga el texto: `[laugh]` → `(laugh)`,
/// `<whisper>` y `</soft>` desaparecen. Así una página no controla la voz.
fn neutralize_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        let c = text[i..].chars().next().expect("valid boundary");
        if c == '[' {
            if let Some(len) = text[i + 1..].find(']') {
                let inner = &text[i + 1..i + 1 + len];
                if !inner.is_empty()
                    && inner.len() <= 24
                    && inner
                        .bytes()
                        .all(|b| b.is_ascii_alphabetic() || b == b'-' || b == b'_')
                {
                    out.push('(');
                    out.push_str(inner);
                    out.push(')');
                    i += len + 2;
                    continue;
                }
            }
        }
        if c == '<' {
            if let Some(len) = text[i + 1..].find('>') {
                let inner = &text[i + 1..i + 1 + len];
                let name = inner.strip_prefix('/').unwrap_or(inner);
                let name = name.strip_suffix('/').unwrap_or(name);
                let tag_name = name.split(' ').next().unwrap_or("");
                if len <= 64
                    && !tag_name.is_empty()
                    && tag_name.as_bytes()[0].is_ascii_alphabetic()
                    && tag_name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                    && !inner.contains('<')
                {
                    i += len + 2;
                    continue;
                }
            }
        }
        out.push(c);
        i += c.len_utf8();
    }
    out
}
/// Colapsa espacios, recorta líneas, separa párrafos con una línea en blanco y,
/// si `join_lines`, une los saltos simples dentro de un párrafo.
fn normalize_layout(text: &str, join_lines: bool) -> String {
    let mut paragraphs: Vec<String> = vec![];
    let mut current: Vec<String> = vec![];
    for line in text.split('\n') {
        let collapsed = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if collapsed.is_empty() {
            if !current.is_empty() {
                paragraphs.push(current.join(if join_lines { " " } else { "\n" }));
                current.clear();
            }
        } else {
            current.push(collapsed);
        }
    }
    if !current.is_empty() {
        paragraphs.push(current.join(if join_lines { " " } else { "\n" }));
    }
    paragraphs.join("\n\n")
}

/// Abreviaturas comunes en español e inglés que no cierran una frase.
const ABBREVIATIONS: &[&str] = &[
    "sr", "sra", "srta", "dr", "dra", "lic", "ing", "arq", "prof", "etc", "ej", "pág", "pag",
    "núm", "num", "nro", "nº", "art", "aprox", "vs", "ee", "uu", "ud", "uds", "vd", "vds", "av",
    "avda", "cap", "fig", "vol", "ed", "sto", "sta", "dto", "depto", "tel", "mr", "mrs", "ms",
    "st", "no", "inc", "ltd", "co", "jr", "e.g", "i.e", "p.ej", "cía", "gral", "cnel", "mons",
    "pte", "min", "máx", "max", "mín", "km", "kg", "cm", "mm",
];
fn is_terminator(c: char) -> bool {
    matches!(c, '.' | '!' | '?' | '…' | '。' | '！' | '？')
}
fn is_closer(c: char) -> bool {
    matches!(c, ')' | ']' | '»' | '”' | '"' | '\'' | '’' | '›')
}
fn opens_sentence(c: char) -> bool {
    c.is_uppercase()
        || c.is_numeric()
        || matches!(
            c,
            '¿' | '¡' | '"' | '«' | '“' | '(' | '\'' | '‘' | '—' | '-' | '[' | '‹'
        )
}
/// Palabra (letras y puntos internos) que termina justo antes de `end`.
fn word_before(text: &str, end: usize) -> &str {
    let start = text[..end]
        .char_indices()
        .rev()
        .find(|(_, c)| !(c.is_alphabetic() || *c == '.' || *c == 'º'))
        .map_or(0, |(i, c)| i + c.len_utf8());
    &text[start..end]
}
/// Divide en frases. Cada salto de línea también es un límite.
pub fn split_sentences(text: &str) -> Vec<TextSpan> {
    let mut out = vec![];
    let mut line_start = 0;
    for line in text.split_inclusive('\n') {
        let line_end = line_start + line.len();
        split_line(text, line_start, line_end, &mut out);
        line_start = line_end;
    }
    out
}
fn split_line(text: &str, start: usize, end: usize, out: &mut Vec<TextSpan>) {
    let line = &text[start..end];
    let mut sentence_start = start;
    let mut iter = line.char_indices().peekable();
    while let Some((offset, c)) = iter.next() {
        if !is_terminator(c) {
            continue;
        }
        let mut cut = start + offset + c.len_utf8();
        while let Some(&(o, n)) = iter.peek() {
            if is_terminator(n) || is_closer(n) {
                cut = start + o + n.len_utf8();
                iter.next();
            } else {
                break;
            }
        }
        let after = &text[cut..end];
        let next_visible = after.trim_start().chars().next();
        let followed_by_space = after.chars().next().is_none_or(char::is_whitespace);
        if !followed_by_space {
            continue; // "3.5", "x.com", "p.ej."
        }
        let boundary = match next_visible {
            None => true,
            Some(n) => opens_sentence(n),
        };
        if !boundary {
            continue;
        }
        if c == '.' && next_visible.is_some() {
            let word = word_before(text, start + offset);
            let lower = word.trim_end_matches('.').to_lowercase();
            let single_initial =
                word.chars().count() == 1 && word.chars().next().is_some_and(char::is_uppercase);
            if single_initial || ABBREVIATIONS.contains(&lower.as_str()) {
                continue;
            }
        }
        push_sentence(text, sentence_start, cut, out);
        sentence_start = cut;
    }
    push_sentence(text, sentence_start, end, out);
}
/// Recorta espacios, ignora tramos sin letras ni números y parte frases largas.
fn push_sentence(text: &str, start: usize, end: usize, out: &mut Vec<TextSpan>) {
    let slice = &text[start..end];
    let lead = slice.len() - slice.trim_start().len();
    let trail = slice.len() - slice.trim_end().len();
    let (start, end) = (start + lead, end - trail);
    if start >= end || !text[start..end].chars().any(char::is_alphanumeric) {
        return;
    }
    let mut s = start;
    while text[s..end].chars().count() > MAX_SENTENCE_CHARS {
        let limit = s + text[s..]
            .char_indices()
            .nth(MAX_SENTENCE_CHARS)
            .map_or(end - s, |(i, _)| i);
        let window = &text[s..limit];
        let cut = [", ", "; ", ": ", " — ", " – "]
            .iter()
            .filter_map(|p| window.rfind(p).map(|i| i + p.len() - 1))
            .filter(|i| *i > MAX_SENTENCE_CHARS / 3)
            .max()
            .or_else(|| window.rfind(' ').filter(|i| *i > 0))
            .map_or(limit, |i| s + i);
        let piece = text[s..cut].trim_end();
        if !piece.is_empty() {
            out.push(TextSpan {
                start: s,
                end: s + piece.len(),
            });
        }
        s = cut;
        s += text[s..end].len() - text[s..end].trim_start().len();
    }
    if s < end {
        out.push(TextSpan { start: s, end });
    }
}
/// Agrupa frases en locuciones. La primera es corta para que el audio empiece
/// rápido; las demás rondan `CHUNK_TARGET_CHARS` sin cortar frases.
pub fn plan_chunks(text: &str, sentences: &[TextSpan]) -> Vec<Chunk> {
    let mut chunks = vec![];
    let mut i = 0;
    while i < sentences.len() {
        let first = i;
        let target = if chunks.is_empty() {
            // Un título corto viaja con la frase siguiente.
            if text[sentences[i].start..sentences[i].end].chars().count() < 40 {
                200
            } else {
                0
            }
        } else {
            CHUNK_TARGET_CHARS
        };
        i += 1;
        while i < sentences.len() {
            let len = text[sentences[first].start..sentences[i].end]
                .chars()
                .count();
            if len > target {
                break;
            }
            i += 1;
        }
        chunks.push(Chunk {
            first_sentence: first,
            end_sentence: i,
            span: TextSpan {
                start: sentences[first].start,
                end: sentences[i - 1].end,
            },
        });
    }
    chunks
}

// ---------------------------------------------------------------------------
// Alineación de marcas de tiempo
// ---------------------------------------------------------------------------

/// Carácter con su tramo, relativo al inicio de la locución.
#[derive(Debug, Clone, PartialEq)]
pub struct CharTime {
    pub ch: String,
    pub start_ms: u64,
    pub end_ms: u64,
}
fn fold(c: char) -> char {
    let lower = c.to_lowercase().next().unwrap_or(c);
    match lower {
        'á' | 'à' | 'â' | 'ä' | 'ã' => 'a',
        'é' | 'è' | 'ê' | 'ë' => 'e',
        'í' | 'ì' | 'î' | 'ï' => 'i',
        'ó' | 'ò' | 'ô' | 'ö' | 'õ' => 'o',
        'ú' | 'ù' | 'û' | 'ü' => 'u',
        'ñ' => 'n',
        'ç' => 'c',
        other => other,
    }
}
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric()
}
/// Palabras del texto como rangos en bytes.
pub fn word_spans(text: &str) -> Vec<TextSpan> {
    let mut spans = vec![];
    let mut start = None;
    let mut last_end = 0;
    for (i, c) in text.char_indices() {
        let inner_joiner = matches!(c, '\'' | '’' | '-')
            && start.is_some()
            && text[i + c.len_utf8()..]
                .chars()
                .next()
                .is_some_and(is_word_char);
        if is_word_char(c) || inner_joiner {
            if start.is_none() {
                start = Some(i);
            }
            last_end = i + c.len_utf8();
        } else if let Some(s) = start.take() {
            spans.push(TextSpan {
                start: s,
                end: last_end,
            });
        }
    }
    if let Some(s) = start {
        spans.push(TextSpan {
            start: s,
            end: last_end,
        });
    }
    spans
}
/// Asigna tiempos a cada palabra de `text` a partir de las marcas por carácter
/// del proveedor. Tolera diferencias (normalización, reemplazos, etiquetas).
/// Las palabras sin coincidencia se interpolan. Sin coincidencias, reparte
/// `duration_ms` en proporción a la longitud.
pub fn align_words(text: &str, marks: &[CharTime], duration_ms: u64) -> Vec<WordTiming> {
    let words = word_spans(text);
    if words.is_empty() {
        return vec![];
    }
    // Secuencia de caracteres del texto, con la palabra a la que pertenecen.
    let mut text_chars: Vec<(char, usize)> = vec![];
    for (w, span) in words.iter().enumerate() {
        for c in text[span.start..span.end].chars() {
            if is_word_char(c) {
                text_chars.push((fold(c), w));
            }
        }
    }
    let graph: Vec<(char, u64, u64)> = marks
        .iter()
        .filter_map(|m| {
            let c = m.ch.chars().next()?;
            is_word_char(c).then(|| (fold(c), m.start_ms, m.end_ms.max(m.start_ms)))
        })
        .collect();
    let mut times: Vec<Option<(u64, u64)>> = vec![None; words.len()];
    let (mut i, mut j) = (0, 0);
    const WINDOW: usize = 12;
    while i < text_chars.len() && j < graph.len() {
        let (tc, word) = text_chars[i];
        let (gc, start, end) = graph[j];
        if tc == gc {
            let t = times[word].get_or_insert((start, end));
            t.0 = t.0.min(start);
            t.1 = t.1.max(end);
            i += 1;
            j += 1;
            continue;
        }
        let skip_graph = (j + 1..graph.len().min(j + 1 + WINDOW)).find(|&k| graph[k].0 == tc);
        let skip_text =
            (i + 1..text_chars.len().min(i + 1 + WINDOW)).find(|&k| text_chars[k].0 == gc);
        match (skip_graph, skip_text) {
            (Some(k), Some(l)) if k - j <= l - i => j = k,
            (Some(k), None) => j = k,
            (_, Some(l)) => i = l,
            (None, None) => {
                i += 1;
                j += 1;
            }
        }
    }
    if times.iter().all(Option::is_none) {
        return proportional_words(text, duration_ms);
    }
    let end_total = duration_ms.max(
        times
            .iter()
            .flatten()
            .map(|t| t.1)
            .max()
            .unwrap_or_default(),
    );
    fill_gaps(text, &words, times, end_total)
}
/// Reparte el tiempo por longitud de palabra, con pausas en puntuación.
pub fn proportional_words(text: &str, duration_ms: u64) -> Vec<WordTiming> {
    let words = word_spans(text);
    fill_gaps(text, &words, vec![None; words.len()], duration_ms)
}
fn weight(text: &str, words: &[TextSpan], w: usize) -> f64 {
    let span = words[w];
    let mut weight = text[span.start..span.end].chars().count() as f64 + 1.0;
    let tail_end = words.get(w + 1).map_or(text.len(), |n| n.start);
    let tail = &text[span.end..tail_end];
    if tail.contains(['.', '!', '?', '…']) {
        weight += 4.0;
    } else if tail.contains([',', ';', ':']) {
        weight += 2.0;
    }
    weight
}
fn fill_gaps(
    text: &str,
    words: &[TextSpan],
    mut times: Vec<Option<(u64, u64)>>,
    total_ms: u64,
) -> Vec<WordTiming> {
    let n = words.len();
    let mut w = 0;
    while w < n {
        if times[w].is_some() {
            w += 1;
            continue;
        }
        let gap_start = w;
        while w < n && times[w].is_none() {
            w += 1;
        }
        let from = if gap_start == 0 {
            0
        } else {
            times[gap_start - 1].map_or(0, |t| t.1)
        };
        let to = if w < n {
            times[w].map_or(total_ms, |t| t.0)
        } else {
            total_ms
        }
        .max(from);
        let weights: Vec<f64> = (gap_start..w).map(|k| weight(text, words, k)).collect();
        let sum: f64 = weights.iter().sum::<f64>().max(1.0);
        let mut cursor = from as f64;
        for (offset, wt) in weights.iter().enumerate() {
            let len = (to - from) as f64 * wt / sum;
            let start = cursor.round() as u64;
            cursor += len;
            times[gap_start + offset] = Some((start, (cursor.round() as u64).max(start)));
        }
    }
    // Monotonía: un desorden del proveedor no debe hacer retroceder el resaltado.
    let mut floor = 0;
    words
        .iter()
        .zip(times)
        .map(|(span, t)| {
            let (s, e) = t.unwrap_or((floor, floor));
            let start = s.max(floor);
            let end = e.max(start);
            floor = start;
            WordTiming {
                span: *span,
                start_ms: start,
                end_ms: end,
            }
        })
        .collect()
}
/// Índice de la palabra que suena en `ms`: la última que empezó.
pub fn word_at(words: &[WordTiming], ms: u64) -> Option<usize> {
    let idx = words.partition_point(|w| w.start_ms <= ms);
    idx.checked_sub(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> CleanOptions {
        CleanOptions::from(&TtsConfig::default())
    }
    fn texts(text: &str) -> Vec<&str> {
        split_sentences(text)
            .iter()
            .map(|s| &text[s.start..s.end])
            .collect()
    }

    #[test]
    fn defaults_are_valid_api_key_and_never_save_audio() {
        let c = TtsConfig::default();
        c.validate().unwrap();
        assert_eq!(c.provider, ProviderMode::ApiKey);
        assert!(c.history_enabled && !c.saves_audio());
        assert_eq!(c.read_shortcut, "Ctrl+Alt+Space");
        assert_eq!(c.source, TtsSource::CopySelection);
        assert!(c.restore_clipboard);
        assert!(!c.interpret_tags);
    }
    #[test]
    fn config_rejects_out_of_range_and_duplicate_pronunciations() {
        let base = TtsConfig::default();
        for bad in [
            TtsConfig {
                playback_rate: 3.0,
                ..base.clone()
            },
            TtsConfig {
                generation_speed: 0.5,
                ..base.clone()
            },
            TtsConfig {
                sample_rate: 44_100,
                ..base.clone()
            },
            TtsConfig {
                voice: "../x".into(),
                ..base.clone()
            },
            TtsConfig {
                language: "es ES".into(),
                ..base.clone()
            },
            TtsConfig {
                confirm_chars: 30_000,
                ..base.clone()
            },
            TtsConfig {
                pronunciations: vec![
                    Pronunciation {
                        from: "Alumni".into(),
                        to: "Álumni".into(),
                    },
                    Pronunciation {
                        from: "alumni".into(),
                        to: "x".into(),
                    },
                ],
                ..base.clone()
            },
            TtsConfig {
                pronunciations: vec![Pronunciation {
                    from: "C++".into(),
                    to: "ce más más".into(),
                }],
                ..base.clone()
            },
            TtsConfig {
                pronunciations: vec![
                    Pronunciation {
                        from: "Mars  voice".into(),
                        to: "a".into(),
                    },
                    Pronunciation {
                        from: "mars voice".into(),
                        to: "b".into(),
                    },
                ],
                ..base.clone()
            },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
    }
    #[test]
    fn old_config_without_tts_section_loads_defaults_and_roundtrips() {
        let c: crate::AppConfig = toml::from_str("theme = 'orbit'\n").unwrap();
        assert_eq!(c.tts, TtsConfig::default());
        let mut custom = c.clone();
        custom.tts.voice = "eve".into();
        custom.tts.pronunciations.push(Pronunciation {
            from: "GPUI".into(),
            to: "ge pe u i".into(),
        });
        let back: crate::AppConfig = toml::from_str(&toml::to_string(&custom).unwrap()).unwrap();
        assert_eq!(back, custom);
        assert!(toml::from_str::<crate::AppConfig>("[tts]\nunknown = 1\n").is_err());
    }
    #[test]
    fn settings_key_changes_with_voice_but_not_with_playback_rate() {
        let a = TtsConfig::default();
        let mut b = a.clone();
        b.playback_rate = 1.5;
        b.volume = 0.3;
        assert_eq!(a.voice_settings().key(), b.voice_settings().key());
        b.voice = "eve".into();
        assert_ne!(a.voice_settings().key(), b.voice_settings().key());
    }
    #[test]
    fn style_wraps_without_touching_display_text() {
        assert_eq!(TtsStyle::Normal.wrap("Hola."), "Hola.");
        assert_eq!(
            TtsStyle::SlowSoft.wrap("Hola."),
            "<slow><soft>Hola.</soft></slow>"
        );
    }

    #[test]
    fn spanish_abbreviations_decimals_and_initials_do_not_split() {
        let t = "El Sr. Pérez pagó 3.5 dólares en EE. UU. el lunes. J. R. R. Tolkien escribió mucho. ¿Vino? ¡Sí! Fin…";
        assert_eq!(
            texts(t),
            vec![
                "El Sr. Pérez pagó 3.5 dólares en EE. UU. el lunes.",
                "J. R. R. Tolkien escribió mucho.",
                "¿Vino?",
                "¡Sí!",
                "Fin…"
            ]
        );
    }
    #[test]
    fn lowercase_after_period_and_urls_stay_in_sentence() {
        assert_eq!(
            texts("Usá x.com o etc. y seguí. Listo."),
            vec!["Usá x.com o etc. y seguí.", "Listo."]
        );
        assert_eq!(
            texts("Dijo «basta.» Después se fue."),
            vec!["Dijo «basta.»", "Después se fue."]
        );
    }
    #[test]
    fn newlines_are_boundaries_and_punctuation_only_lines_are_skipped() {
        assert_eq!(
            texts("Título\nPrimera línea\n—\n\nOtra"),
            vec!["Título", "Primera línea", "Otra"]
        );
    }
    #[test]
    fn long_sentences_split_at_commas_or_spaces_within_limit() {
        let long = format!(
            "{}, {}.",
            "palabra ".repeat(30).trim(),
            "otra ".repeat(60).trim()
        );
        let spans = split_sentences(&long);
        assert!(spans.len() >= 2);
        for s in &spans {
            assert!(long[s.start..s.end].chars().count() <= MAX_SENTENCE_CHARS);
            assert!(!long[s.start..s.end].starts_with(' '));
        }
        let unbroken = "a".repeat(700);
        let spans = split_sentences(&unbroken);
        assert_eq!(spans.iter().map(|s| s.len()).sum::<usize>(), 700);
    }

    #[test]
    fn cleanup_joins_hyphenation_lines_and_collapses_space() {
        let raw = "Esta es una pala-\nbra cortada\r\ny una línea\u{a0}partida.\n\n\n  Nuevo   párrafo.\u{200b}";
        assert_eq!(
            clean(raw, opts()),
            "Esta es una palabra cortada y una línea partida.\n\nNuevo párrafo."
        );
        let keep = CleanOptions {
            join_lines: false,
            join_hyphenated: false,
            ..opts()
        };
        assert_eq!(clean("Uno-\nDos\ntres", keep), "Uno-\nDos\ntres");
    }
    #[test]
    fn urls_emoji_code_and_tags_follow_options() {
        let raw = "Mirá (https://x.ai/docs). Genial 🚀 [laugh] <whisper>secreto</whisper>\n```\nlet x = 1;\n```\nFin";
        let mut o = opts();
        o.skip_code = true;
        assert_eq!(
            clean(raw, o),
            "Mirá (enlace). Genial (laugh) secreto\n\nBloque de código omitido.\n\nFin"
        );
        let mut literal = opts();
        literal.skip_urls = false;
        literal.read_emoji = true;
        literal.interpret_tags = true;
        assert_eq!(
            clean("Ver https://x.ai 🚀 [pause] <soft>a</soft>", literal),
            "Ver https://x.ai 🚀 [pause] <soft>a</soft>"
        );
        // Comparaciones y flechas no son etiquetas.
        assert_eq!(clean("Si a < b y b > c", opts()), "Si a < b y b > c");
    }
    #[test]
    fn prepare_truncates_at_sentence_boundary_and_reports_it() {
        let raw = "Primera frase completa. Segunda frase que no entra.";
        let p = prepare(
            raw,
            CleanOptions {
                max_chars: 30,
                ..opts()
            },
        )
        .unwrap();
        assert_eq!(p.text, "Primera frase completa.");
        assert!(p.truncated);
        assert_eq!(p.original_chars, raw.chars().count());
        let word_cut = prepare(
            "unapalabra otrapalabra tercera",
            CleanOptions {
                max_chars: 15,
                ..opts()
            },
        )
        .unwrap();
        assert_eq!(word_cut.text, "unapalabra");
        assert!(prepare(" \n🚀 — ", opts()).is_none());
    }
    #[test]
    fn chunks_cover_all_sentences_in_order_with_a_short_first_one() {
        let sentence = "Esta es una frase de prueba con algo de longitud para el plan. ";
        let raw = sentence.repeat(40);
        let p = prepare(&raw, opts()).unwrap();
        assert_eq!(p.chunks[0].end_sentence, 1, "la primera locución es corta");
        let mut next = 0;
        for c in &p.chunks {
            assert_eq!(c.first_sentence, next);
            assert!(c.end_sentence > c.first_sentence);
            assert!(p.chunk_text(0).len() < 120);
            next = c.end_sentence;
        }
        assert_eq!(next, p.sentences.len());
        assert!(p.chunks.len() < p.sentences.len());
        let titled = prepare("Capítulo 1\n\nEl comienzo de todo fue simple.", opts()).unwrap();
        assert_eq!(titled.chunks[0].end_sentence, 2);
        assert_eq!(titled.chunk_of_sentence(1), Some(0));
    }

    fn marks(text: &str, per_char_ms: u64) -> Vec<CharTime> {
        text.chars()
            .enumerate()
            .map(|(i, c)| CharTime {
                ch: c.to_string(),
                start_ms: i as u64 * per_char_ms,
                end_ms: (i as u64 + 1) * per_char_ms,
            })
            .collect()
    }
    #[test]
    fn alignment_follows_exact_marks() {
        let text = "Hola, mundo bello.";
        let words = align_words(text, &marks(text, 10), 180);
        let got: Vec<_> = words
            .iter()
            .map(|w| (&text[w.span.start..w.span.end], w.start_ms, w.end_ms))
            .collect();
        assert_eq!(
            got,
            vec![("Hola", 0, 40), ("mundo", 60, 110), ("bello", 120, 170)]
        );
        assert_eq!(word_at(&words, 65), Some(1));
        assert_eq!(word_at(&words, 0), Some(0));
    }
    #[test]
    fn alignment_tolerates_accents_replacements_and_missing_words() {
        let text = "Alumni estudió en Hyprland ayer.";
        // El proveedor pronunció un reemplazo y omitió acentos.
        let spoken = "Alumni estudio en Haipr lend ayer";
        let words = align_words(text, &marks(spoken, 10), 400);
        assert_eq!(words.len(), 5);
        assert_eq!(words[0].start_ms, 0);
        assert_eq!(words[1].start_ms, 70, "acento plegado");
        let hypr = words[3];
        assert!(hypr.start_ms >= words[2].end_ms && hypr.end_ms <= words[4].start_ms);
        assert!(words.windows(2).all(|w| w[0].start_ms <= w[1].start_ms));
    }
    #[test]
    fn alignment_without_marks_falls_back_to_proportional() {
        let text = "Uno dos. Tres";
        let words = align_words(text, &[], 1300);
        assert_eq!(words.len(), 3);
        assert_eq!(words[0].start_ms, 0);
        assert!(words[2].end_ms <= 1300 && words[2].end_ms >= 1290);
        // La pausa del punto da más tiempo a "dos".
        assert!(words[1].end_ms - words[1].start_ms > words[0].end_ms - words[0].start_ms);
    }
    #[test]
    fn estimates_are_monotonic_and_labelled_by_documented_price() {
        assert_eq!(estimate_usd(1_000_000), 15.0);
        assert!(estimate_ms(1450, 1.0) == 100_000);
        assert!(estimate_ms(1450, 1.5) < estimate_ms(1450, 1.0));
    }
    #[test]
    fn word_spans_keep_contractions_and_hyphenated_words() {
        let t = "don't stop — e-mail, ¿qué?";
        let words: Vec<_> = word_spans(t).iter().map(|s| &t[s.start..s.end]).collect();
        assert_eq!(words, vec!["don't", "stop", "e-mail", "qué"]);
    }
}
