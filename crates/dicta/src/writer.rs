//! Escritura: arreglar y traducir texto con Grok. Vive en el hilo del
//! controlador, como la lectura: copia la selección en un hilo aparte, hace un
//! único pedido a Grok en Tokio y publica `snapshot.writer`. Los resultados
//! vuelven como `WriterEvent` y se descartan si llegan tarde (`request_id`).
//!
//! El texto nunca viaja por el IPC ni se registra. La selección se copia una
//! sola vez, al pulsar el atajo, con el mismo camino que la lectura
//! (`copy_selection`), y se recuerda la ventana de origen para Reemplazar.
use dicta_core::{
    AppConfig, AppSnapshot, Command,
    writer::{
        self, DiffSpan, WriterChange, WriterExplain, WriterMode, WriterOutput, WriterPhase,
        WriterRequest, WriterSnapshot,
    },
};
use dicta_platform::{
    desktop::{self, PasteTarget},
    selection::{self, SelectionSource},
};
use dicta_provider::{CancellationToken, Credentials, ProviderError, WriterResult, XaiProvider};
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    time::Duration,
};
use tokio::runtime::Handle;

/// Bytes máximos leídos de la selección; `prepare_input` recorta después a
/// `MAX_INPUT_CHARS`, con aviso.
const SELECTION_MAX_BYTES: usize = 4 * 1024 * 1024;
/// Texto de la demo: un error típico de quien aprende inglés.
const DEMO_TEXT: &str = "I has went to the store yesterday.";
/// Arreglos fijos de la demo: original, corregido y explicación.
const DEMO_FIXES: &[(&str, &str, &str)] = &[(
    "I has went",
    "I went",
    "«went» ya es pasado simple y no lleva auxiliar. «has» va con el participio («I have gone») y con he/she/it, no con I.",
)];
/// Windows envía Ctrl+C a la ventana en primer plano: abrir la ventana antes
/// de copiar le quitaría el foco a la selección. Ahí la ventana aparece cuando
/// ya se copió. En Hyprland el atajo va a la ventana por `stableId`.
const SHOW_WHILE_CAPTURING: bool = !cfg!(windows);

pub(crate) fn is_writer_command(command: &Command) -> bool {
    matches!(
        command,
        Command::WriterSelection
            | Command::WriterClipboard
            | Command::WriterSetInput(_)
            | Command::WriterRun(_)
            | Command::WriterSetTarget(_)
            | Command::WriterCopy
            | Command::WriterReplace
            | Command::WriterSpeak
            | Command::WriterClose
    )
}

/// Lo que el controlador debe hacer después de un comando de Escritura.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Outcome {
    /// Leer este texto en voz alta: el controlador lo pasa a la lectura como
    /// `Command::ReadText`.
    pub speak: Option<String>,
}

/// Funciones de texto puras del core. Las pruebas del controlador las
/// sustituyen para no depender de su implementación.
#[derive(Clone, Copy)]
pub(crate) struct TextOps {
    pub prepare: fn(&str) -> (String, bool),
    pub diff: fn(&str, &str) -> Vec<DiffSpan>,
}
impl Default for TextOps {
    fn default() -> Self {
        Self {
            prepare: writer::prepare_input,
            diff: writer::word_diff,
        }
    }
}

#[derive(Clone)]
enum Backend {
    Real {
        provider: XaiProvider,
        credentials: Credentials,
        allow_env: bool,
    },
    Demo {
        delay: Duration,
    },
}

/// Texto capturado y, si vino de una selección, la ventana de origen.
type Capture = Result<(Option<String>, Option<PasteTarget>), String>;

enum WriterEvent {
    Captured {
        capture: u64,
        empty: &'static str,
        result: Capture,
    },
    Done {
        request: u64,
        result: Result<WriterResult, ProviderError>,
    },
    Copied(Result<&'static str, String>),
    Replaced {
        epoch: u64,
        result: Result<&'static str, String>,
    },
}

struct InFlight {
    id: u64,
    cancel: CancellationToken,
}

/// Estado de la ventana antes de Reemplazar, para reabrirla si el pegado falla.
struct Stash {
    window: WriterSnapshot,
    source: PasteTarget,
}

pub(crate) struct Writer {
    runtime: Handle,
    backend: Backend,
    demo: bool,
    ops: TextOps,
    events_tx: Sender<WriterEvent>,
    events: Receiver<WriterEvent>,
    next_capture: u64,
    /// Captura en curso; otra captura o cerrar la invalida.
    capture: Option<u64>,
    request: Option<InFlight>,
    /// Ventana de la que se copió la selección. Nunca una ventana propia.
    source: Option<PasteTarget>,
    /// Crece al cerrar o abrir otra escritura: cancela un Reemplazar en cola.
    epoch: Arc<AtomicU64>,
    stash: Option<Stash>,
}

impl Writer {
    pub(crate) fn new(
        runtime: Handle,
        demo: bool,
        allow_env: bool,
        credentials: Credentials,
    ) -> anyhow::Result<Self> {
        let backend = if demo {
            Backend::Demo {
                delay: Duration::from_millis(600),
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
            TextOps::default(),
        ))
    }
    fn with_backend(runtime: Handle, backend: Backend, demo: bool, ops: TextOps) -> Self {
        let (events_tx, events) = mpsc::channel();
        Self {
            runtime,
            backend,
            demo,
            ops,
            events_tx,
            events,
            next_capture: 0,
            capture: None,
            request: None,
            source: None,
            epoch: Arc::new(AtomicU64::new(0)),
            stash: None,
        }
    }

