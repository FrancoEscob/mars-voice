//! Interface language: a compile-time catalogue of every user-facing string.
//!
//! Each [`Msg`] is declared once with its Spanish (rioplatense, the original
//! copy) and English text, so a missing translation does not compile and a
//! lookup is a `match`, with no files, allocation or hashing at runtime.
//! Texts with values use named placeholders (`{n}`, `{key}`) filled by
//! [`fill`] or the [`tf!`] macro; tests check both languages name the same
//! placeholders.
//!
//! The active language lives in [`crate::UiState::lang`]. `ui_language =
//! "auto"` follows the system locale ([`Lang::system`]); changing it in
//! Ajustes switches every window at once (see `Hub::apply_language`).
//!
//! Status and error text written by the controller (`AppSnapshot::status`,
//! `error`, `TtsSnapshot::status`, `notice`) arrives already in Spanish and is
//! shown as is; it needs message ids on the controller side.
//!
//! Copy that only applies to one desktop lives in messages whose id starts
//! with `Hypr` or `Linux` and is shown only for that [`crate::session::Session`].

use std::fmt::Display;

/// A language the interface is translated to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Lang {
    /// The original copy, rioplatense Spanish.
    #[default]
    Es,
    En,
}

impl Lang {
    pub const ALL: [Self; 2] = [Self::Es, Self::En];

    /// The text of `m` in this language.
    #[inline]
    pub fn t(self, m: Msg) -> &'static str {
        match self {
            Self::Es => m.es(),
            Self::En => m.en(),
        }
    }

    /// BCP-47 or POSIX locale (`es-AR`, `en_US.UTF-8`, `pt-BR`) to an
    /// interface language: Spanish for any Spanish locale, English otherwise.
    pub fn from_locale(locale: &str) -> Self {
        let primary = locale
            .split(['-', '_', '.', '@'])
            .next()
            .unwrap_or_default();
        if primary.eq_ignore_ascii_case("es") {
            Self::Es
        } else {
            Self::En
        }
    }

    /// Language of the system locale. Without a readable locale, English.
    pub fn system() -> Self {
        sys_locale::get_locale()
            .map(|l| Self::from_locale(&l))
            .unwrap_or(Self::En)
    }

    /// `AppConfig::ui_language` ("auto", "es" or "en") with `system` for auto.
    pub fn resolve(ui_language: &str, system: Self) -> Self {
        match ui_language {
            "es" => Self::Es,
            "en" => Self::En,
            _ => system,
        }
    }

    /// Value stored in `AppConfig::ui_language` for an explicit choice.
    pub fn code(self) -> &'static str {
        match self {
            Self::Es => "es",
            Self::En => "en",
        }
    }

    /// Thousands separator for counts.
    pub fn thousands(self) -> char {
        match self {
            Self::Es => '.',
            Self::En => ',',
        }
    }

    /// Decimal separator.
    pub fn decimal(self) -> char {
        match self {
            Self::Es => ',',
            Self::En => '.',
        }
    }
}

/// Replaces each `{name}` in `template` with its value. Unknown names stay
/// as written, so a missing argument is visible instead of silently empty.
pub fn fill(template: &str, args: &[(&str, &dyn Display)]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('}') {
            Some(close) => {
                let name = &after[..close];
                match args.iter().find(|(k, _)| *k == name) {
                    Some((_, v)) => {
                        use std::fmt::Write as _;
                        let _ = write!(out, "{v}");
                    }
                    None => {
                        out.push('{');
                        out.push_str(name);
                        out.push('}');
                    }
                }
                rest = &after[close + 1..];
            }
            None => {
                out.push_str(&rest[open..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// Placeholders named in a template, in order of appearance.
#[cfg(test)]
fn placeholders(template: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else { break };
        out.push(&after[..close]);
        rest = &after[close + 1..];
    }
    out
}

/// `tf!(lang, Id, name = value, …)`: the text of `Msg::Id` in `lang` with its
/// placeholders filled.
macro_rules! tf {
    ($lang:expr, $id:ident $(, $k:ident = $v:expr)* $(,)?) => {
        $crate::i18n::fill(
            $lang.t($crate::i18n::Msg::$id),
            &[$((stringify!($k), &$v as &dyn ::std::fmt::Display)),*],
        )
    };
}
pub(crate) use tf;

macro_rules! catalogue {
    ($($id:ident { es: $es:literal, en: $en:literal $(,)? })*) => {
        /// Every user-facing string of the interface.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Msg {
            $($id,)*
        }

        impl Msg {
            /// Every message, for tests and tools.
            pub const ALL: &'static [Msg] = &[$(Msg::$id,)*];

            pub const fn es(self) -> &'static str {
                match self {
                    $(Msg::$id => $es,)*
                }
            }

            pub const fn en(self) -> &'static str {
                match self {
                    $(Msg::$id => $en,)*
                }
            }

            /// The identifier, for tests and diagnostics.
            pub const fn id(self) -> &'static str {
                match self {
                    $(Msg::$id => stringify!($id),)*
                }
            }
        }
    };
}

