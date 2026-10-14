//! Escritura: arreglar y traducir texto seleccionado con Grok.
//! Este módulo define configuración, DTOs compartidos y lógica de texto pura.
//! No hace red ni lee la selección; el controlador lo orquesta.
use crate::ProviderMode;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

/// `app_id` (Wayland) de la ventana de Escritura, para reglas del compositor.
pub const WRITER_APP_ID: &str = "dicta-writer";
/// Caracteres máximos por pedido. El resto se descarta con aviso.
pub const MAX_INPUT_CHARS: usize = 8_000;
/// Idiomas destino que ofrece la UI (ISO 639-1). `target_language` acepta
/// cualquier código de dos letras; esta lista solo ordena el selector.
pub const TARGET_LANGUAGES: [&str; 10] =
    ["en", "es", "pt", "fr", "it", "de", "ja", "zh", "ko", "ru"];

/// Qué se le pide a Grok.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WriterMode {
    /// Si el texto ya está en el idioma destino lo arregla; si no, lo traduce.
    #[default]
    Auto,
    Fix,
    Translate,
}
/// Cuánto explica cada cambio. Las explicaciones van en `explain_language`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WriterExplain {
    Off,
    /// Una línea por cambio.
    #[default]
    Brief,
    /// La regla gramatical o de uso, con un ejemplo.
    Detailed,
}
/// Cuánto puede cambiar el texto al arreglarlo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WriterStrength {
    /// Solo errores de gramática, ortografía y puntuación.
    Minimal,
    /// Además, lo que un nativo diría distinto.
    #[default]
    Natural,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WriterTone {
    /// Conserva el tono del original.
    #[default]
    Keep,
    Casual,
    Neutral,
    Formal,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct WriterConfig {
    /// Modo de cuenta propio. Mismas credenciales que el dictado.
    pub provider: ProviderMode,
    /// Atajo global que copia la selección y abre la ventana.
    pub shortcut: String,
    pub default_mode: WriterMode,
    /// ISO 639-1 del idioma al que se traduce y en el que se arregla en modo Auto.
    pub target_language: String,
    /// ISO 639-1 de las explicaciones. `auto` usa el idioma de la interfaz.
    pub explain_language: String,
    pub explain: WriterExplain,
    pub strength: WriterStrength,
    pub tone: WriterTone,
    /// Aplica las reglas anti "texto de IA" (sin rayas, sin vocabulario de IA…).
    pub unslop: bool,
    pub model: String,
    /// Preferencias libres que se agregan al prompt.
    pub instructions: String,
    /// Devolver al portapapeles lo que tenía antes de copiar la selección.
    pub restore_clipboard: bool,
    /// Copiar el resultado al portapapeles en cuanto llega.
    pub copy_on_finish: bool,
    pub window_centered: bool,
    pub window_x: f32,
    pub window_y: f32,
}
impl Default for WriterConfig {
    fn default() -> Self {
        Self {
            provider: ProviderMode::ApiKey,
            shortcut: "Ctrl+Alt+E".into(),
            default_mode: WriterMode::Auto,
            target_language: "en".into(),
            explain_language: "auto".into(),
            explain: WriterExplain::Brief,
            strength: WriterStrength::Natural,
            tone: WriterTone::Keep,
            unslop: true,
            model: "grok-4.3".into(),
            instructions: String::new(),
            restore_clipboard: true,
            copy_on_finish: true,
            window_centered: true,
            window_x: 24.0,
            window_y: 24.0,
        }
    }
}
impl WriterConfig {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            is_language_code(&self.target_language),
            "Idioma destino inválido; usá un código de dos letras"
        );
        ensure!(
            self.explain_language == "auto" || is_language_code(&self.explain_language),
            "Idioma de las explicaciones inválido"
        );
        ensure!(
            self.instructions.len() <= 4000,
            "Las preferencias de Escritura son demasiado largas"
        );
        ensure!(
            !self.model.trim().is_empty(),
            "Elegí un modelo de Escritura"
        );
        ensure!(
            self.window_x.is_finite() && self.window_y.is_finite(),
            "Posición de ventana inválida"
        );
        Ok(())
    }
}
fn is_language_code(code: &str) -> bool {
    code.len() == 2 && code.bytes().all(|b| b.is_ascii_lowercase())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WriterPhase {
    /// Ventana cerrada.
    #[default]
    Hidden,
    /// Copiando la selección.
    Capturing,
    /// Esperando a Grok.
    Working,
    Ready,
    Error,
}
/// Un cambio explicado. `original` y `corrected` son fragmentos literales.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct WriterChange {
    pub original: String,
    pub corrected: String,
    /// Vacío con `WriterExplain::Off`.
    pub why: String,
}
/// Tramo del diff por palabras entre entrada y resultado (solo modo Fix).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "text", rename_all = "snake_case")]
pub enum DiffSpan {
    Same(String),
    Removed(String),
    Added(String),
}
/// Resultado ya interpretado de una respuesta de Grok.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct WriterOutput {
    /// Lo que se hizo de verdad (nunca `Auto`).
    pub mode: WriterMode,
    /// ISO 639-1 detectado del texto de entrada.
    pub detected_language: String,
    pub text: String,
    pub changes: Vec<WriterChange>,
}
/// Estado de la ventana de Escritura que el controlador publica a la UI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct WriterSnapshot {
    pub phase: WriterPhase,
    /// Crece con cada pedido; descarta respuestas tardías.
    pub request_id: u64,
    /// Modo elegido en la ventana (puede ser `Auto`).
    pub mode: WriterMode,
    pub target_language: String,
    pub input: String,
    /// Se descartó texto por superar `MAX_INPUT_CHARS`.
    pub truncated: bool,
    pub output: Option<WriterOutput>,
    /// Diff por palabras de `input` contra `output.text`. Vacío al traducir.
    pub diff: Vec<DiffSpan>,
    pub status: String,
    pub error: Option<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    /// Se puede pegar el resultado en la ventana de origen.
    pub can_replace: bool,
}