    // ---- comandos ---------------------------------------------------------

    /// Ejecuta un comando de Escritura. Devuelve `Err(command)` si no lo es.
    pub(crate) fn handle(
        &mut self,
        command: Command,
        snap: &mut AppSnapshot,
    ) -> Result<Outcome, Command> {
        let mut outcome = Outcome::default();
        let visible = snap.writer.phase != WriterPhase::Hidden;
        match command {
            Command::WriterSelection => self.capture(true, snap),
            Command::WriterClipboard => self.capture(false, snap),
            Command::WriterSetInput(text) if visible => {
                let (input, truncated) = (self.ops.prepare)(&text);
                snap.writer.input = input;
                snap.writer.truncated = truncated;
            }
            Command::WriterRun(mode) if visible => self.run(mode, snap),
            Command::WriterSetTarget(language) => self.set_target(&language, snap),
            Command::WriterCopy if visible => match &snap.writer.output {
                Some(output) => {
                    let text = output.text.clone();
                    self.copy_result(text);
                }
                None => snap.writer.status = "Todavía no hay un resultado para copiar".into(),
            },
            Command::WriterSpeak if visible => match &snap.writer.output {
                Some(output) => {
                    outcome.speak = Some(output.text.clone());
                    snap.writer.status = "Leyendo el resultado en voz alta".into();
                }
                None => snap.writer.status = "Todavía no hay un resultado para escuchar".into(),
            },
            Command::WriterReplace if visible => self.replace(snap),
            Command::WriterClose => self.close(snap),
            command if is_writer_command(&command) => {}
            other => return Err(other),
        }
        Ok(outcome)
    }

    /// Abre la ventana y toma el texto: la selección (copiándola en la ventana
    /// activa) o el portapapeles. Reemplaza cualquier escritura anterior.
    fn capture(&mut self, copy: bool, snap: &mut AppSnapshot) {
        self.close(snap);
        self.next_capture += 1;
        let id = self.next_capture;
        self.capture = Some(id);
        let config = &snap.config.writer;
        snap.writer = WriterSnapshot {
            phase: if SHOW_WHILE_CAPTURING {
                WriterPhase::Capturing
            } else {
                WriterPhase::Hidden
            },
            request_id: snap.writer.request_id,
            mode: config.default_mode,
            target_language: config.target_language.clone(),
            status: if copy {
                "Copiando la selección…"
            } else {
                "Leyendo el portapapeles…"
            }
            .into(),
            ..Default::default()
        };
        let empty = if copy {
            "No había texto seleccionado. Escribí o pegá el texto y elegí Arreglar o Traducir"
        } else {
            "El portapapeles está vacío. Escribí o pegá el texto y elegí Arreglar o Traducir"
        };
        if self.demo {
            let source = copy.then(demo_source);
            let _ = self.events_tx.send(WriterEvent::Captured {
                capture: id,
                empty,
                result: Ok((Some(DEMO_TEXT.into()), source)),
            });
            return;
        }
        let restore = config.restore_clipboard;
        let terminals = snap.config.tts.terminal_classes.clone();
        let tx = self.events_tx.clone();
        self.runtime.spawn_blocking(move || {
            let result = if copy {
                // Copia en la ventana activa en el momento del atajo, antes de
                // que cambie el foco. Restaura el portapapeles aunque falle.
                selection::copy_selection_from(selection::CopyOptions {
                    max_bytes: SELECTION_MAX_BYTES,
                    restore_clipboard: restore,
                    extra_terminal_classes: &terminals,
                })
                .map(|copied| (copied.text, Some(copied.source)))
            } else {
                selection::read_text(SelectionSource::Clipboard, SELECTION_MAX_BYTES)
                    .map(|text| (text, None))
            };
            let _ = tx.send(WriterEvent::Captured {
                capture: id,
                empty,
                result: result.map_err(|e| e.to_string()),
            });
        });
    }