catalogue! {
    // ------------------------------------------------------------ common
    Cancel { es: "Cancelar", en: "Cancel" }
    Retry { es: "Reintentar", en: "Retry" }
    Settings { es: "Ajustes", en: "Settings" }
    Add { es: "Agregar", en: "Add" }
    Remember { es: "Recordar", en: "Remember" }
    Copy { es: "Copiar", en: "Copy" }
    CopyText { es: "Copiar texto", en: "Copy text" }
    Paste { es: "Pegar", en: "Paste" }
    Delete { es: "Borrar", en: "Delete" }
    DeleteAudio { es: "Borrar audio", en: "Delete audio" }
    Disconnect { es: "Desconectar", en: "Disconnect" }
    Dictate { es: "Dictar", en: "Dictate" }
    Stop { es: "Detener", en: "Stop" }
    Play { es: "Escuchar", en: "Play" }
    Read { es: "Leer", en: "Read" }
    ReadClipboard { es: "Leer portapapeles", en: "Read clipboard" }
    RefreshList { es: "Actualizar lista", en: "Refresh list" }
    Default { es: "Predeterminado.", en: "Default." }
    Automatic { es: "Automático", en: "Automatic" }
    Centre { es: "Centro", en: "Center" }
    Horizontal { es: "Horizontal (px)", en: "Horizontal (px)" }
    Vertical { es: "Vertical (px)", en: "Vertical (px)" }
    TopLeft { es: "Arriba izquierda", en: "Top left" }
    TopCentre { es: "Arriba centro", en: "Top center" }
    TopRight { es: "Arriba derecha", en: "Top right" }
    BottomCentre { es: "Abajo centro", en: "Bottom center" }
    BottomRight { es: "Abajo derecha", en: "Bottom right" }
    Audio { es: "Audio", en: "Audio" }
    NotAvailable { es: "No disponible", en: "Not available" }
    NoText { es: "Sin texto", en: "No text" }
    WithError { es: "Con error", en: "With error" }
    Completed { es: "Completada", en: "Completed" }
    Simulated { es: "simulado, no es una cuenta real", en: "simulated, not a real account" }
    ConnectGrok { es: "Conectar con Grok", en: "Connect to Grok" }
    SaveKey { es: "Guardar clave", en: "Save key" }
    CredentialAvailable { es: "Credencial disponible", en: "Credential available" }
    ExperimentalBadge { es: "Experimental", en: "Experimental" }
    RecommendedBadge { es: "Recomendada", en: "Recommended" }
    PercentValue { es: "{v} %", en: "{v}%" }
    Characters { es: "{n} caracteres", en: "{n} characters" }
    LangSpanish { es: "Español", en: "Spanish" }
    LangEnglish { es: "Inglés", en: "English" }
    LangPortuguese { es: "Portugués", en: "Portuguese" }

    // ------------------------------------------------------- formatting
    LessThan { es: "menos de {amount}", en: "less than {amount}" }
    WordsOne { es: "1 palabra", en: "1 word" }
    WordsMany { es: "{n} palabras", en: "{n} words" }
    DateToday { es: "Hoy {time}", en: "Today {time}" }
    DateYesterday { es: "Ayer {time}", en: "Yesterday {time}" }
    DateUnknown { es: "Fecha desconocida", en: "Unknown date" }
    CostEstimate { es: "≈ {amount} estimado", en: "≈ {amount} estimated" }
    CostBelowEstimate { es: "menos de {amount} estimado", en: "less than {amount} estimated" }

    // ------------------------------------------------- phases and styles
    PhaseIdle { es: "En espera", en: "Idle" }
    PhaseConnecting { es: "Conectando", en: "Connecting" }
    PhaseRecording { es: "Grabando", en: "Recording" }
    PhaseFinalizing { es: "Finalizando", en: "Finishing" }
    PhasePolishing { es: "Puliendo", en: "Polishing" }
    PhaseReady { es: "Listo", en: "Ready" }
    PhaseError { es: "Error", en: "Error" }
    TtsIdle { es: "En espera", en: "Idle" }
    TtsReading { es: "Leyendo la selección…", en: "Reading the selection…" }
    TtsConfirm { es: "Confirmá la lectura", en: "Confirm the reading" }
    TtsConnecting { es: "Generando…", en: "Generating…" }
    TtsBuffering { es: "Esperando audio…", en: "Waiting for audio…" }
    TtsPlaying { es: "Leyendo", en: "Reading aloud" }
    TtsPaused { es: "En pausa", en: "Paused" }
    TtsFinished { es: "Lectura terminada", en: "Reading finished" }
    TtsError { es: "No se pudo leer", en: "Couldn't read" }
    ThemeSignal { es: "Señal", en: "Signal" }
    ThemeRegolith { es: "Regolito", en: "Regolith" }
    ThemeOrbit { es: "Órbita", en: "Orbit" }
    BlurbSignal {
        es: "Onda continua y limpia. Naranja solo en Marte y Terminar.",
        en: "A clean, continuous wave. Orange only on Mars and Finish.",
    }
    BlurbRegolith {
        es: "Columnas de puntos con grano suave y el planeta asomando.",
        en: "Columns of dots, soft grain and the planet peeking in.",
    }
    BlurbOrbit {
        es: "Marte al centro con anillos de polvo que crecen con tu voz.",
        en: "Mars in the center, with rings of dust that grow with your voice.",
    }
    ModeLight { es: "Claro", en: "Light" }
    ModeDark { es: "Oscuro", en: "Dark" }
    VeryLoud { es: "Nivel muy alto", en: "Very loud" }

    // ------------------------------------------------------------ errors
    ErrTitleMic { es: "No se pudo usar el micrófono", en: "Couldn't use the microphone" }
    ErrTitleCredential { es: "Problema con la credencial", en: "Problem with the credential" }
    ErrTitleDictation { es: "No se completó el dictado", en: "The dictation didn't finish" }
    ErrTitleApp { es: "Mars voice informó un error", en: "Mars voice reported an error" }
    FontsFailed {
        es: "No se pudieron cargar las fuentes de Mars voice",
        en: "Couldn't load the Mars voice fonts",
    }

    // ----------------------------------------------------------- overlay
    FinalTextPlaceholder { es: "El texto final aparece acá", en: "The final text appears here" }
    PolishingWith { es: "Puliendo con {model}", en: "Polishing with {model}" }
    HintToDictate { es: "para dictar", en: "to dictate" }
    HintFinishes { es: "termina", en: "to finish" }
    HintHold { es: "Mantené", en: "Hold" }
    HintRelease { es: "Soltá", en: "Release" }
    Listening { es: "Te escucho…", en: "Listening…" }
    ConnectToDictate { es: "Conectá Grok para dictar", en: "Connect Grok to dictate" }
    ReadyToDictate { es: "Listo para dictar", en: "Ready to dictate" }
    Connect { es: "Conectar", en: "Connect" }
    ConnectingGrok { es: "Conectando con Grok…", en: "Connecting to Grok…" }
    Finish { es: "Terminar", en: "Finish" }
    SaveEditKey { es: "{key} guarda la edición", en: "{key} saves the edit" }
    EditedStatus { es: "Editado · {status}", en: "Edited · {status}" }
    RememberProposal { es: "¿Recordar «{from}» → «{to}»?", en: "Remember “{from}” → “{to}”?" }
    NewDictation { es: "Nuevo dictado", en: "New dictation" }
    ChooseMic { es: "Elegir micrófono", en: "Choose microphone" }
    CheckConnection { es: "Revisar conexión", en: "Check connection" }

    // ---------------------------------------------------------- sections
    SecHome { es: "Inicio", en: "Home" }
    SecHistory { es: "Historial", en: "History" }
    SecGeneral { es: "General", en: "General" }
    SecProvider { es: "Proveedor", en: "Provider" }
    SecWriting { es: "Escritura", en: "Writing" }
    SecVocabulary { es: "Vocabulario", en: "Vocabulary" }
    SecCorrections { es: "Correcciones", en: "Corrections" }
    SecMicrophone { es: "Micrófono", en: "Microphone" }
    SecShortcuts { es: "Atajos", en: "Shortcuts" }
    SecAppearance { es: "Apariencia", en: "Appearance" }
    SecData { es: "Historial y audio", en: "History & audio" }
    SecOverlay { es: "Overlay", en: "Overlay" }
    SecBackground { es: "Segundo plano", en: "Background" }
    SecReading { es: "Lectura", en: "Read aloud" }
    SubHome {
        es: "Tu uso de dictado y lectura, y las últimas notas.",
        en: "Your dictation and read-aloud usage, and your latest notes.",
    }
    SubHistory {
        es: "Tus dictados y lecturas, guardados en este equipo.",
        en: "Your dictations and readings, stored on this device.",
    }
    SubGeneral {
        es: "Idioma y qué pasa al terminar de dictar.",
        en: "Language, and what happens when you finish dictating.",
    }
    SubProvider { es: "Cómo se conecta Mars voice con Grok.", en: "How Mars voice connects to Grok." }
    SubWriting {
        es: "Pulido y formato del texto transcripto.",
        en: "Polishing and formatting of the transcribed text.",
    }
    SubVocabulary {
        es: "Nombres y términos que Grok debe reconocer.",
        en: "Names and terms Grok should recognize.",
    }
    SubCorrections {
        es: "Lo que Mars voice aprendió, siempre con tu aprobación.",
        en: "What Mars voice learned, always with your approval.",
    }
    SubMicrophone {
        es: "Qué entrada se usa mientras dictás.",
        en: "Which input is used while you dictate.",
    }
    SubShortcuts {
        es: "Cómo empezar, terminar y pegar sin tocar el mouse.",
        en: "How to start, finish and paste without touching the mouse.",
    }
    SubAppearance {
        es: "Estilo de la voz y colores claros u oscuros.",
        en: "Voice style, and light or dark colors.",
    }
    SubData {
        es: "Qué se guarda de cada dictado y por cuánto tiempo.",
        en: "What is kept from each dictation, and for how long.",
    }
    SubOverlay {
        es: "Dónde aparece la tarjeta de dictado y cuándo se muestra.",
        en: "Where the dictation card appears and when it shows.",
    }
    SubBackground {
        es: "Qué pasa al cerrar esta ventana.",
        en: "What happens when you close this window.",
    }
    SubReading {
        es: "Leer en voz alta lo que selecciones: atajo, cuenta, voz y ventana.",
        en: "Read aloud whatever you select: shortcut, account, voice and window.",
    }
    NavDictation { es: "Dictado", en: "Dictation" }
    NavText { es: "Texto", en: "Text" }
    NavApp { es: "Aplicación", en: "App" }
    NavModePress { es: "pulsar", en: "press" }
    NavModeHold { es: "mantener", en: "hold" }
    NavCheckMic { es: "Revisá el micrófono", en: "Check the microphone" }
    NavCheckConnection { es: "Revisá la conexión", en: "Check the connection" }
    NavGrokSimulated { es: "Grok simulado", en: "Simulated Grok" }
    NavGrokConnected { es: "Grok conectado", en: "Grok connected" }
    NavGrokDisconnected { es: "Grok sin conectar", en: "Grok not connected" }
    GoTo { es: "Ir a {section}", en: "Go to {section}" }

    // ----------------------------------------------------- settings form
    ErrOffset {
        es: "Escribí la posición en píxeles, por ejemplo 24 o -48",
        en: "Enter the position in pixels, for example 24 or -48",
    }
    ErrEndpointing {
        es: "El silencio de corte va de 0 a 5000 ms",
        en: "The end-of-speech silence goes from 0 to 5000 ms",
    }
    ErrChars {
        es: "Escribí una cantidad de caracteres, por ejemplo 20.000",
        en: "Enter a number of characters, for example 20,000",
    }
    ErrPasteKey { es: "Pegá la clave de API antes de guardarla", en: "Paste the API key before saving it" }
    KeySent { es: "Clave enviada al llavero del sistema", en: "Key sent to the system keychain" }
    ErrTermExists { es: "«{term}» ya está en el vocabulario", en: "“{term}” is already in the vocabulary" }
    ErrCorrectionEmpty {
        es: "Completá cómo se escucha y cómo se escribe",
        en: "Fill in how it sounds and how it is written",
    }
    PhInstructions { es: "Cómo querés que se pula el texto", en: "How you want the text polished" }
    PhKeyterm { es: "Agregar término y Enter", en: "Add a term and press Enter" }
    PhSoundsLike { es: "Se escucha como…", en: "Sounds like…" }
    PhWrittenAs { es: "Se escribe…", en: "Written as…" }
    PhPronounced { es: "Se pronuncia…", en: "Pronounced as…" }
    PhSearch { es: "Buscar en el historial", en: "Search history" }
    PhUnassigned { es: "Sin asignar", en: "Not set" }
    PhWindowClass { es: "Clase de la ventana", en: "Window class" }
    PhPasteToRead {
        es: "Pegá acá el texto que querés escuchar",
        en: "Paste the text you want to hear here",
    }

    // ----------------------------------------------------------- general
    UiLanguage { es: "Idioma de la interfaz", en: "Interface language" }
    UiLanguageAuto {
        es: "Automático sigue el idioma del sistema (ahora, {lang}). Mars voice cambia de idioma al instante.",
        en: "Automatic follows the system language (now {lang}). Mars voice switches language at once.",
    }
    DictationLanguage { es: "Idioma del dictado", en: "Dictation language" }
    DictationLanguageHint {
        es: "Código de idioma. El formato de números necesita un idioma definido.",
        en: "Language code. Number formatting needs a set language.",
    }
    OnFinish { es: "Al terminar de dictar", en: "When you finish dictating" }
    CopyToClipboard { es: "Copiar al portapapeles", en: "Copy to the clipboard" }
    OnlyShowText { es: "Solo mostrar el texto", en: "Only show the text" }
    OnlyShowTextHint {
        es: "Copiás o pegás vos desde el overlay.",
        en: "You copy or paste from the overlay yourself.",
    }
    PasteHintBefore {
        es: "Para pegar, enfocá el campo de destino y usá",
        en: "To paste, focus the target field and use",
    }
    PasteHintAfter {
        es: "o el botón Pegar. Mars voice nunca envía Enter.",
        en: "or the Paste button. Mars voice never sends Enter.",
    }

    // -------------------------------------------------------- appearance
    Colors { es: "Colores", en: "Colors" }
    ColorsHint {
        es: "Se aplica a la ventana y al overlay, con cualquier estilo.",
        en: "Applies to the window and the overlay, with any style.",
    }
    VoiceStyle { es: "Estilo de la voz", en: "Voice style" }
    VoiceStyleHint {
        es: "Cómo se dibuja tu voz mientras dictás. Los controles son los mismos en los tres.",
        en: "How your voice is drawn while you dictate. The controls are the same in all three.",
    }

    // -------------------------------------------------- history and audio
    RetentionOff { es: "No guardar audio", en: "Don't keep audio" }
    Retention7 { es: "Guardar 7 días", en: "Keep 7 days" }
    Retention30 { es: "Guardar 30 días", en: "Keep 30 days" }
    RetentionForever { es: "Hasta que lo borre", en: "Until I delete it" }
    RetentionOffNote {
        es: "Predeterminado. El audio se descarta al terminar.",
        en: "Default. Audio is discarded when you finish.",
    }
    Retention7Note { es: "Se borra solo a los 7 días.", en: "Deleted automatically after 7 days." }
    Retention30Note { es: "Se borra solo a los 30 días.", en: "Deleted automatically after 30 days." }
    RetentionForeverNote {
        es: "Queda hasta que lo borres desde la nota.",
        en: "Kept until you delete it from the note.",
    }
    TranscriptHistory { es: "Historial de transcripciones", en: "Transcription history" }
    KeepTranscriptHistory {
        es: "Guardar el historial de transcripciones",
        en: "Keep a transcription history",
    }
    KeepTranscriptHistoryNote {
        es: "Texto, duración y uso de cada dictado, solo en este equipo.",
        en: "Text, duration and usage of each dictation, only on this device.",
    }
    HistoryOffNote {
        es: "Apagado: los dictados nuevos no se guardan ni suman al uso. Lo ya guardado se conserva.",
        en: "Off: new dictations are not saved and don't count towards usage. What is already saved stays.",
    }
    DictationAudio { es: "Audio de cada dictado", en: "Audio of each dictation" }
    AudioNeedsHistory {
        es: "Guardar audio necesita el historial activado.",
        en: "Keeping audio needs the history turned on.",
    }
    AudioChangesNext {
        es: "Los cambios valen para los próximos dictados. Si lo cambiás mientras grabás, el audio de ese dictado se descarta.",
        en: "Changes apply to the next dictations. If you change it while recording, that dictation's audio is discarded.",
    }
    AudioKeptNote {
        es: "Elegir No guardar audio no borra los audios que ya guardaste ({size}). Borralos desde cada nota o con Borrar historial.",
        en: "Choosing Don't keep audio doesn't delete audio you already kept ({size}). Delete it from each note or with Clear history.",
    }
    ClearHistoryNote {
        es: "Borra todas las notas y sus audios. El uso de Inicio se calcula desde el historial, así que vuelve a cero.",
        en: "Deletes every note and its audio. Usage on Home is computed from the history, so it goes back to zero.",
    }
    ClearHistoryQuestion {
        es: "¿Borrar todo el historial? No se puede deshacer.",
        en: "Clear the whole history? This can't be undone.",
    }
    ClearHistory { es: "Borrar historial", en: "Clear history" }

    // ---------------------------------------------------------- provider
    NoVerifiedCredential { es: "Sin credencial verificada", en: "No verified credential" }
    CredentialNotInvolved {
        es: "La credencial no está involucrada en el último error.",
        en: "The credential is not involved in the last error.",
    }
    HowConnects { es: "Cómo se conecta Mars voice con Grok", en: "How Mars voice connects to Grok" }
    ProviderApiTitle { es: "Clave de API de xAI", en: "xAI API key" }
    ProviderApiDesc {
        es: "La vía oficial de xAI. Se paga por uso en tu cuenta de xAI; los costes que ves acá son estimaciones.",
        en: "The official xAI route. You pay per use on your xAI account; the costs shown here are estimates.",
    }
    ProviderSubTitle {
        es: "Suscripción de Grok — experimental, no oficial",
        en: "Grok subscription — experimental, unofficial",
    }
    ProviderSubDesc {
        es: "Usa tu suscripción con un inicio de sesión no oficial. Puede dejar de funcionar.",
        en: "Uses your subscription through an unofficial sign-in. It may stop working.",
    }
    NeverSwitches {
        es: "Mars voice nunca cambia de modo por su cuenta ni usa la clave de API como respaldo.",
        en: "Mars voice never switches modes on its own or falls back to the API key.",
    }
    CoverageChecked {
        es: "El acceso y la cobertura de voz a texto se comprueban al dictar.",
        en: "Access and speech-to-text coverage are checked when you dictate.",
    }
    Subscription { es: "Suscripción", en: "Subscription" }
    SubscriptionHint {
        es: "Conectá tu cuenta de Grok. Mars voice gestiona el inicio de sesión directamente.",
        en: "Connect your Grok account. Mars voice handles the sign-in itself.",
    }
    ReconnectGrok { es: "Volver a conectar con Grok", en: "Reconnect to Grok" }
    ApiKey { es: "Clave de API", en: "API key" }
    ApiKeyStored {
        es: "Se guarda en el llavero del sistema, nunca en config.toml. El campo se vacía al guardar.",
        en: "Stored in the system keychain, never in config.toml. The field clears when saved.",
    }
    DeleteKey { es: "Borrar clave", en: "Delete key" }
    ConsentTitle {
        es: "Antes de usar tu suscripción de Grok",
        en: "Before you use your Grok subscription",
    }
    ConsentIntro {
        es: "Esta opción es experimental y no oficial. Leé esto antes de seguir:",
        en: "This option is experimental and unofficial. Please read this first:",
    }
    ConsentClient {
        es: "Usa el cliente OAuth público de compatibilidad de Grok CLI. No es una integración oficial de xAI para Mars voice.",
        en: "It uses the Grok CLI's public OAuth compatibility client. It is not an official xAI integration for Mars voice.",
    }
    ConsentMayStop {
        es: "Puede dejar de funcionar o ser bloqueada en cualquier momento.",
        en: "It may stop working or be blocked at any time.",
    }
    ConsentTerms {
        es: "Puede entrar en conflicto con los términos de servicio de xAI.",
        en: "It may conflict with xAI's terms of service.",
    }
    ConsentRisk {
        es: "El riesgo lo asume tu cuenta.",
        en: "Your account bears the risk.",
    }
    ConsentNotAffiliated {
        es: "Mars voice no está afiliado a xAI.",
        en: "Mars voice is not affiliated with xAI.",
    }
    ConsentAccept { es: "Entiendo, usar suscripción", en: "I understand, use subscription" }
    ConsentApiAdvice {
        es: "La clave de API es la vía oficial y recomendada.",
        en: "The API key is the official, recommended route.",
    }
    SignInBrowser { es: "Iniciá sesión en tu navegador", en: "Sign in with your browser" }
    LoginStep1 {
        es: "1. Abrí este enlace. Mars voice no completa el inicio de sesión por vos.",
        en: "1. Open this link. Mars voice doesn't complete the sign-in for you.",
    }
    OpenLink { es: "Abrir enlace", en: "Open link" }
    LinkNotHttps {
        es: "El enlace no es HTTPS; no se abre automáticamente.",
        en: "The link isn't HTTPS, so it won't open automatically.",
    }
    LoginStep2 { es: "2. Escribí este código cuando te lo pida:", en: "2. Enter this code when asked:" }
    CopyCode { es: "Copiar código", en: "Copy code" }
    WaitingConfirmation { es: "Esperando la confirmación…", en: "Waiting for confirmation…" }

    // ----------------------------------------------------------- writing
    Polish { es: "Pulido", en: "Polishing" }
    PolishToggle { es: "Pulir el texto con Grok", en: "Polish the text with Grok" }
    PolishToggleNote {
        es: "Corrige puntuación y estilo después de transcribir.",
        en: "Fixes punctuation and style after transcribing.",
    }
    Model { es: "Modelo", en: "Model" }
    Instructions { es: "Instrucciones", en: "Instructions" }
    InstructionsSave {
        es: "{key} o salir del campo guarda las instrucciones.",
        en: "{key} or leaving the field saves the instructions.",
    }
    Transcription { es: "Transcripción", en: "Transcription" }
    FormatNumbers { es: "Formatear números", en: "Format numbers" }
    FormatNumbersNote {
        es: "Escribe cifras en vez de palabras. Revisá fechas y montos.",
        en: "Writes digits instead of words. Check dates and amounts.",
    }
    KeepFillers { es: "Conservar muletillas", en: "Keep filler words" }
    KeepFillersNote { es: "Mantiene «eh», «este» y similares.", en: "Keeps “uh”, “um” and the like." }
    EndpointingLabel { es: "Silencio de corte (ms)", en: "End-of-speech silence (ms)" }

    // ------------------------------------------- vocabulary, corrections
    TermsCount { es: "Términos ({n}/100)", en: "Terms ({n}/100)" }
    TermsHint {
        es: "Nombres y palabras técnicas que Grok debe reconocer. Hasta 50 caracteres cada uno.",
        en: "Names and technical words Grok should recognize. Up to 50 characters each.",
    }
    CorrectionsIntro {
        es: "Cuando editás un dictado, el overlay propone lo que cambiaste. Nada se aprende sin que lo apruebes.",
        en: "When you edit a dictation, the overlay proposes what you changed. Nothing is learned without your approval.",
    }
    AddManually { es: "Agregar a mano", en: "Add manually" }
    LearnedCount { es: "Aprendidas ({n})", en: "Learned ({n})" }
    NoCorrections {
        es: "Todavía no aprobaste ninguna corrección.",
        en: "You haven't approved any corrections yet.",
    }
    Forget { es: "Olvidar", en: "Forget" }

    // -------------------------------------------------------- microphone
    MicPickHint {
        es: "Elegí una entrada de la lista, o reconectá el micrófono y tocá Actualizar lista. Después volvé a dictar.",
        en: "Pick an input from the list, or reconnect the microphone and press Refresh list. Then dictate again.",
    }
    Input { es: "Entrada", en: "Input" }
    SystemDefault { es: "Predeterminado del sistema", en: "System default" }
    SystemDefaultNote {
        es: "El que usa el sistema en este momento.",
        en: "Whatever the system uses right now.",
    }
    MicMissing { es: "Seleccionado, pero desconectado", en: "Selected, but disconnected" }
    MicMissingNote { es: "{id} · elegí otro o reconectalo", en: "{id} · pick another or reconnect it" }
    MicSavesAudio {
        es: "El micrófono se abre solo mientras dictás. El audio se guarda según Historial y audio.",
        en: "The microphone opens only while you dictate. Audio is kept as set in History & audio.",
    }
    MicNoAudio {
        es: "El micrófono se abre solo mientras dictás y el audio no se guarda.",
        en: "The microphone opens only while you dictate, and audio is not kept.",
    }

    // --------------------------------------------------------- shortcuts
    HowShortcutWorks { es: "Cómo funciona el atajo", en: "How the shortcut works" }
    ModeToggle { es: "Pulsar para empezar y terminar", en: "Press to start and finish" }
    ModeToggleNote {
        es: "Tocás el atajo una vez para empezar y otra vez para terminar.",
        en: "Press the shortcut once to start and again to finish.",
    }
    ModeHold { es: "Mantener presionado para hablar", en: "Hold to talk" }
    ModeHoldNote {
        es: "Grabás mientras mantenés el atajo; al soltarlo termina el dictado.",
        en: "You record while you hold the shortcut; releasing it finishes the dictation.",
    }
    HoldCopy {
        es: "Mantené el atajo y hablá. Al soltarlo termina y el texto se copia al portapapeles.",
        en: "Hold the shortcut and talk. Releasing it finishes, and the text is copied to the clipboard.",
    }
    HoldShow {
        es: "Mantené el atajo y hablá. Al soltarlo termina y el texto queda en el overlay.",
        en: "Hold the shortcut and talk. Releasing it finishes, and the text stays in the overlay.",
    }
    ToggleCopy {
        es: "Pulsalo para empezar y otra vez para terminar. El texto se copia al portapapeles.",
        en: "Press it to start and again to finish. The text is copied to the clipboard.",
    }
    ToggleShow {
        es: "Pulsalo para empezar y otra vez para terminar. El texto queda en el overlay.",
        en: "Press it to start and again to finish. The text stays in the overlay.",
    }
    CopyIsInGeneral {
        es: "Copiar al terminar se elige en General. Los botones del overlay funcionan igual en los dos modos.",
        en: "Copy on finish is set in General. The overlay buttons work the same in both modes.",
    }
    HoldNeedsRelease {
        es: "Mantener necesita que el sistema avise al presionar y al soltar la tecla.",
        en: "Hold needs the system to report both the key press and the release.",
    }
    HyprHoldBinds {
        es: "En Hyprland: son dos binds, uno al presionar y otro al soltar. Mars voice no los instala por su cuenta.",
        en: "On Hyprland: these are two binds, one on press and one on release. Mars voice doesn't install them on its own.",
    }
    Combinations { es: "Combinaciones", en: "Key combinations" }
    StartOrFinish { es: "Empezar o terminar", en: "Start or finish" }
    HoldToTalk { es: "Mantener para hablar", en: "Hold to talk" }
    PasteLast { es: "Pegar el último dictado", en: "Paste the last dictation" }
    PasteKeys { es: "Teclas que envía Pegar", en: "Keys sent by Paste" }
    PasteKeysHint {
        es: "En algunas terminales hace falta CTRL+SHIFT+V. Pegar nunca envía Enter.",
        en: "Some terminals need CTRL+SHIFT+V. Paste never sends Enter.",
    }
    NoDesktopChanges {
        es: "Mars voice no cambia la configuración de tu escritorio por su cuenta.",
        en: "Mars voice never changes your desktop settings on its own.",
    }
    HyprShortcutBinds {
        es: "En Hyprland: los atajos globales son binds del compositor. Si cambiás estas combinaciones, actualizá también tus binds.",
        en: "On Hyprland: global shortcuts are compositor binds. If you change these combinations, update your binds too.",
    }

    // ----------------------------------------------------------- overlay
    Visibility { es: "Visibilidad", en: "Visibility" }
    ShowIdle { es: "Mostrar en reposo", en: "Show when idle" }
    ShowIdleNote { es: "Si lo apagás, aparece solo al dictar.", en: "If off, it appears only while dictating." }
    ShowOverlayNow { es: "Mostrar overlay ahora", en: "Show overlay now" }
    Position { es: "Posición", en: "Position" }
    OverlayDragHint {
        es: "El overlay es una ventana flotante: arrastralo desde la barra superior o, en reposo, desde la tarjeta. Se queda donde lo dejes mientras pasa de grabar a listo.",
        en: "The overlay is a floating window: drag it by its top bar or, when idle, by the card. It stays where you leave it from recording to ready.",
    }
    OverlayOffsetsHint {
        es: "Valores positivos se miden desde arriba e izquierda; negativos, desde abajo y derecha. Al editarlos deja de estar centrado. Al volver a abrirse, el overlay usa esta posición, no la arrastrada.",
        en: "Positive values count from the top and left; negative ones from the bottom and right. Editing them turns centering off. When it opens again, the overlay uses this position, not the dragged one.",
    }
    DesktopMayPlace {
        es: "Algunos escritorios deciden por su cuenta dónde se abre una ventana flotante; ahí la posición puede ser aproximada.",
        en: "Some desktops decide where a floating window opens; there the position may be approximate.",
    }
    HyprOverlayRules {
        es: "En Hyprland: centrarlo y quitarle borde, sombra y desenfoque depende de reglas para la clase {class}. Mars voice no las instala por su cuenta.",
        en: "On Hyprland: centering it and removing its border, shadow and blur depends on rules for the {class} class. Mars voice doesn't install them on its own.",
    }

    // -------------------------------------------------------- background
    Windows { es: "Ventanas", en: "Windows" }
    BackgroundHint {
        es: "Cerrar esta ventana deja Mars voice funcionando. Volvés desde la bandeja o con el atajo.",
        en: "Closing this window leaves Mars voice running. Come back from the tray or with the shortcut.",
    }
    CloseSettings { es: "Cerrar ajustes", en: "Close settings" }
    QuitApp { es: "Salir de Mars voice", en: "Quit Mars voice" }

    // ----------------------------------------------------- Ajustes → Lectura
    SourceCopy {
        es: "Copiar la selección automáticamente",
        en: "Copy the selection automatically",
    }
    SourceSelectionClipboard {
        es: "Selección primaria; si está vacía, portapapeles",
        en: "Primary selection, else the clipboard",
    }
    SourceSelection { es: "Solo la selección primaria", en: "Primary selection only" }
    SourceClipboard { es: "Solo el portapapeles", en: "Clipboard only" }
    SourceCopyDetail {
        es: "Al pulsar el atajo, Mars voice copia lo seleccionado en la ventana activa ({keys}), lo lee y devuelve tu portapapeles. Funciona aunque el sistema no tenga selección primaria.",
        en: "When you press the shortcut, Mars voice copies the selection in the active window ({keys}), reads it and restores your clipboard. Works even without a primary selection.",
    }
    LinuxCopyKeys { es: "Ctrl+C; en terminales, Ctrl+Shift+C", en: "Ctrl+C; in terminals, Ctrl+Shift+C" }
    SourceSelectionClipboardDetail {
        es: "Necesita la selección primaria de Linux. Sin ella lee lo último que copiaste.",
        en: "Needs the Linux primary selection. Without it, reads what you last copied.",
    }
    SourceSelectionDetail {
        es: "Necesita la selección primaria de Linux. Sin ella no encuentra texto.",
        en: "Needs the Linux primary selection. Without it, finds no text.",
    }
    HyprPrimarySelection {
        es: "En Hyprland: la selección primaria se activa con misc:middle_click_paste.",
        en: "On Hyprland: turn the primary selection on with misc:middle_click_paste.",
    }
    SourceClipboardDetail {
        es: "Lee lo último que copiaste: copiá y después pulsá el atajo.",
        en: "Reads what you last copied: copy, then press the shortcut.",
    }
    ErrTerminalEmpty {
        es: "Escribí la clase de la ventana, por ejemplo org.wezfurlong.wezterm",
        en: "Enter the window class, for example org.wezfurlong.wezterm",
    }
    ErrTerminalListed { es: "«{class}» ya está en la lista", en: "“{class}” is already listed" }
    LookaheadWindow {
        es: "Solo lo próximo (ahorra si detenés)",
        en: "Only what's next (saves if you stop)",
    }
    LookaheadAll { es: "Todo el texto de una vez", en: "The whole text at once" }
    LookaheadWindowDetail {
        es: "Predeterminado. Si detenés a la mitad, no se envía el resto.",
        en: "Default. If you stop halfway, the rest is never sent.",
    }
    LookaheadAllDetail {
        es: "Envía todo el texto al empezar, aunque después detengas.",
        en: "Sends the whole text at the start, even if you stop later.",
    }
    ErrNothingToRead {
        es: "Pegá o escribí el texto que querés escuchar",
        en: "Paste or type the text you want to hear",
    }
    TextSentToReader {
        es: "Texto enviado a la ventana de lectura",
        en: "Text sent to the reader window",
    }
    ErrPronunciationEmpty {
        es: "Completá cómo se escribe y cómo se pronuncia",
        en: "Fill in how it's written and how it's pronounced",
    }
    ReadNow { es: "Leer ahora", en: "Read now" }
    ReadNowHint {
        es: "lee lo que selecciones en cualquier app; no hace falta copiarlo.",
        en: "reads what you select in any app; no need to copy it.",
    }
    PasteTextToRead { es: "Pegar texto para leer", en: "Paste text to read" }
    ReadNowDemo {
        es: "DEMO: la lectura usa audio sintético local, sin tu cuenta ni la selección real.",
        en: "DEMO: reading uses local synthetic audio, without your account or the real selection.",
    }
    ReadNowPrivacy {
        es: "Mars voice toma el texto solo al pulsar el atajo o estos botones, nunca en segundo plano. Leer envía el texto a Grok y consume tu cuenta.",
        en: "Mars voice takes the text only when you press the shortcut or these buttons, never in the background. Reading sends the text to Grok and uses your account.",
    }
    Activation { es: "Activación", en: "Activation" }
    Shortcuts { es: "Atajos", en: "Shortcuts" }
    ReadSelection { es: "Leer la selección", en: "Read the selection" }
    PauseResume { es: "Pausar o reanudar", en: "Pause or resume" }
    ReadShortcutsHint {
        es: "Pausar y detener no tienen atajo por defecto. Mars voice no cambia la configuración de tu escritorio por su cuenta: si editás estos campos, actualizá también tus atajos globales.",
        en: "Pause and stop have no shortcut by default. Mars voice never changes your desktop settings on its own: if you edit these fields, update your global shortcuts too.",
    }
    HyprReadShortcuts {
        es: "En Hyprland: los atajos viven en ~/.config/hypr/config/dicta.lua (dicta --read-selection).",
        en: "On Hyprland: the shortcuts live in ~/.config/hypr/config/dicta.lua (dicta --read-selection).",
    }
    WhatShortcutReads { es: "Qué lee el atajo", en: "What the shortcut reads" }
    IfReading { es: "Si ya hay una lectura", en: "If a reading is already playing" }
    SameText { es: "Con el mismo texto", en: "With the same text" }
    NewText { es: "Con un texto nuevo", en: "With a new text" }
    SameTextRestart { es: "Empezar de nuevo", en: "Start over" }
    NewTextReplace { es: "Reemplazar la lectura", en: "Replace the reading" }
    NewTextQueue { es: "Agregar a la cola", en: "Add to the queue" }
    Limits { es: "Límites", en: "Limits" }
    CharsPerReading { es: "Caracteres por lectura", en: "Characters per reading" }
    ConfirmFrom { es: "Confirmar desde", en: "Confirm from" }
    LimitsHint {
        es: "Lo que supere el límite no se lee y la ventana lo avisa. La confirmación muestra los caracteres y el coste estimado antes de enviar; 0 nunca pregunta.",
        en: "Anything over the limit isn't read, and the window says so. The confirmation shows the characters and estimated cost before sending; 0 never asks.",
    }
    RestoreClipboard { es: "Restaurar el portapapeles", en: "Restore the clipboard" }
    RestoreClipboardNote {
        es: "Si lo apagás, el texto leído queda copiado.",
        en: "If off, the text read stays copied.",
    }
    ExtraTerminals {
        es: "Terminales extra (usan Ctrl+Shift+C) · {n}/50",
        en: "Extra terminals (use Ctrl+Shift+C) · {n}/50",
    }
    TerminalsHint {
        es: "Las terminales comunes (Ghostty, Kitty, Alacritty, foot, WezTerm, Konsole, GNOME Terminal…) ya se reconocen. Agregá la clase de otra ventana que copie con Ctrl+Shift+C, por ejemplo org.wezfurlong.wezterm.",
        en: "Common terminals (Ghostty, Kitty, Alacritty, foot, WezTerm, Konsole, GNOME Terminal…) are already recognized. Add the class of any other window that copies with Ctrl+Shift+C, for example org.wezfurlong.wezterm.",
    }
    HyprWindowClass {
        es: "En Hyprland: la clase se ve con hyprctl activewindow.",
        en: "On Hyprland: see the class with hyprctl activewindow.",
    }
    ClipboardHistoryWarning {
        es: "El texto pasa un instante por el portapapeles: si usás un gestor de historial del portapapeles, puede quedar guardado ahí.",
        en: "The text passes through the clipboard for a moment, so a clipboard history manager may keep it.",
    }
    CredentialForReading { es: "Credencial disponible para leer", en: "Credential available for reading" }
    NoCredentialForMode { es: "Sin credencial para este modo", en: "No credential for this mode" }
    Account { es: "Cuenta", en: "Account" }
    ReadingSubDesc {
        es: "El mismo inicio de sesión que el dictado. Experimental y no oficial.",
        en: "Same sign-in as dictation. Experimental and unofficial.",
    }
    ReadingOwnMode {
        es: "Es un modo propio de la lectura con las mismas credenciales que el dictado. Mars voice nunca cambia de modo por su cuenta ni usa la clave como respaldo.",
        en: "Read aloud has its own mode, with the same credentials as dictation. Mars voice never switches modes on its own or falls back to the key.",
    }
    ReadingCoverage {
        es: "Tener credencial no confirma que la suscripción cubra la lectura en voz alta. Si la rechaza, el error te indica cómo pasar a la clave de API.",
        en: "Having a credential doesn't confirm the subscription covers read aloud. If it's refused, the error tells you how to switch to the API key.",
    }
    AlsoDisconnectsDictation {
        es: "También desconecta el dictado: comparten la credencial.",
        en: "This also disconnects dictation: they share the credential.",
    }
    GoToProvider { es: "Ir a Proveedor", en: "Go to Provider" }
    ToDisconnectCredential { es: "Para desconectar esta credencial.", en: "To disconnect this credential." }
    VoiceCustom { es: "{name} · propia", en: "{name} · custom" }
    VoiceUnavailable { es: "{name} · no disponible", en: "{name} · unavailable" }
    Voice { es: "Voz", en: "Voice" }
    StopSample { es: "Detener muestra", en: "Stop sample" }
    TryVoice { es: "Probar voz", en: "Try voice" }
    TryVoiceDemo {
        es: "DEMO: la muestra es sintética y local, no usa tu cuenta.",
        en: "DEMO: the sample is synthetic and local; it doesn't use your account.",
    }
    TryVoiceHint {
        es: "Probar voz genera una muestra corta con Grok y consume tu cuenta. La voz vale para las próximas lecturas; en la ventana de lectura podés regenerar la actual con otra.",
        en: "Try voice generates a short sample with Grok and uses your account. The voice applies to the next readings; in the reader window you can regenerate the current one with another.",
    }
    Language { es: "Idioma", en: "Language" }
    TtsLanguageHint {
        es: "Automático o un código BCP-47, por ejemplo es-AR. Las voces hablan todos los idiomas documentados.",
        en: "Automatic or a BCP-47 code, for example en-US. The voices speak every documented language.",
    }
    Style { es: "Estilo", en: "Style" }
    StyleNormal { es: "Normal", en: "Normal" }
    StyleSoft { es: "Suave", en: "Soft" }
    StyleWhisper { es: "Susurro", en: "Whisper" }
    StyleSlow { es: "Lento", en: "Slow" }
    StyleFast { es: "Rápido", en: "Fast" }
    StyleSlowSoft { es: "Lento y suave", en: "Slow and soft" }
    StyleTagsHint {
        es: "Mars voice agrega las etiquetas de estilo; el texto que leés no puede elegirlas.",
        en: "Mars voice adds the style tags; the text being read can't choose them.",
    }
    Generation { es: "Generación", en: "Generation" }
    GenerationSpeed { es: "Velocidad de generación", en: "Generation speed" }
    GenerationSpeedHint {
        es: "De 0,70 a 1,50. Cambia el audio, así que hace falta regenerar; es distinta de la velocidad de reproducción.",
        en: "From 0.70 to 1.50. It changes the audio, so it needs regenerating; it's not the playback speed.",
    }
    AudioQuality { es: "Calidad de audio", en: "Audio quality" }
    LowLatency { es: "Menor latencia", en: "Lower latency" }
    LowLatencyNote { es: "Predeterminado. Empieza a sonar antes.", en: "Default. Starts playing sooner." }
    BetterQuality { es: "Mejor calidad", en: "Better quality" }
    BetterQualityNote { es: "Tarda un poco más en empezar.", en: "Takes a little longer to start." }
    Normalize {
        es: "Normalizar números y abreviaturas en el servidor",
        en: "Normalize numbers and abbreviations on the server",
    }
    NormalizeNote { es: "Apagado por defecto, como en la API de xAI.", en: "Off by default, as in the xAI API." }
    Playback { es: "Reproducción", en: "Playback" }
    InitialSpeed { es: "Velocidad inicial", en: "Initial speed" }
    RememberSpeed { es: "Recordar la última velocidad", en: "Remember the last speed" }
    RememberSpeedNote {
        es: "La que elijas en la ventana de lectura pasa a ser la inicial.",
        en: "The one you pick in the reader window becomes the initial one.",
    }
    PlaybackSpeedHint {
        es: "De 0,5× a 2,5×. Se aplica al instante y conserva el tono, sin volver a generar.",
        en: "From 0.5× to 2.5×. Applies instantly and keeps the pitch, without regenerating.",
    }
    Volume { es: "Volumen", en: "Volume" }
    AudioOutput { es: "Salida de audio", en: "Audio output" }
    SystemDefaultOutput { es: "Predeterminada del sistema", en: "System default" }
    OutputMissing { es: "Elegida, pero desconectada", en: "Selected, but disconnected" }
    OutputMissingNote { es: "{id} · elegí otra o reconectala", en: "{id} · pick another or reconnect it" }
    WhileGenerating { es: "Al generar", en: "While generating" }
    Autoplay { es: "Reproducir apenas llega el audio", en: "Play as soon as audio arrives" }
    AutoplayNote {
        es: "No espera a que se genere todo el texto.",
        en: "Doesn't wait for the whole text to be generated.",
    }
    Ahead { es: "Por adelantado", en: "Ahead" }
    Controls { es: "Controles", en: "Controls" }
    SkipButtons { es: "Salto de los botones ±", en: "Skip of the ± buttons" }
    OnDictationStart { es: "Al empezar un dictado", en: "When a dictation starts" }
    OnDictationPause { es: "Pausar la lectura", en: "Pause the reading" }
    OnDictationStop { es: "Detener la lectura", en: "Stop the reading" }
    NeverOpensMic {
        es: "La lectura nunca abre el micrófono. Pausarla al dictar evita que el dictado capte la voz; al terminar no se reanuda sola.",
        en: "Read aloud never opens the microphone. Pausing it while dictating keeps its voice out of the dictation; it doesn't resume by itself afterwards.",
    }
    JoinHyphenated {
        es: "Unir palabras cortadas con guion al final de línea",
        en: "Join words hyphenated at line ends",
    }
    JoinLines { es: "Unir líneas partidas", en: "Join broken lines" }
    JoinLinesNote {
        es: "Útil al copiar de PDF o de una terminal.",
        en: "Useful when copying from a PDF or a terminal.",
    }
    SkipLinks { es: "Omitir enlaces", en: "Skip links" }
    SkipLinksNote { es: "Se leen como «enlace».", en: "Read as “link”." }
    SkipCode { es: "Omitir bloques de código", en: "Skip code blocks" }
    SkipCodeNote { es: "Se avisa que hay un bloque omitido.", en: "Says that a block was skipped." }
    ReadEmoji { es: "Leer emojis", en: "Read emojis" }
    InterpretTags { es: "Interpretar etiquetas de voz del texto", en: "Interpret voice tags in the text" }
    InterpretTagsNote {
        es: "Apagado por defecto: así una página no puede meter [laugh] o <whisper>.",
        en: "Off by default, so a page can't sneak in [laugh] or <whisper>.",
    }
    TextAndPronunciation { es: "Texto y pronunciación", en: "Text and pronunciation" }
    PronunciationCount { es: "Pronunciación ({n}/{max})", en: "Pronunciation ({n}/{max})" }
    PronunciationHint {
        es: "Cambia cómo suena, no el texto que ves. Acepta IPA. Es independiente del vocabulario del dictado.",
        en: "Changes how it sounds, not the text you see. Accepts IPA. Separate from the dictation vocabulary.",
    }
    Window { es: "Ventana", en: "Window" }
    ReaderDragHint {
        es: "Arrastrala desde la barra superior. Se queda donde la dejes mientras lee; al volver a abrirse usa esta posición.",
        en: "Drag it by its top bar. It stays where you leave it while reading; when it opens again it uses this position.",
    }
    ReaderOffsetsHint {
        es: "Positivos desde arriba e izquierda; negativos, desde abajo y derecha. Al editarlos deja de estar centrada.",
        en: "Positive from the top and left; negative from the bottom and right. Editing them turns centering off.",
    }
    CompactMode { es: "Modo compacto", en: "Compact mode" }
    CompactModeNote {
        es: "Solo la barra y los controles, sin el texto.",
        en: "Only the bar and the controls, without the text.",
    }
    CloseWhenDone { es: "Cerrar al terminar", en: "Close when finished" }
    Never { es: "Nunca", en: "Never" }
    AfterSeconds { es: "Tras {s} s", en: "After {s} s" }
    HyprReaderRules {
        es: "En Hyprland: centrarla, abrirla sin quitar el foco y sin borde depende de reglas para la clase {class} en ~/.config/hypr/config/dicta.lua. Mars voice no las instala por su cuenta.",
        en: "On Hyprland: centering it, opening it without stealing focus and without a border depends on rules for the {class} class in ~/.config/hypr/config/dicta.lua. Mars voice doesn't install them on its own.",
    }
    ReadingRetentionOffNote {
        es: "Predeterminado. El audio vive en memoria mientras la ventana está abierta.",
        en: "Default. Audio lives in memory while the window is open.",
    }
    ReadingRetentionForeverNote {
        es: "Queda hasta que lo borres desde la lectura.",
        en: "Kept until you delete it from the reading.",
    }
    HistoryAndCost { es: "Historial y coste", en: "History and cost" }
    KeepReadings { es: "Guardar las lecturas en el historial", en: "Keep readings in the history" }
    KeepReadingsNote {
        es: "El texto, la voz y los caracteres enviados, solo en este equipo.",
        en: "The text, voice and characters sent, only on this device.",
    }
    ReadingAudio { es: "Audio de cada lectura", en: "Audio of each reading" }
    ReadingAudioNeedsHistory {
        es: "Guardar audio necesita el historial de lecturas activado.",
        en: "Keeping audio needs the readings history turned on.",
    }
    ShowCost { es: "Mostrar el coste estimado", en: "Show the estimated cost" }
    ShowCostNote { es: "En la ventana de lectura y en Inicio.", en: "In the reader window and on Home." }
    TtsPriceHint {
        es: "Precio público de xAI: US$ 15 por millón de caracteres enviados ({chars} caracteres ≈ {cost}). Regenerar y probar voces vuelven a contar. Es una estimación, no un cobro ni el saldo de tu suscripción.",
        en: "xAI public price: US$15 per million characters sent ({chars} characters ≈ {cost}). Regenerating and trying voices count again. It's an estimate, not a charge or your subscription balance.",
    }

    // ------------------------------------------------------ reader window
    VoiceFallback { es: "Voz", en: "Voice" }
    TakingCopyRestore {
        es: "Mars voice copia lo seleccionado una sola vez y después devuelve tu portapapeles.",
        en: "Mars voice copies the selection once, then restores your clipboard.",
    }
    TakingCopyKeep {
        es: "Mars voice copia lo seleccionado una sola vez; el texto queda en tu portapapeles.",
        en: "Mars voice copies the selection once; the text stays on your clipboard.",
    }
    TakingClipboard { es: "Mars voice toma tu portapapeles una sola vez.", en: "Mars voice takes your clipboard once." }
    TakingSelection {
        es: "Mars voice toma el texto seleccionado una sola vez, en este instante.",
        en: "Mars voice takes the selected text once, right now.",
    }
    SaveDialogFailed { es: "No se pudo abrir el diálogo para guardar.", en: "Couldn't open the save dialog." }
    LinuxSaveDialogPortal {
        es: "Revisá que xdg-desktop-portal esté instalado.",
        en: "Check that xdg-desktop-portal is installed.",
    }
    WavFileName { es: "lectura.wav", en: "reading.wav" }
    Queued { es: "{n} en cola", en: "{n} queued" }
    GeneratingWithGrok { es: "Generando el audio con Grok…", en: "Generating the audio with Grok…" }
    PlaybackCaughtUp {
        es: "La reproducción alcanzó a la generación; sigue sola.",
        en: "Playback caught up with generation; it will continue on its own.",
    }
    PickedVoice {
        es: "Elegiste {voice}: Regenerar sigue desde la frase actual.",
        en: "You picked {voice}: Regenerate continues from the current sentence.",
    }
    VersionsCount {
        es: "{n} versiones de esta lectura: cambiá sin costo desde la voz.",
        en: "{n} versions of this reading: switch for free from the voice menu.",
    }
    ReaderDemo { es: "DEMO: audio sintético local, sin tu cuenta.", en: "DEMO: local synthetic audio, without your account." }
    CharsSentToGrok { es: "{n} caracteres enviados a Grok", en: "{n} characters sent to Grok" }
    SaveAudio { es: "Guardar audio", en: "Save audio" }
    Regenerate { es: "Regenerar", en: "Regenerate" }
    NothingToRead { es: "Nada para leer", en: "Nothing to read" }
    SelectAndPress { es: "Seleccioná texto y pulsá {key}.", en: "Select text and press {key}." }
    OverLimit {
        es: "Supera el límite por lectura: se leen los primeros {n} caracteres.",
        en: "Over the per-reading limit: only the first {n} characters are read.",
    }
    ConfirmDemo {
        es: "DEMO: el audio es sintético y local; no usa tu cuenta.",
        en: "DEMO: the audio is synthetic and local; it doesn't use your account.",
    }
    ConfirmCost {
        es: "Leer envía el texto a Grok y consume tu cuenta. El coste es una estimación, no un cobro.",
        en: "Reading sends the text to Grok and uses your account. The cost is an estimate, not a charge.",
    }
    ReadingFailed { es: "La lectura no se pudo completar.", en: "The reading couldn't be completed." }
    ReadingSettings { es: "Ajustes de lectura", en: "Read-aloud settings" }
    Custom { es: "propia", en: "custom" }
    VersionsOfReading { es: "Versiones de esta lectura", en: "Versions of this reading" }
    VersionSwitchFree { es: "Cambiar de versión es gratis.", en: "Switching versions is free." }
    VoiceToRegenerate { es: "Voz para regenerar", en: "Voice to regenerate" }
    VoiceMenuHint {
        es: "Elegí una voz y tocá Regenerar: sigue desde la frase actual y consume tu cuenta.",
        en: "Pick a voice and press Regenerate: it continues from the current sentence and uses your account.",
    }
    Speed { es: "Velocidad", en: "Speed" }
    NoTextToShow { es: "Sin texto para mostrar.", en: "No text to show." }
    FollowReading { es: "Seguir la lectura", en: "Follow the reading" }

    // -------------------------------------------------------------- home
    PeriodToday { es: "Hoy", en: "Today" }
    PeriodWeek { es: "7 días", en: "7 days" }
    PeriodAll { es: "Todo", en: "All" }
    SttCostLabel { es: "Equivalente STT estimado", en: "Estimated STT equivalent" }
    SttCostNote {
        es: "A US$ 0,20 por hora de audio enviado. No es un cobro ni el saldo de tu suscripción.",
        en: "At US$0.20 per hour of audio sent. Not a charge or your subscription balance.",
    }
    TtsCostLabel { es: "Equivalente TTS estimado", en: "Estimated TTS equivalent" }
    TtsCostNote {
        es: "A US$ 15 por millón de caracteres enviados. No es un cobro ni el saldo de tu suscripción.",
        en: "At US$15 per million characters sent. Not a charge or your subscription balance.",
    }
    TokenPair { es: "{input} entrada · {output} salida", en: "{input} in · {output} out" }
    HeroSimulated {
        es: "para dictar · Grok simulado, no es una cuenta real",
        en: "to dictate · simulated Grok, not a real account",
    }
    HeroConnected { es: "para dictar · Grok conectado", en: "to dictate · Grok connected" }
    HeroNotConnected {
        es: "para dictar · falta conectar tu cuenta",
        en: "to dictate · your account isn't connected yet",
    }
    Dictations { es: "Dictados", en: "Dictations" }
    DictationsNote { es: "Incluye los que terminaron con error.", en: "Includes those that ended with an error." }
    Words { es: "Palabras", en: "Words" }
    TimeDictated { es: "Tiempo dictado", en: "Time dictated" }
    TimeDictatedNote { es: "Audio capturado del micrófono.", en: "Audio captured from the microphone." }
    AudioSent { es: "Audio enviado", en: "Audio sent" }
    AudioSentNote {
        es: "Lo que se envió a Grok para transcribir.",
        en: "What was sent to Grok to transcribe.",
    }
    PolishTokens { es: "Tokens de pulido", en: "Polishing tokens" }
    NoTokensReported {
        es: "Grok no informó tokens para este período.",
        en: "Grok reported no tokens for this period.",
    }
    TokensReported {
        es: "Entrada · salida, informados por Grok al pulir.",
        en: "Input · output, as reported by Grok when polishing.",
    }
    Usage { es: "Uso", en: "Usage" }
    AudioKept { es: "Audio guardado: {size}", en: "Audio kept: {size}" }
    UsageFromHistory {
        es: "Calculado desde tu historial de transcripciones. Si borrás notas, dejan de contar.",
        en: "Computed from your transcription history. Deleted notes stop counting.",
    }
    Readings { es: "Lecturas", en: "Readings" }
    ReadingsNote { es: "Incluye las que terminaron con error.", en: "Includes those that ended with an error." }
    CharsSent { es: "Caracteres enviados", en: "Characters sent" }
    CharsSentNote { es: "Incluye las regeneraciones.", en: "Includes regenerations." }
    AudioGenerated { es: "Audio generado", en: "Audio generated" }
    ReadAloud { es: "Lectura en voz alta", en: "Read aloud" }
    ReadAloudPeriod {
        es: "lee lo que selecciones · mismo período: {period}",
        en: "reads what you select · same period: {period}",
    }
    ReadingUsageFromHistory {
        es: "Calculado desde tu historial de lecturas. Las muestras de voz no se guardan ni cuentan acá.",
        en: "Computed from your readings history. Voice samples are neither kept nor counted here.",
    }
    RecentNotes { es: "Notas recientes", en: "Recent notes" }
    HistoryOffHome {
        es: "El historial de transcripciones está apagado: los dictados nuevos no se guardan.",
        en: "The transcription history is off: new dictations aren't saved.",
    }
    TurnHistoryOn { es: "Activar historial", en: "Turn history on" }
    NoNotesYet {
        es: "Todavía no hay notas. Cada dictado que termines aparece acá; el historial empieza con esta versión, los dictados anteriores no se guardaron.",
        en: "No notes yet. Every dictation you finish shows up here; the history starts with this version, so earlier dictations weren't kept.",
    }
    SeeHistory { es: "Ver historial", en: "See history" }

    // ----------------------------------------------------------- history
    ProviderSubscription { es: "Suscripción de Grok", en: "Grok subscription" }
    ProviderApi { es: "Clave de API de xAI", en: "xAI API key" }
    HistoryEmpty {
        es: "Tu historial de transcripciones está vacío. Cada dictado que termines aparece acá.",
        en: "Your transcription history is empty. Every dictation you finish shows up here.",
    }
    HistoryOff {
        es: "El historial de transcripciones está apagado. Activalo en Historial y audio para guardar los próximos dictados.",
        en: "The transcription history is off. Turn it on in History & audio to keep the next dictations.",
    }
    NoNoteMatches { es: "Ninguna nota contiene «{q}».", en: "No note contains “{q}”." }
    ReadingsEmpty {
        es: "Todavía no hay lecturas. Seleccioná texto en cualquier app y pulsá {key}, o pegalo en Lectura.",
        en: "No readings yet. Select text in any app and press {key}, or paste it in Read aloud.",
    }
    ReadingsOff {
        es: "El historial de lecturas está apagado. Activalo en Lectura para guardar las próximas.",
        en: "The readings history is off. Turn it on in Read aloud to keep the next ones.",
    }
    NoReadingMatches { es: "Ninguna lectura contiene «{q}».", en: "No reading contains “{q}”." }
    ReadingMetaChars { es: "{voice} · {n} car.", en: "{voice} · {n} chars" }
    NotesOne { es: "1 nota", en: "1 note" }
    NotesMany { es: "{n} notas", en: "{n} notes" }
    NotesMore { es: "{n}+ notas", en: "{n}+ notes" }
    ReadingsOne { es: "1 lectura", en: "1 reading" }
    ReadingsMany { es: "{n} lecturas", en: "{n} readings" }
    ReadingsMore { es: "{n}+ lecturas", en: "{n}+ readings" }
    DeleteAll { es: "Borrar todo", en: "Delete all" }
    DeleteAllReadingsQuestion {
        es: "¿Borrar todas las lecturas y sus audios?",
        en: "Delete every reading and its audio?",
    }
    DeleteAllNotesQuestion {
        es: "¿Borrar todo el historial y sus audios?",
        en: "Delete the whole history and its audio?",
    }
    PickNoteWhenAny {
        es: "Cuando haya notas, elegí una para leerla, copiarla o escuchar su audio.",
        en: "When there are notes, pick one to read, copy or play its audio.",
    }
    PickNote { es: "Elegí una nota de la lista.", en: "Pick a note from the list." }
    DeleteNoteQuestion {
        es: "¿Borrar esta nota y su audio? No se puede deshacer.",
        en: "Delete this note and its audio? This can't be undone.",
    }
    DeleteNote { es: "Borrar nota", en: "Delete note" }
    ReadNoteAloud { es: "Leer en voz alta", en: "Read aloud" }
    DeleteAudioQuestion {
        es: "¿Borrar el audio? El texto se conserva.",
        en: "Delete the audio? The text stays.",
    }
    PlayingSuffix { es: " · reproduciendo", en: " · playing" }
    NoNoteAudio { es: "Sin audio guardado para esta nota.", en: "No audio kept for this note." }
    KeepNextNotesAudio { es: "Guardar audio de los próximos", en: "Keep audio of the next ones" }
    Notes { es: "Notas", en: "Notes" }
    MetaWords { es: "Palabras", en: "Words" }
    MetaDictated { es: "Dictado", en: "Dictated" }
    MetaSent { es: "Enviado", en: "Sent" }
    MetaProvider { es: "Proveedor", en: "Provider" }
    DictationEndedError { es: "El dictado terminó con error", en: "The dictation ended with an error" }
    OriginalTranscript { es: "Transcripción original de Grok", en: "Grok's original transcription" }
    PickReadingWhenAny {
        es: "Cuando haya lecturas, elegí una para releerla, copiarla o escucharla.",
        en: "When there are readings, pick one to read again, copy or play.",
    }
    PickReading { es: "Elegí una lectura de la lista.", en: "Pick a reading from the list." }
    DeleteReadingQuestion {
        es: "¿Borrar esta lectura y su audio? No se puede deshacer.",
        en: "Delete this reading and its audio? This can't be undone.",
    }
    DeleteReading { es: "Borrar lectura", en: "Delete reading" }
    ReplaySaved {
        es: "Escuchar usa el audio guardado: no consume tu cuenta.",
        en: "Play uses the kept audio, so it doesn't use your account.",
    }
    ReplayDemo {
        es: "DEMO: Escuchar genera audio sintético local, sin tu cuenta.",
        en: "DEMO: Play generates local synthetic audio, without your account.",
    }
    ReplayRegenerates {
        es: "No hay audio guardado: Escuchar vuelve a generar la lectura con Grok y consume tu cuenta.",
        en: "No audio kept: Play generates the reading again with Grok and uses your account.",
    }
    SavedAudioMeta { es: "Guardado · {clock} · {size}", en: "Kept · {clock} · {size}" }
    NoReadingAudio { es: "Sin audio guardado para esta lectura.", en: "No audio kept for this reading." }
    KeepNextReadingsAudio { es: "Guardar audio de las próximas", en: "Keep audio of the next ones" }
    MetaCharacters { es: "Caracteres", en: "Characters" }
    MetaSentChars { es: "Enviados", en: "Sent" }
    MetaAccount { es: "Cuenta", en: "Account" }
    MetaEstimatedCost { es: "Coste estimado", en: "Estimated cost" }
    ReadingEndedError { es: "La lectura terminó con error", en: "The reading ended with an error" }
    LoadMore { es: "Cargar más", en: "Load more" }
    ReadingPosition { es: "Lectura {i} de {n}", en: "Reading {i} of {n}" }
    ReadingOutsideSearch {
        es: "Lectura abierta, fuera de la búsqueda",
        en: "Open reading, outside the search",
    }
    NotePosition { es: "Nota {i} de {n}", en: "Note {i} of {n}" }
    NoteOutsideSearch { es: "Nota abierta, fuera de la búsqueda", en: "Open note, outside the search" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_message_exists_in_both_languages_with_the_same_placeholders() {
        assert!(Msg::ALL.len() > 400, "catalogue: {}", Msg::ALL.len());
        for &m in Msg::ALL {
            for lang in Lang::ALL {
                let text = lang.t(m);
                assert!(!text.trim().is_empty(), "{} is empty in {lang:?}", m.id());
                assert_eq!(text, text.trim_end(), "{} ends with spaces", m.id());
            }
            let (mut es, mut en) = (placeholders(m.es()), placeholders(m.en()));
            es.sort_unstable();
            en.sort_unstable();
            assert_eq!(es, en, "{} names different placeholders", m.id());
        }
    }

    #[test]
    fn long_texts_are_actually_translated() {
        // Short labels may coincide ("General", "Audio", "Overlay"); a long
        // sentence identical in both languages was left untranslated.
        for &m in Msg::ALL {
            if m.es().chars().count() > 18 {
                assert_ne!(m.es(), m.en(), "{} is not translated", m.id());
            }
        }
    }

    #[test]
    fn desktop_specific_copy_lives_only_in_its_hints() {
        // Platform-neutral copy: Hyprland, its tools and Linux-only pieces
        // appear only in messages shown for that session.
        for &m in Msg::ALL {
            let id = m.id();
            let scoped = id.starts_with("Hypr") || id.starts_with("Linux");
            for lang in Lang::ALL {
                let text = lang.t(m);
                for word in [
                    "Hyprland",
                    "hyprctl",
                    "middle_click_paste",
                    "dicta.lua",
                    "xdg-desktop-portal",
                ] {
                    assert!(scoped || !text.contains(word), "{id} mentions {word}");
                }
                assert!(!text.contains("PipeWire"), "{id} mentions PipeWire");
            }
            if id.starts_with("Hypr") {
                assert!(m.es().starts_with("En Hyprland:"), "{id}");
                assert!(m.en().starts_with("On Hyprland:"), "{id}");
            }
        }
    }

    #[test]
    fn the_subscription_is_labelled_experimental_and_unofficial() {
        assert_eq!(
            Msg::ProviderSubTitle.es(),
            "Suscripción de Grok — experimental, no oficial"
        );
        let en = Msg::ProviderSubTitle.en().to_lowercase();
        assert!(en.contains("experimental") && en.contains("unofficial"));
        // The consent names every risk in both languages.
        for lang in Lang::ALL {
            let all = [
                Msg::ConsentClient,
                Msg::ConsentMayStop,
                Msg::ConsentTerms,
                Msg::ConsentRisk,
                Msg::ConsentNotAffiliated,
            ]
            .map(|m| lang.t(m))
            .join(" ");
            assert!(
                all.contains("Grok CLI") && all.contains("OAuth"),
                "{lang:?}"
            );
            assert!(all.contains("xAI"), "{lang:?}");
        }
        assert!(Msg::ConsentAccept.es().starts_with("Entiendo"));
    }

    #[test]
    fn placeholders_are_filled_by_name() {
        assert_eq!(
            tf!(
                Lang::Es,
                RememberProposal,
                from = "jiperland",
                to = "Hyprland"
            ),
            "¿Recordar «jiperland» → «Hyprland»?"
        );
        assert_eq!(tf!(Lang::En, Queued, n = 2), "2 queued");
        assert_eq!(
            fill("{a} y {b}", &[("a", &1)]),
            "1 y {b}",
            "missing stays visible"
        );
        assert_eq!(fill("sin llaves {", &[]), "sin llaves {");
    }

    #[test]
    fn auto_follows_the_system_and_explicit_choices_win() {
        assert_eq!(Lang::from_locale("es-AR"), Lang::Es);
        assert_eq!(Lang::from_locale("es_ES.UTF-8"), Lang::Es);
        assert_eq!(Lang::from_locale("en-US"), Lang::En);
        assert_eq!(
            Lang::from_locale("pt-BR"),
            Lang::En,
            "untranslated → English"
        );
        assert_eq!(Lang::from_locale(""), Lang::En);
        assert_eq!(Lang::resolve("auto", Lang::Es), Lang::Es);
        assert_eq!(Lang::resolve("auto", Lang::En), Lang::En);
        assert_eq!(Lang::resolve("en", Lang::Es), Lang::En);
        assert_eq!(Lang::resolve("es", Lang::En), Lang::Es);
        for lang in Lang::ALL {
            assert_eq!(Lang::resolve(lang.code(), Lang::Es), lang);
            assert!(dicta_core::config::UI_LANGUAGES.contains(&lang.code()));
        }
    }
}