/// Pedido completo para el proveedor. `Auto` se resuelve en el modelo.
#[derive(Debug, Clone, PartialEq)]
pub struct WriterRequest<'a> {
    pub mode: WriterMode,
    pub target_language: &'a str,
    /// Ya resuelto (nunca `auto`).
    pub explain_language: &'a str,
    pub config: &'a WriterConfig,
    pub text: &'a str,
}

/// Instrucciones de sistema para Grok: modo, idiomas, nivel de cambio, tono,
/// explicaciones, reglas unslop si `config.unslop`, defensa contra órdenes en
/// el texto y el formato JSON de salida (ver [`response_schema`]).
pub fn system_prompt(request: &WriterRequest<'_>) -> String {
    let _ = request;
    todo!("Escritura: prompt")
}
/// JSON Schema estricto de la salida: `mode` (fix|translate),
/// `detected_language`, `text` y `changes[] {original, corrected, why}`.
pub fn response_schema() -> serde_json::Value {
    todo!("Escritura: schema")
}
/// Interpreta el texto JSON devuelto por el modelo. Rechaza `text` vacío,
/// `mode` desconocido o `auto`.
pub fn parse_output(json: &str) -> Result<WriterOutput> {
    let _ = json;
    todo!("Escritura: parse")
}
/// Diff por palabras (conserva espacios y saltos de línea dentro de los tramos).
/// Concatenar `Same` + `Removed` reproduce `before`; `Same` + `Added`, `after`.
pub fn word_diff(before: &str, after: &str) -> Vec<DiffSpan> {
    let _ = (before, after);
    todo!("Escritura: diff")
}
/// Recorta a `MAX_INPUT_CHARS` en un límite de carácter y normaliza saltos de
/// línea `\r\n`. Devuelve si recortó.
pub fn prepare_input(text: &str) -> (String, bool) {
    let _ = text;
    todo!("Escritura: input")
}