    fn on_captured(
        &mut self,
        capture: u64,
        empty: &'static str,
        result: Capture,
        snap: &mut AppSnapshot,
    ) {
        if self.capture != Some(capture) {
            return;
        }
        self.capture = None;
        let w = &mut snap.writer;
        match result {
            Ok((Some(text), source)) if !text.trim().is_empty() => {
                let (input, truncated) = (self.ops.prepare)(&text);
                w.input = input;
                w.truncated = truncated;
                w.can_replace = source.is_some();
                self.source = source;
                let mode = w.mode;
                self.run(mode, snap);
            }
            Ok(_) => {
                w.phase = WriterPhase::Ready;
                w.status = empty.into();
            }
            Err(error) => {
                w.phase = WriterPhase::Error;
                w.error = Some(error);
                w.status = "No se pudo tomar el texto; podés escribirlo o pegarlo".into();
            }
        }
    }

    /// Procesa `input` con `mode`. Cancela el pedido anterior: un solo pedido
    /// a la vez, sin reintentos automáticos.
    fn run(&mut self, mode: WriterMode, snap: &mut AppSnapshot) {
        if snap.writer.input.trim().is_empty() {
            if snap.writer.phase != WriterPhase::Hidden {
                snap.writer.status = "Escribí o pegá un texto para arreglarlo o traducirlo".into();
            }
            return;
        }
        self.cancel_request();
        let w = &mut snap.writer;
        w.request_id += 1;
        let id = w.request_id;
        w.mode = mode;
        w.phase = WriterPhase::Working;
        w.output = None;
        w.diff.clear();
        w.error = None;
        w.input_tokens = None;
        w.output_tokens = None;
        w.status = match mode {
            WriterMode::Fix => "Arreglando con Grok…",
            WriterMode::Translate => "Traduciendo con Grok…",
            WriterMode::Auto => "Revisando con Grok…",
        }
        .into();
        let cancel = CancellationToken::new();
        self.request = Some(InFlight {
            id,
            cancel: cancel.clone(),
        });
        let config = snap.config.clone();
        let input = w.input.clone();
        let target = w.target_language.clone();
        let explain = explain_language(&config);
        let tx = self.events_tx.clone();
        match self.backend.clone() {
            Backend::Real {
                provider,
                credentials,
                allow_env,
            } => {
                self.runtime.spawn(async move {
                    // El modo de cuenta propio de Escritura; nunca cambia solo a la API paga.
                    let result = match credentials
                        .bearer_for(&config, config.writer.provider, allow_env, &cancel)
                        .await
                    {
                        Ok(bearer) => {
                            let request = WriterRequest {
                                mode,
                                target_language: &target,
                                explain_language: &explain,
                                config: &config.writer,
                                text: &input,
                            };
                            provider.rewrite(&bearer, &request, &cancel).await
                        }
                        Err(e) => Err(e),
                    };
                    let _ = tx.send(WriterEvent::Done {
                        request: id,
                        result,
                    });
                });
            }
            Backend::Demo { delay } => {
                let detail = config.writer.explain;
                self.runtime.spawn(async move {
                    tokio::select! {
                        _ = cancel.cancelled() => return,
                        _ = tokio::time::sleep(delay) => {}
                    }
                    let output = demo_output(mode, &input, &target, detail);
                    let _ = tx.send(WriterEvent::Done {
                        request: id,
                        result: Ok(WriterResult {
                            output,
                            input_tokens: Some(180),
                            output_tokens: Some(60),
                        }),
                    });
                });
            }
        }
    }

    fn on_done(
        &mut self,
        request: u64,
        result: Result<WriterResult, ProviderError>,
        snap: &mut AppSnapshot,
    ) {
        let current = self.request.as_ref().is_some_and(|r| r.id == request)
            && snap.writer.request_id == request
            && snap.writer.phase == WriterPhase::Working;
        if !current {
            return;
        }
        self.request = None;
        let w = &mut snap.writer;
        match result {
            Ok(result) => {
                let output = result.output;
                w.diff = if output.mode == WriterMode::Fix {
                    (self.ops.diff)(&w.input, &output.text)
                } else {
                    vec![]
                };
                w.input_tokens = result.input_tokens;
                w.output_tokens = result.output_tokens;
                w.phase = WriterPhase::Ready;
                w.status = match output.mode {
                    WriterMode::Translate => "Texto traducido",
                    _ if output.changes.is_empty() && output.text == w.input => {
                        "El texto ya estaba bien"
                    }
                    _ => "Texto arreglado",
                }
                .into();
                let copy = snap
                    .config
                    .writer
                    .copy_on_finish
                    .then(|| output.text.clone());
                w.output = Some(output);
                if let Some(text) = copy {
                    self.copy_result(text);
                }
            }
            Err(ProviderError::Cancelled) => {
                w.phase = WriterPhase::Ready;
                w.status = "Pedido cancelado".into();
            }
            Err(error) => {
                w.phase = WriterPhase::Error;
                w.error = Some(friendly_error(&error));
                w.status = "Grok no completó el pedido".into();
            }
        }
    }

    fn set_target(&mut self, language: &str, snap: &mut AppSnapshot) {
        let language = language.trim().to_ascii_lowercase();
        if !is_language_code(&language) {
            snap.writer.status = "Idioma inválido; usá un código de dos letras".into();
            return;
        }
        if snap.writer.target_language == language {
            return;
        }
        snap.writer.target_language = language;
        let rerun = matches!(
            snap.writer.phase,
            WriterPhase::Working | WriterPhase::Ready | WriterPhase::Error
        ) && !snap.writer.input.trim().is_empty();
        if rerun {
            let mode = snap.writer.mode;
            self.run(mode, snap);
        }
    }

    fn copy_result(&self, text: String) {
        let tx = self.events_tx.clone();
        let demo = self.demo;
        self.runtime.spawn_blocking(move || {
            let result = if demo {
                Ok("Demo: copiado simulado")
            } else {
                desktop::copy(&text)
                    .map(|_| "Resultado copiado al portapapeles")
                    .map_err(|e| e.to_string())
            };
            let _ = tx.send(WriterEvent::Copied(result));
        });
    }

    /// Cierra la ventana, deja el resultado en el portapapeles, vuelve a la
    /// ventana de origen y pega con las teclas de pegado (Ctrl+Shift+V en
    /// terminales). Si falla, la ventana se reabre con el error.
    fn replace(&mut self, snap: &mut AppSnapshot) {
        let source = match (&self.source, snap.writer.can_replace) {
            (Some(source), true) => source.clone(),
            _ => {
                snap.writer.status =
                    "Reemplazar solo funciona con texto seleccionado en otra ventana; usá Copiar"
                        .into();
                return;
            }
        };
        let text = match (&snap.writer.output, snap.writer.phase) {
            (Some(output), WriterPhase::Ready) => output.text.clone(),
            _ => {
                snap.writer.status = "Todavía no hay un resultado para reemplazar".into();
                return;
            }
        };
        let window = snap.writer.clone();
        // Primero se cierra: así la ventana de origen recupera el foco.
        self.close(snap);
        let epoch = self.epoch.load(Ordering::Acquire);
        self.stash = Some(Stash {
            window,
            source: source.clone(),
        });
        let keys = snap.config.paste_keys.clone();
        let terminals = snap.config.tts.terminal_classes.clone();
        let current = self.epoch.clone();
        let tx = self.events_tx.clone();
        let demo = self.demo;
        self.runtime.spawn_blocking(move || {
            let result = if demo {
                Ok("Demo: reemplazo simulado")
            } else {
                desktop::paste_into(&text, &source, &keys, &terminals, || {
                    current.load(Ordering::Acquire) == epoch
                })
                .map(|_| "Texto reemplazado")
                .map_err(|e| e.to_string())
            };
            let _ = tx.send(WriterEvent::Replaced { epoch, result });
        });
    }

    fn on_replaced(
        &mut self,
        epoch: u64,
        result: Result<&'static str, String>,
        snap: &mut AppSnapshot,
    ) {
        if self.epoch.load(Ordering::Acquire) != epoch {
            return;
        }
        let Some(stash) = self.stash.take() else {
            return;
        };
        match result {
            Ok(message) => snap.status = message.into(),
            Err(error) => {
                // El resultado quedó en el portapapeles: se muestra de nuevo con el error.
                snap.writer = WriterSnapshot {
                    phase: WriterPhase::Error,
                    request_id: snap.writer.request_id,
                    error: Some(error),
                    status: "No se pudo reemplazar; el resultado está en el portapapeles".into(),
                    ..stash.window
                };
                self.source = Some(stash.source);
            }
        }
    }

    /// Cancela el pedido y la captura en curso y oculta la ventana.
    fn close(&mut self, snap: &mut AppSnapshot) {
        self.capture = None;
        self.cancel_request();
        self.epoch.fetch_add(1, Ordering::AcqRel);
        self.stash = None;
        self.source = None;
        snap.writer = WriterSnapshot {
            request_id: snap.writer.request_id,
            target_language: snap.writer.target_language.clone(),
            ..Default::default()
        };
    }

    fn cancel_request(&mut self) {
        if let Some(request) = self.request.take() {
            request.cancel.cancel();
        }
    }

    /// Atiende los resultados de las tareas. Devuelve si cambió el estado.
    pub(crate) fn poll(&mut self, snap: &mut AppSnapshot) -> bool {
        let mut dirty = false;
        while let Ok(event) = self.events.try_recv() {
            dirty = true;
            match event {
                WriterEvent::Captured {
                    capture,
                    empty,
                    result,
                } => self.on_captured(capture, empty, result, snap),
                WriterEvent::Done { request, result } => self.on_done(request, result, snap),
                WriterEvent::Copied(result) => {
                    if snap.writer.phase != WriterPhase::Hidden {
                        snap.writer.status = match result {
                            Ok(message) => message.into(),
                            Err(error) => error,
                        };
                    }
                }
                WriterEvent::Replaced { epoch, result } => self.on_replaced(epoch, result, snap),
            }
        }
        dirty
    }

    /// Quit: cancela el pedido y cualquier Reemplazar en cola.
    pub(crate) fn shutdown(&mut self, snap: &mut AppSnapshot) {
        self.close(snap);
    }
}

// ---------------------------------------------------------------------------
// Funciones auxiliares
// ---------------------------------------------------------------------------

fn is_language_code(code: &str) -> bool {
    code.len() == 2 && code.bytes().all(|b| b.is_ascii_lowercase())
}

/// Idioma de las explicaciones: el elegido o, con `auto`, el de la interfaz
/// (que con `auto` sigue al sistema, como hace la UI: español o inglés).
pub(crate) fn explain_language(config: &AppConfig) -> String {
    let explain = config.writer.explain_language.trim();
    if explain != "auto" && is_language_code(explain) {
        return explain.to_owned();
    }
    ui_language(&config.ui_language, sys_locale::get_locale().as_deref()).into()
}
fn ui_language(ui_language: &str, locale: Option<&str>) -> &'static str {
    match ui_language {
        "es" => "es",
        "en" => "en",
        _ => {
            let primary = locale
                .unwrap_or_default()
                .split(['-', '_', '.', '@'])
                .next()
                .unwrap_or_default();
            if primary.eq_ignore_ascii_case("es") {
                "es"
            } else {
                "en"
            }
        }
    }
}

/// Los mensajes del proveedor nombran el dictado o STT en algunos casos.
fn friendly_error(error: &ProviderError) -> String {
    match error {
        ProviderError::Forbidden => "Tu cuenta no autorizó este pedido a Grok (403). Si tenés una API key, elegí «API key» en Ajustes → Escritura; no se cambió a API paga automáticamente".into(),
        ProviderError::Disconnected => "xAI cerró la conexión antes de responder. Volvé a intentar".into(),
        ProviderError::Upstream => "xAI no pudo procesar el texto. Volvé a intentar".into(),
        other => other.to_string(),
    }
}

/// Ventana de origen ficticia: en la demo Reemplazar se simula.
fn demo_source() -> PasteTarget {
    PasteTarget {
        address: "0x1".into(),
        pid: 1,
        class: "demo".into(),
        initial_class: "demo".into(),
        stable_id: Some("demo".into()),
    }
}

/// Resultado fijo de la demo, sin red: `Auto` y `Fix` arreglan los errores de
/// [`DEMO_FIXES`]; `Translate` marca el texto como traducción simulada.
pub(crate) fn demo_output(
    mode: WriterMode,
    input: &str,
    target: &str,
    explain: WriterExplain,
) -> WriterOutput {
    if mode == WriterMode::Translate {
        return WriterOutput {
            mode: WriterMode::Translate,
            detected_language: "en".into(),
            text: format!("[Demo: traducción simulada a «{target}»] {input}"),
            changes: vec![],
        };
    }
    let mut text = input.to_owned();
    let mut changes = vec![];
    for (original, corrected, why) in DEMO_FIXES {
        if text.contains(original) {
            text = text.replacen(original, corrected, 1);
            changes.push(WriterChange {
                original: (*original).into(),
                corrected: (*corrected).into(),
                why: if explain == WriterExplain::Off {
                    String::new()
                } else {
                    (*why).into()
                },
            });
        }
    }
    WriterOutput {
        mode: WriterMode::Fix,
        detected_language: "en".into(),
        text,
        changes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// Sustitutos de las funciones del core, para probar solo el controlador.
    fn ops() -> TextOps {
        TextOps {
            prepare: |text| (text.chars().take(20).collect(), text.chars().count() > 20),
            diff: |before, after| {
                vec![
                    DiffSpan::Removed(before.to_owned()),
                    DiffSpan::Added(after.to_owned()),
                ]
            },
        }
    }
    struct Fixture {
        runtime: tokio::runtime::Runtime,
        writer: Writer,
        snap: AppSnapshot,
    }
    impl Fixture {
        fn new(delay: Duration) -> Self {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .unwrap();
            let writer = Writer::with_backend(
                runtime.handle().clone(),
                Backend::Demo { delay },
                true,
                ops(),
            );
            let mut snap = AppSnapshot::new(AppConfig::default());
            snap.demo = true;
            Self {
                runtime,
                writer,
                snap,
            }
        }
        fn send(&mut self, command: Command) -> Outcome {
            match self.writer.handle(command, &mut self.snap) {
                Ok(outcome) => outcome,
                Err(_) => panic!("no es un comando de Escritura"),
            }
        }
        fn until(&mut self, predicate: impl Fn(&WriterSnapshot) -> bool) {
            let deadline = Instant::now() + Duration::from_secs(3);
            while !predicate(&self.snap.writer) {
                assert!(Instant::now() < deadline, "{:?}", self.snap.writer);
                self.writer.poll(&mut self.snap);
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        fn settle(&mut self) {
            std::thread::sleep(Duration::from_millis(30));
            self.writer.poll(&mut self.snap);
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.writer.shutdown(&mut self.snap);
            let _ = &self.runtime;
        }
    }
    fn result(text: &str) -> Result<WriterResult, ProviderError> {
        Ok(WriterResult {
            output: WriterOutput {
                mode: WriterMode::Fix,
                detected_language: "en".into(),
                text: text.into(),
                changes: vec![],
            },
            input_tokens: Some(1),
            output_tokens: Some(2),
        })
    }

    #[test]
    fn demo_selection_opens_fixes_copies_and_offers_replace() {
        let mut f = Fixture::new(Duration::ZERO);
        f.send(Command::WriterSelection);
        if SHOW_WHILE_CAPTURING {
            assert_eq!(f.snap.writer.phase, WriterPhase::Capturing);
        }
        f.until(|w| w.phase == WriterPhase::Ready && w.status == "Demo: copiado simulado");
        let w = &f.snap.writer;
        assert_eq!(w.input, "I has went to the st");
        assert!(w.truncated);
        assert!(w.can_replace);
        assert_eq!(w.mode, WriterMode::Auto);
        assert_eq!(w.target_language, "en");
        let output = w.output.as_ref().unwrap();
        assert_eq!(output.mode, WriterMode::Fix);
        assert_eq!(output.text, "I went to the st");
        assert_eq!(w.diff.len(), 2);
        assert_eq!((w.input_tokens, w.output_tokens), (Some(180), Some(60)));
        // Reemplazar cierra primero y el resultado llega a la barra principal.
        f.send(Command::WriterReplace);
        assert_eq!(f.snap.writer.phase, WriterPhase::Hidden);
        assert!(f.snap.writer.input.is_empty() && f.snap.writer.output.is_none());
        let deadline = Instant::now() + Duration::from_secs(3);
        while f.snap.status != "Demo: reemplazo simulado" {
            assert!(Instant::now() < deadline);
            f.settle();
        }
        assert_eq!(f.snap.writer.phase, WriterPhase::Hidden);
    }

    #[test]
    fn clipboard_text_cannot_be_replaced_and_translation_has_no_diff() {
        let mut f = Fixture::new(Duration::ZERO);
        f.snap.config.writer.copy_on_finish = false;
        f.send(Command::WriterClipboard);
        f.until(|w| w.phase == WriterPhase::Ready);
        assert!(!f.snap.writer.can_replace);
        f.send(Command::WriterReplace);
        assert_eq!(f.snap.writer.phase, WriterPhase::Ready);
        assert!(f.snap.writer.status.contains("usá Copiar"));
        f.send(Command::WriterRun(WriterMode::Translate));
        f.until(|w| w.phase == WriterPhase::Ready);
        let w = &f.snap.writer;
        assert_eq!(w.output.as_ref().unwrap().mode, WriterMode::Translate);
        assert!(w.diff.is_empty());
        assert_eq!(w.status, "Texto traducido");
        // Escuchar pasa el resultado a la lectura.
        let speak = f.send(Command::WriterSpeak).speak.unwrap();
        assert!(speak.contains("traducción simulada"));
    }

    #[test]
    fn stale_responses_are_discarded() {
        let mut f = Fixture::new(Duration::from_secs(60));
        f.send(Command::WriterClipboard);
        f.until(|w| w.phase == WriterPhase::Working);
        let first = f.snap.writer.request_id;
        f.send(Command::WriterRun(WriterMode::Fix));
        let second = f.snap.writer.request_id;
        assert_eq!(second, first + 1);
        f.writer
            .events_tx
            .send(WriterEvent::Done {
                request: first,
                result: result("viejo"),
            })
            .unwrap();
        f.settle();
        assert_eq!(f.snap.writer.phase, WriterPhase::Working);
        assert!(f.snap.writer.output.is_none());
        f.writer
            .events_tx
            .send(WriterEvent::Done {
                request: second,
                result: result("nuevo"),
            })
            .unwrap();
        f.until(|w| w.phase == WriterPhase::Ready);
        assert_eq!(f.snap.writer.output.as_ref().unwrap().text, "nuevo");
        // Una respuesta repetida tampoco pisa el estado.
        f.writer
            .events_tx
            .send(WriterEvent::Done {
                request: second,
                result: result("repetido"),
            })
            .unwrap();
        f.settle();
        assert_eq!(f.snap.writer.output.as_ref().unwrap().text, "nuevo");
    }

    #[test]
    fn close_cancels_the_request_and_clears_the_window() {
        let mut f = Fixture::new(Duration::from_secs(60));
        f.send(Command::WriterClipboard);
        f.until(|w| w.phase == WriterPhase::Working);
        let request = f.snap.writer.request_id;
        let cancel = f.writer.request.as_ref().unwrap().cancel.clone();
        f.send(Command::WriterClose);
        assert!(cancel.is_cancelled());
        let w = &f.snap.writer;
        assert_eq!(w.phase, WriterPhase::Hidden);
        assert!(w.input.is_empty() && w.output.is_none() && w.status.is_empty());
        assert_eq!(w.request_id, request);
        f.writer
            .events_tx
            .send(WriterEvent::Done {
                request,
                result: result("tarde"),
            })
            .unwrap();
        f.settle();
        assert_eq!(f.snap.writer.phase, WriterPhase::Hidden);
        // Con la ventana cerrada, editar o procesar no hace nada.
        f.send(Command::WriterSetInput("hola".into()));
        f.send(Command::WriterRun(WriterMode::Fix));
        assert_eq!(f.snap.writer.phase, WriterPhase::Hidden);
        assert!(f.snap.writer.input.is_empty());
    }

    #[test]
    fn a_late_capture_after_close_does_not_reopen_the_window() {
        let mut f = Fixture::new(Duration::ZERO);
        f.send(Command::WriterSelection);
        f.send(Command::WriterClose);
        f.settle();
        assert_eq!(f.snap.writer.phase, WriterPhase::Hidden);
        assert!(f.writer.request.is_none());
    }

    #[test]
    fn set_target_validates_and_reruns_with_text() {
        let mut f = Fixture::new(Duration::from_secs(60));
        f.send(Command::WriterClipboard);
        f.until(|w| w.phase == WriterPhase::Working);
        let request = f.snap.writer.request_id;
        f.send(Command::WriterSetTarget("esp".into()));
        assert_eq!(f.snap.writer.target_language, "en");
        assert_eq!(f.snap.writer.request_id, request);
        assert!(f.snap.writer.status.contains("dos letras"));
        f.send(Command::WriterSetTarget(" ES ".into()));
        assert_eq!(f.snap.writer.target_language, "es");
        assert_eq!(f.snap.writer.request_id, request + 1);
        assert_eq!(f.snap.writer.phase, WriterPhase::Working);
        // Sin texto no hay pedido.
        f.send(Command::WriterSetInput("   ".into()));
        f.send(Command::WriterSetTarget("pt".into()));
        assert_eq!(f.snap.writer.target_language, "pt");
        f.send(Command::WriterRun(WriterMode::Fix));
        assert_eq!(f.snap.writer.request_id, request + 1);
        assert!(f.snap.writer.status.contains("Escribí o pegá"));
    }

    #[test]
    fn errors_show_the_window_with_a_friendly_message() {
        let mut f = Fixture::new(Duration::from_secs(60));
        f.send(Command::WriterClipboard);
        f.until(|w| w.phase == WriterPhase::Working);
        let request = f.snap.writer.request_id;
        f.writer
            .events_tx
            .send(WriterEvent::Done {
                request,
                result: Err(ProviderError::Forbidden),
            })
            .unwrap();
        f.until(|w| w.phase == WriterPhase::Error);
        let error = f.snap.writer.error.clone().unwrap();
        assert!(error.contains("Ajustes → Escritura"), "{error}");
        assert!(!error.contains("STT"), "{error}");
        // Reintentar desde el error vuelve a pedir.
        f.send(Command::WriterRun(WriterMode::Fix));
        assert_eq!(f.snap.writer.phase, WriterPhase::Working);
        assert!(f.snap.writer.error.is_none());
    }

    #[test]
    fn empty_capture_opens_the_window_without_a_request() {
        let mut f = Fixture::new(Duration::ZERO);
        f.writer.demo = false; // sin hilo de copia: el evento se inyecta
        f.writer.capture = Some(7);
        f.snap.writer.phase = WriterPhase::Capturing;
        f.writer
            .events_tx
            .send(WriterEvent::Captured {
                capture: 7,
                empty: "No había texto seleccionado",
                result: Ok((None, Some(demo_source()))),
            })
            .unwrap();
        f.settle();
        let w = &f.snap.writer;
        assert_eq!(w.phase, WriterPhase::Ready);
        assert_eq!(w.status, "No había texto seleccionado");
        assert!(!w.can_replace);
        assert_eq!(w.request_id, 0);
        f.writer.capture = Some(8);
        f.writer
            .events_tx
            .send(WriterEvent::Captured {
                capture: 8,
                empty: "",
                result: Err("Seleccioná el texto en otra ventana".into()),
            })
            .unwrap();
        f.settle();
        assert_eq!(f.snap.writer.phase, WriterPhase::Error);
    }

    #[test]
    fn failed_replace_reopens_the_window_with_the_result() {
        let mut f = Fixture::new(Duration::ZERO);
        f.snap.config.writer.copy_on_finish = false;
        f.send(Command::WriterSelection);
        f.until(|w| w.phase == WriterPhase::Ready);
        let text = f.snap.writer.output.clone().unwrap().text;
        // Simula el fallo del pegado reemplazando el resultado de la tarea.
        f.send(Command::WriterReplace);
        let epoch = f.writer.epoch.load(Ordering::Acquire);
        std::thread::sleep(Duration::from_millis(30));
        while f.writer.events.try_recv().is_ok() {}
        f.writer
            .events_tx
            .send(WriterEvent::Replaced {
                epoch,
                result: Err("La ventana de origen ya no está abierta".into()),
            })
            .unwrap();
        f.settle();
        let w = &f.snap.writer;
        assert_eq!(w.phase, WriterPhase::Error);
        assert_eq!(w.output.as_ref().unwrap().text, text);
        assert!(w.error.as_deref().unwrap().contains("ya no está abierta"));
        assert!(f.writer.source.is_some());
        // Un resultado de un Reemplazar anterior a otra escritura se ignora.
        f.send(Command::WriterClose);
        f.writer
            .events_tx
            .send(WriterEvent::Replaced {
                epoch,
                result: Err("viejo".into()),
            })
            .unwrap();
        f.settle();
        assert_eq!(f.snap.writer.phase, WriterPhase::Hidden);
    }

    #[test]
    fn demo_output_fixes_the_example_with_one_explained_change() {
        let output = demo_output(WriterMode::Auto, DEMO_TEXT, "en", WriterExplain::Brief);
        assert_eq!(output.mode, WriterMode::Fix);
        assert_eq!(output.text, "I went to the store yesterday.");
        assert_eq!(output.changes.len(), 1);
        assert_eq!(output.changes[0].original, "I has went");
        assert_eq!(output.changes[0].corrected, "I went");
        assert!(!output.changes[0].why.is_empty());
        let quiet = demo_output(WriterMode::Fix, DEMO_TEXT, "en", WriterExplain::Off);
        assert!(quiet.changes[0].why.is_empty());
        let translated = demo_output(WriterMode::Translate, "hola", "fr", WriterExplain::Brief);
        assert_eq!(translated.mode, WriterMode::Translate);
        assert!(translated.text.contains("«fr»") && translated.text.ends_with("hola"));
        assert_eq!(
            demo_output(WriterMode::Fix, "Fine.", "en", WriterExplain::Brief).changes,
            vec![]
        );
    }

    #[test]
    fn explanations_follow_the_interface_language() {
        assert_eq!(ui_language("es", Some("en_US.UTF-8")), "es");
        assert_eq!(ui_language("en", Some("es-AR")), "en");
        assert_eq!(ui_language("auto", Some("es_AR.UTF-8")), "es");
        assert_eq!(ui_language("auto", Some("pt-BR")), "en");
        assert_eq!(ui_language("auto", None), "en");
        let mut config = AppConfig::default();
        config.writer.explain_language = "fr".into();
        assert_eq!(explain_language(&config), "fr");
        config.writer.explain_language = "auto".into();
        config.ui_language = "es".into();
        assert_eq!(explain_language(&config), "es");
    }

    #[test]
    fn only_writer_commands_are_taken() {
        let mut f = Fixture::new(Duration::ZERO);
        assert!(matches!(
            f.writer.handle(Command::TtsStop, &mut f.snap),
            Err(Command::TtsStop)
        ));
        // Con la ventana cerrada los comandos de la ventana se ignoran sin error.
        assert!(f.writer.handle(Command::WriterCopy, &mut f.snap).is_ok());
        assert!(f.send(Command::WriterSpeak).speak.is_none());
        assert!(is_writer_command(&Command::WriterSetTarget("en".into())));
        assert!(!is_writer_command(&Command::ReadSelection));
    }
}
