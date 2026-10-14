//! Main window of Mars voice: Inicio (dashboard), Historial and every
//! setting. Every control maps to a `Command`; configuration edits are
//! validated locally with `AppConfig::validate` and sent as `UpdateConfig`,
//! style and colour mode as `SetTheme` / `SetColorMode`. Closing the window
//! leaves the app running; only "Salir de Mars voice" (or Ctrl+Q) quits.

use dicta_core::{
    AppConfig, AudioRetention, ColorMode, Command, ProviderMode, ShortcutMode, Theme,
};
use gpui::{
    AnyElement, ClipboardItem, Context, Div, Entity, FontWeight, SharedString, Window, div, img,
    prelude::*, px,
};

use crate::{
    CloseWindow, Hub, OVERLAY_APP_ID, OVERLAY_WIDTH, Quit,
    assets::{self, DISPLAY_FONT, Icon, MONO_FONT, UI_FONT},
    errors::ErrorOrigin,
    i18n::{Lang, Msg, tf},
    lang, perf,
    text_field::{FieldEvent, TextField},
    theme::{Palette, mode_label, theme_blurb, theme_name},
    ui_state,
    wave::{self, LevelHistory},
    widgets::{
        Kind, badge, button, confirm_bar, count_chip, demo_badge, disabled, grain, icon,
        icon_button, icon_label_button, kbd, radio_row, radio_row_with, segment, segmented,
        toggle_row,
    },
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Section {
    Home,
    History,
    General,
    Provider,
    Writing,
    Vocabulary,
    Corrections,
    Microphone,
    Shortcuts,
    Appearance,
    Data,
    Overlay,
    Background,
    Reading,
    Writer,
}

impl Section {
    #[cfg(test)]
    const ALL: [Self; 15] = [
        Self::Home,
        Self::History,
        Self::Reading,
        Self::Writer,
        Self::General,
        Self::Provider,
        Self::Writing,
        Self::Vocabulary,
        Self::Corrections,
        Self::Microphone,
        Self::Shortcuts,
        Self::Appearance,
        Self::Data,
        Self::Overlay,
        Self::Background,
    ];

    pub(crate) fn label(self, l: Lang) -> &'static str {
        l.t(match self {
            Self::Home => Msg::SecHome,
            Self::History => Msg::SecHistory,
            Self::General => Msg::SecGeneral,
            Self::Provider => Msg::SecProvider,
            Self::Writing => Msg::SecWriting,
            Self::Vocabulary => Msg::SecVocabulary,
            Self::Corrections => Msg::SecCorrections,
            Self::Microphone => Msg::SecMicrophone,
            Self::Shortcuts => Msg::SecShortcuts,
            Self::Appearance => Msg::SecAppearance,
            Self::Data => Msg::SecData,
            Self::Overlay => Msg::SecOverlay,
            Self::Background => Msg::SecBackground,
            Self::Reading => Msg::SecReading,
            Self::Writer => Msg::SecWriter,
        })
    }

    pub(crate) fn icon(self) -> Icon {
        match self {
            Self::Home => Icon::Home,
            Self::History => Icon::History,
            Self::General => Icon::General,
            Self::Provider => Icon::Provider,
            Self::Writing => Icon::Writing,
            Self::Vocabulary => Icon::Vocabulary,
            Self::Corrections => Icon::Corrections,
            Self::Microphone => Icon::Microphone,
            Self::Shortcuts => Icon::Shortcuts,
            Self::Appearance => Icon::Appearance,
            Self::Data => Icon::Audio,
            Self::Overlay => Icon::Overlay,
            Self::Background => Icon::Background,
            Self::Reading => Icon::Read,
            Self::Writer => Icon::Writer,
        }
    }

    /// One line under the page title.
    fn subtitle(self, l: Lang) -> &'static str {
        l.t(match self {
            Self::Home => Msg::SubHome,
            Self::History => Msg::SubHistory,
            Self::General => Msg::SubGeneral,
            Self::Provider => Msg::SubProvider,
            Self::Writing => Msg::SubWriting,
            Self::Vocabulary => Msg::SubVocabulary,
            Self::Corrections => Msg::SubCorrections,
            Self::Microphone => Msg::SubMicrophone,
            Self::Shortcuts => Msg::SubShortcuts,
            Self::Appearance => Msg::SubAppearance,
            Self::Data => Msg::SubData,
            Self::Overlay => Msg::SubOverlay,
            Self::Background => Msg::SubBackground,
            Self::Reading => Msg::SubReading,
            Self::Writer => Msg::SubWriter,
        })
    }

    /// Pages that manage their own layout instead of the settings column.
    fn full_bleed(self) -> bool {
        self == Self::History
    }
}

/// Sidebar groups, in order. The first has no heading.
const NAV_GROUPS: [(Option<Msg>, &[Section]); 4] = [
    (
        None,
        &[
            Section::Home,
            Section::History,
            Section::Reading,
            Section::Writer,
        ],
    ),
    (
        Some(Msg::NavDictation),
        &[
            Section::General,
            Section::Provider,
            Section::Microphone,
            Section::Shortcuts,
        ],
    ),
    (
        Some(Msg::NavText),
        &[Section::Writing, Section::Vocabulary, Section::Corrections],
    ),
    (
        Some(Msg::NavApp),
        &[
            Section::Appearance,
            Section::Data,
            Section::Overlay,
            Section::Background,
        ],
    ),
];

/// A destructive action waiting for the user's confirmation in place.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Confirm {
    DeleteNote(i64),
    DeleteAudio(i64),
    ClearHistory,
    DeleteReading(i64),
    DeleteReadingAudio(i64),
    ClearReadings,
}

/// Where the subscription consent card is open: the dictation provider,
/// the read-aloud account or Escritura's. Accepting it sets
/// `subscription_acknowledged`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Consent {
    Dictation,
    Reading,
    Writer,
}

/// The section that shows an error of this origin inline, if any.
fn inline_section(origin: ErrorOrigin) -> Option<Section> {
    match origin {
        ErrorOrigin::Microphone => Some(Section::Microphone),
        ErrorOrigin::Credential => Some(Section::Provider),
        ErrorOrigin::Dictation | ErrorOrigin::App => None,
    }
}

pub(crate) struct Fields {
    language: Entity<TextField>,
    pub(crate) api_key: Entity<TextField>,
    polish_model: Entity<TextField>,
    instructions: Entity<TextField>,
    endpointing: Entity<TextField>,
    keyterm: Entity<TextField>,
    corr_from: Entity<TextField>,
    corr_to: Entity<TextField>,
    toggle_shortcut: Entity<TextField>,
    paste_shortcut: Entity<TextField>,
    paste_keys: Entity<TextField>,
    overlay_x: Entity<TextField>,
    overlay_y: Entity<TextField>,
    pub(crate) search: Entity<TextField>,
    pub(crate) tts_read_shortcut: Entity<TextField>,
    pub(crate) tts_pause_shortcut: Entity<TextField>,
    pub(crate) tts_stop_shortcut: Entity<TextField>,
    pub(crate) tts_max_chars: Entity<TextField>,
    pub(crate) tts_confirm_chars: Entity<TextField>,
    pub(crate) tts_language: Entity<TextField>,
    pub(crate) tts_pron_from: Entity<TextField>,
    pub(crate) tts_pron_to: Entity<TextField>,
    pub(crate) tts_x: Entity<TextField>,
    pub(crate) tts_y: Entity<TextField>,
    pub(crate) tts_terminal: Entity<TextField>,
    /// Text pasted to be read; only "Leer" sends it.
    pub(crate) tts_paste: Entity<TextField>,
    pub(crate) writer_shortcut: Entity<TextField>,
    pub(crate) writer_model: Entity<TextField>,
    pub(crate) writer_instructions: Entity<TextField>,
    pub(crate) writer_x: Entity<TextField>,
    pub(crate) writer_y: Entity<TextField>,
}

pub struct Settings {
    pub(crate) hub: Entity<Hub>,
    pub(crate) section: Section,
    pub(crate) fields: Fields,
    pub(crate) confirm: Option<Confirm>,
    /// Narrow Historial shows one pane: the detail when a note was chosen.
    pub(crate) history_detail_open: bool,
    pub(crate) error: Option<String>,
    /// Backend error the user closed in the global banner; it reappears only
    /// when the controller reports a different one.
    dismissed_backend_error: Option<String>,
    pub(crate) notice: Option<String>,
    /// Subscription consent waiting for "Entiendo" or "Cancelar".
    pub(crate) consent: Option<Consent>,
    /// Width of the display this window is on, for the position presets.
    pub(crate) display_w: f32,
    digest: Option<u64>,
    /// Search typed but not yet sent, and the timer that will send it.
    pending_search: Option<String>,
    search_debounce: Option<gpui::Task<()>>,
    _subs: Vec<gpui::Subscription>,
}

/// Pause after the last keystroke before the history search is sent.
pub(crate) const SEARCH_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(180);

/// Streams `Debug` output into a hasher, so no string is built per snapshot.
struct HashWriter<'a>(&'a mut std::collections::hash_map::DefaultHasher);

impl std::fmt::Write for HashWriter<'_> {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        std::hash::Hasher::write(self.0, s.as_bytes());
        Ok(())
    }
}

/// Hash of everything the main window shows. Audio levels, `level_seq` and
/// the live transcript are left out: the main window never shows them.
fn digest(hub: &Hub) -> u64 {
    use std::{fmt::Write as _, hash::Hash, hash::Hasher};
    perf::bump(&perf::SETTINGS_DIGESTS);
    let s = &hub.snap;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let _ = write!(
        HashWriter(&mut h),
        "{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}",
        s.config,
        hub.pending_theme,
        hub.pending_mode,
        s.login,
        s.devices,
        s.corrections,
        hub.overlay_placement(),
        s.metrics,
        s.usage_period,
        s.phase,
        s.error,
    );
    (s.authenticated, s.demo, &s.status, &s.history_query).hash(&mut h);
    (s.notes_has_more, s.active_note_id, s.audio_playing_note_id).hash(&mut h);
    // Lectura: what Ajustes, Historial and Inicio show of it, never the
    // position or level of the reading being played.
    let t = &s.tts;
    let _ = write!(
        HashWriter(&mut h),
        "{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}",
        t.phase,
        t.voices,
        t.outputs,
        t.previewing_voice,
        s.history_kind,
        s.reading_metrics,
        s.selected_reading,
    );
    (
        t.show,
        t.account_ready,
        s.readings_has_more,
        s.active_reading_id,
    )
        .hash(&mut h);
    for r in s.readings.iter() {
        (
            r.id,
            r.created_at_ms,
            &r.text,
            &r.error,
            &r.voice,
            &r.language,
        )
            .hash(&mut h);
        (r.chars, r.chars_sent, r.duration_ms).hash(&mut h);
        (&r.audio_filename, r.audio_bytes).hash(&mut h);
        let _ = write!(HashWriter(&mut h), "{:?}{:?}", r.status, r.provider);
    }
    // Hash every mutable field of the notes: an edit can keep the length
    // and word count, so size alone is not enough.
    for n in s.notes.iter().chain(s.selected_note.as_ref()) {
        perf::bump(&perf::DIGEST_NOTES);
        (n.id, n.created_at_ms, &n.text, &n.original, &n.error).hash(&mut h);
        (n.word_count, n.captured_ms, n.sent_ms).hash(&mut h);
        (&n.audio_filename, n.audio_bytes).hash(&mut h);
        (n.input_tokens, n.output_tokens).hash(&mut h);
        let _ = write!(HashWriter(&mut h), "{:?}{:?}", n.status, n.provider);
    }
    // Separates "selected note" from "one more note in the list".
    s.selected_note.is_some().hash(&mut h);
    h.finish()
}

/// Shown instead of any OAuth client detail ([`Msg::SubscriptionHint`]):
/// Dicta runs the sign-in natively and only reports actionable errors
/// through the status line.
pub(crate) const SUBSCRIPTION_HINT: Msg = Msg::SubscriptionHint;

/// "Conectar con Grok" never depends on a client ID typed by the user.
pub(crate) fn connect_command() -> Command {
    Command::StartLogin
}

pub(crate) fn fmt_offset(v: f32) -> String {
    if v.fract() == 0. {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

fn parse_offset(text: &str, l: Lang) -> Result<f32, String> {
    text.trim()
        .replace(',', ".")
        .parse::<f32>()
        .ok()
        .filter(|v| v.is_finite() && v.abs() <= 20_000.)
        .ok_or_else(|| l.t(Msg::ErrOffset).to_string())
}

fn parse_endpointing(text: &str, l: Lang) -> Result<u16, String> {
    text.trim()
        .parse::<u16>()
        .ok()
        .filter(|v| *v <= 5000)
        .ok_or_else(|| l.t(Msg::ErrEndpointing).to_string())
}

impl Settings {
    pub fn new(hub: Entity<Hub>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let c = hub.read(cx).snap.config.clone();
        let l = lang(cx);
        fn single(
            text: String,
            placeholder: impl Into<crate::text_field::Placeholder>,
            window: &mut Window,
            cx: &mut Context<Settings>,
        ) -> Entity<TextField> {
            cx.new(|cx| TextField::new(text, placeholder, window, cx))
        }
        macro_rules! field {
            ($text:expr, $ph:expr) => {
                single($text, $ph, window, cx)
            };
        }
        let fields = Fields {
            language: field!(c.language.clone(), "es"),
            api_key: cx.new(|cx| TextField::new("", "xai-…", window, cx).masked()),
            polish_model: field!(c.polish_model.clone(), "grok-4.3"),
            instructions: cx.new(|cx| {
                TextField::new(c.instructions.clone(), Msg::PhInstructions, window, cx).multiline()
            }),
            endpointing: field!(c.endpointing_ms.to_string(), "400"),
            keyterm: field!(String::new(), Msg::PhKeyterm),
            corr_from: field!(String::new(), Msg::PhSoundsLike),
            corr_to: field!(String::new(), Msg::PhWrittenAs),
            toggle_shortcut: field!(c.toggle_shortcut.clone(), "Ctrl+Space"),
            paste_shortcut: field!(c.paste_shortcut.clone(), "Super+Shift+V"),
            paste_keys: field!(c.paste_keys.clone(), "CTRL+V"),
            overlay_x: field!(fmt_offset(c.overlay_x), "24"),
            overlay_y: field!(fmt_offset(c.overlay_y), "24"),
            search: field!(String::new(), Msg::PhSearch),
            tts_read_shortcut: field!(c.tts.read_shortcut.clone(), "Ctrl+Alt+Space"),
            tts_pause_shortcut: field!(c.tts.pause_shortcut.clone(), Msg::PhUnassigned),
            tts_stop_shortcut: field!(c.tts.stop_shortcut.clone(), Msg::PhUnassigned),
            tts_max_chars: field!(
                crate::format::count(c.tts.max_chars.into(), l),
                crate::format::count(20_000, l)
            ),
            tts_confirm_chars: field!(
                crate::format::count(c.tts.confirm_chars.into(), l),
                crate::format::count(5_000, l)
            ),
            tts_language: field!(c.tts.language.clone(), "auto"),
            tts_pron_from: field!(String::new(), Msg::PhWrittenAs),
            tts_pron_to: field!(String::new(), Msg::PhPronounced),
            tts_terminal: field!(String::new(), Msg::PhWindowClass),
            tts_x: field!(fmt_offset(c.tts.window_x), "24"),
            tts_y: field!(fmt_offset(c.tts.window_y), "24"),
            tts_paste: cx.new(|cx| {
                TextField::new(String::new(), Msg::PhPasteToRead, window, cx).multiline()
            }),
            writer_shortcut: field!(c.writer.shortcut.clone(), "Ctrl+Alt+E"),
            writer_model: field!(c.writer.model.clone(), "grok-4.3"),
            writer_instructions: cx.new(|cx| {
                TextField::new(
                    c.writer.instructions.clone(),
                    Msg::PhWriterInstructions,
                    window,
                    cx,
                )
                .multiline()
            }),
            writer_x: field!(fmt_offset(c.writer.window_x), "24"),
            writer_y: field!(fmt_offset(c.writer.window_y), "24"),
        };

        let mut subs = Vec::new();
        let mut on_commit =
            |f: &Entity<TextField>, apply: fn(&mut Settings, String, &mut Context<Settings>)| {
                subs.push(cx.subscribe(f, move |this, f, ev, cx| {
                    if *ev == FieldEvent::Commit {
                        let text = f.read(cx).text().to_string();
                        apply(this, text, cx);
                    }
                }));
            };
        on_commit(&fields.language, |s, t, cx| {
            s.edit(cx, |c| c.language = t.trim().to_string())
        });
        on_commit(&fields.api_key, |s, _, cx| s.save_api_key(cx));
        on_commit(&fields.polish_model, |s, t, cx| {
            s.edit(cx, |c| c.polish_model = t.trim().to_string())
        });
        on_commit(&fields.instructions, |s, t, cx| {
            s.edit(cx, |c| c.instructions = t)
        });
        on_commit(&fields.endpointing, |s, t, cx| {
            match parse_endpointing(&t, lang(cx)) {
                Ok(v) => s.edit(cx, |c| c.endpointing_ms = v),
                Err(e) => s.fail(e, cx),
            }
        });
        on_commit(&fields.keyterm, |s, _, cx| s.add_keyterm(cx));
        on_commit(&fields.corr_to, |s, _, cx| s.remember_manual(cx));
        on_commit(&fields.toggle_shortcut, |s, t, cx| {
            s.edit(cx, |c| c.toggle_shortcut = t.trim().to_string())
        });
        on_commit(&fields.paste_shortcut, |s, t, cx| {
            s.edit(cx, |c| c.paste_shortcut = t.trim().to_string())
        });
        on_commit(&fields.paste_keys, |s, t, cx| {
            s.edit(cx, |c| c.paste_keys = t.trim().to_string())
        });
        on_commit(&fields.overlay_x, |s, t, cx| {
            match parse_offset(&t, lang(cx)) {
                Ok(v) => s.edit(cx, |c| {
                    c.overlay_x = v;
                    c.overlay_centered = false;
                }),
                Err(e) => s.fail(e, cx),
            }
        });
        on_commit(&fields.overlay_y, |s, t, cx| {
            match parse_offset(&t, lang(cx)) {
                Ok(v) => s.edit(cx, |c| {
                    c.overlay_y = v;
                    c.overlay_centered = false;
                }),
                Err(e) => s.fail(e, cx),
            }
        });
        on_commit(&fields.tts_read_shortcut, |s, t, cx| {
            s.edit(cx, |c| c.tts.read_shortcut = t.trim().to_string())
        });
        on_commit(&fields.tts_pause_shortcut, |s, t, cx| {
            s.edit(cx, |c| c.tts.pause_shortcut = t.trim().to_string())
        });
        on_commit(&fields.tts_stop_shortcut, |s, t, cx| {
            s.edit(cx, |c| c.tts.stop_shortcut = t.trim().to_string())
        });
        on_commit(
            &fields.tts_max_chars,
            |s, t, cx| match crate::lectura::parse_chars(&t, lang(cx)) {
                Ok(v) => s.edit(cx, |c| c.tts.max_chars = v),
                Err(e) => s.fail(e, cx),
            },
        );
        on_commit(
            &fields.tts_confirm_chars,
            |s, t, cx| match crate::lectura::parse_chars(&t, lang(cx)) {
                Ok(v) => s.edit(cx, |c| c.tts.confirm_chars = v),
                Err(e) => s.fail(e, cx),
            },
        );
        on_commit(&fields.tts_language, |s, t, cx| {
            let code = t.trim();
            let code = if code.is_empty() { "auto" } else { code }.to_string();
            s.edit(cx, |c| c.tts.language = code)
        });
        on_commit(&fields.tts_pron_to, |s, _, cx| s.add_pronunciation(cx));
        on_commit(&fields.tts_terminal, |s, _, cx| s.add_terminal_class(cx));
        on_commit(&fields.tts_x, |s, t, cx| match parse_offset(&t, lang(cx)) {
            Ok(v) => s.edit(cx, |c| {
                c.tts.window_x = v;
                c.tts.window_centered = false;
            }),
            Err(e) => s.fail(e, cx),
        });
        on_commit(&fields.tts_y, |s, t, cx| match parse_offset(&t, lang(cx)) {
            Ok(v) => s.edit(cx, |c| {
                c.tts.window_y = v;
                c.tts.window_centered = false;
            }),
            Err(e) => s.fail(e, cx),
        });
        on_commit(&fields.writer_shortcut, |s, t, cx| {
            s.edit(cx, |c| c.writer.shortcut = t.trim().to_string())
        });
        on_commit(&fields.writer_model, |s, t, cx| {
            s.edit(cx, |c| c.writer.model = t.trim().to_string())
        });
        on_commit(&fields.writer_instructions, |s, t, cx| {
            s.edit(cx, |c| c.writer.instructions = t)
        });
        on_commit(&fields.writer_x, |s, t, cx| {
            match parse_offset(&t, lang(cx)) {
                Ok(v) => s.edit(cx, |c| {
                    c.writer.window_x = v;
                    c.writer.window_centered = false;
                }),
                Err(e) => s.fail(e, cx),
            }
        });
        on_commit(&fields.writer_y, |s, t, cx| {
            match parse_offset(&t, lang(cx)) {
                Ok(v) => s.edit(cx, |c| {
                    c.writer.window_y = v;
                    c.writer.window_centered = false;
                }),
                Err(e) => s.fail(e, cx),
            }
        });
        // The controller filters the history as the user types. A literal
        // search over a large history costs the controller tens of
        // milliseconds, so keystrokes are debounced: the query goes out
        // after a short pause, or at once on Enter.
        subs.push(cx.subscribe(&fields.search, |this, f, ev, cx| match ev {
            FieldEvent::Changed => {
                let query = f.read(cx).text().to_string();
                this.confirm = None;
                this.schedule_search(query, cx);
            }
            FieldEvent::Commit => this.flush_search(cx),
        }));
        // Snapshots arrive many times per second while recording; settings
        // only repaint when something they show actually changed.
        subs.push(cx.observe(&hub, |this, hub, cx| {
            let digest = digest(hub.read(cx));
            if this.digest == Some(digest) {
                return;
            }
            this.digest = Some(digest);
            let c = hub.read(cx).snap.config.clone();
            this.sync_fields(&c, lang(cx), cx);
            cx.notify();
        }));

        Self {
            hub,
            section: Section::Home,
            fields,
            confirm: None,
            history_detail_open: false,
            error: None,
            dismissed_backend_error: None,
            notice: None,
            consent: None,
            display_w: 1920.,
            digest: None,
            pending_search: None,
            search_debounce: None,
            _subs: subs,
        }
    }

    /// Sends `query` after [`SEARCH_DEBOUNCE`] without further typing; a
    /// newer query replaces a pending one.
    fn schedule_search(&mut self, query: String, cx: &mut Context<Self>) {
        self.pending_search = Some(query);
        self.search_debounce = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SEARCH_DEBOUNCE).await;
            this.update(cx, |this, cx| this.flush_search(cx)).ok();
        }));
    }

    /// Sends the pending query now, if any.
    fn flush_search(&mut self, cx: &mut Context<Self>) {
        self.search_debounce = None;
        if let Some(query) = self.pending_search.take() {
            perf::bump(&perf::SEARCH_SENT);
            self.send(cx, Command::SearchNotes(query));
        }
    }

    #[cfg_attr(not(feature = "screenshots"), allow(dead_code))]
    pub(crate) fn with_section(mut self, section: Section) -> Self {
        self.section = section;
        self
    }

    /// Gallery: open with the subscription consent showing.
    #[cfg_attr(not(feature = "screenshots"), allow(dead_code))]
    pub(crate) fn with_consent(mut self, consent: Consent) -> Self {
        self.consent = Some(consent);
        self
    }

    /// Fields that show configuration in the interface language (counts
    /// with separators) follow it: the config changes with the language, so
    /// the digest brings them here.
    fn sync_fields(&self, c: &AppConfig, l: Lang, cx: &mut Context<Self>) {
        let f = &self.fields;
        let pairs: [(&Entity<TextField>, String); 22] = [
            (&f.language, c.language.clone()),
            (&f.polish_model, c.polish_model.clone()),
            (&f.instructions, c.instructions.clone()),
            (&f.endpointing, c.endpointing_ms.to_string()),
            (&f.toggle_shortcut, c.toggle_shortcut.clone()),
            (&f.paste_shortcut, c.paste_shortcut.clone()),
            (&f.paste_keys, c.paste_keys.clone()),
            (&f.overlay_x, fmt_offset(c.overlay_x)),
            (&f.overlay_y, fmt_offset(c.overlay_y)),
            (&f.tts_read_shortcut, c.tts.read_shortcut.clone()),
            (&f.tts_pause_shortcut, c.tts.pause_shortcut.clone()),
            (&f.tts_stop_shortcut, c.tts.stop_shortcut.clone()),
            (
                &f.tts_max_chars,
                crate::format::count(c.tts.max_chars.into(), l),
            ),
            (
                &f.tts_confirm_chars,
                crate::format::count(c.tts.confirm_chars.into(), l),
            ),
            (&f.tts_language, c.tts.language.clone()),
            (&f.tts_x, fmt_offset(c.tts.window_x)),
            (&f.tts_y, fmt_offset(c.tts.window_y)),
            (&f.writer_shortcut, c.writer.shortcut.clone()),
            (&f.writer_model, c.writer.model.clone()),
            (&f.writer_instructions, c.writer.instructions.clone()),
            (&f.writer_x, fmt_offset(c.writer.window_x)),
            (&f.writer_y, fmt_offset(c.writer.window_y)),
        ];
        for (field, text) in pairs {
            field.update(cx, |field, cx| field.sync(&text, cx));
        }
    }

    pub(crate) fn send(&self, cx: &gpui::App, command: Command) {
        self.hub.read(cx).send(command);
    }

    pub(crate) fn edit(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut AppConfig)) {
        let result = self.hub.update(cx, |hub, cx| hub.update_config(cx, f));
        self.error = result.err();
        cx.notify();
    }

    pub(crate) fn fail(&mut self, message: String, cx: &mut Context<Self>) {
        self.error = Some(message);
        cx.notify();
    }

    pub(crate) fn save_api_key(&mut self, cx: &mut Context<Self>) {
        let l = lang(cx);
        let key = self.fields.api_key.read(cx).text().trim().to_string();
        if key.is_empty() {
            return self.fail(l.t(Msg::ErrPasteKey).into(), cx);
        }
        self.send(cx, Command::SaveApiKey(key));
        self.fields.api_key.update(cx, |f, cx| f.clear(cx));
        self.error = None;
        self.notice = Some(l.t(Msg::KeySent).into());
        cx.notify();
    }

    fn add_keyterm(&mut self, cx: &mut Context<Self>) {
        let term = self.fields.keyterm.read(cx).text().trim().to_string();
        if term.is_empty() {
            return;
        }
        if self
            .hub
            .read(cx)
            .snap
            .config
            .keyterms
            .iter()
            .any(|t| t.eq_ignore_ascii_case(&term))
        {
            return self.fail(tf!(lang(cx), ErrTermExists, term = term), cx);
        }
        self.edit(cx, |c| c.keyterms.push(term));
        if self.error.is_none() {
            self.fields.keyterm.update(cx, |f, cx| f.clear(cx));
        }
    }

    fn remember_manual(&mut self, cx: &mut Context<Self>) {
        let from = self.fields.corr_from.read(cx).text().trim().to_string();
        let to = self.fields.corr_to.read(cx).text().trim().to_string();
        if from.is_empty() || to.is_empty() || from == to {
            return self.fail(lang(cx).t(Msg::ErrCorrectionEmpty).into(), cx);
        }
        self.send(cx, Command::RememberCorrection { from, to });
        self.fields.corr_from.update(cx, |f, cx| f.clear(cx));
        self.fields.corr_to.update(cx, |f, cx| f.clear(cx));
        self.error = None;
        cx.notify();
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        self.hub.update(cx, |hub, cx| hub.close_settings(cx));
    }

    /// Switches page. Inicio always shows the latest notes, so an active
    /// history search is cleared on the way there.
    pub(crate) fn go(&mut self, section: Section, cx: &mut Context<Self>) {
        if section == Section::Home && !self.hub.read(cx).snap.history_query.is_empty() {
            self.fields.search.update(cx, |f, cx| f.clear(cx));
            self.pending_search = None;
            self.search_debounce = None;
            self.send(cx, Command::SearchNotes(String::new()));
        }
        self.section = section;
        // The sidebar opens Historial on its list.
        self.history_detail_open = false;
        self.error = None;
        self.notice = None;
        self.confirm = None;
        self.consent = None;
        cx.notify();
    }

    /// Picks the subscription for dictation or reading. The first time
    /// (`subscription_acknowledged` false) it opens the consent card instead;
    /// users already on the subscription never see it again.
    pub(crate) fn choose_subscription(&mut self, target: Consent, cx: &mut Context<Self>) {
        if self.hub.read(cx).snap.config.subscription_acknowledged {
            self.consent = None;
            self.edit(cx, |c| subscribe(c, target));
        } else {
            self.consent = Some(target);
            cx.notify();
        }
    }

    /// "Entiendo, usar suscripción": one `UpdateConfig` with the mode and
    /// the acknowledgement together.
    pub(crate) fn accept_subscription(&mut self, target: Consent, cx: &mut Context<Self>) {
        self.consent = None;
        self.edit(cx, |c| {
            c.subscription_acknowledged = true;
            subscribe(c, target);
        });
    }

    /// Choosing the API key closes a pending consent.
    pub(crate) fn choose_api_key(&mut self, target: Consent, cx: &mut Context<Self>) {
        if self.consent == Some(target) {
            self.consent = None;
        }
        self.edit(cx, |c| match target {
            Consent::Dictation => c.provider = ProviderMode::ApiKey,
            Consent::Reading => c.tts.provider = ProviderMode::ApiKey,
            Consent::Writer => c.writer.provider = ProviderMode::ApiKey,
        });
    }
}

fn subscribe(c: &mut AppConfig, target: Consent) {
    match target {
        Consent::Dictation => c.provider = ProviderMode::Subscription,
        Consent::Reading => c.tts.provider = ProviderMode::Subscription,
        Consent::Writer => c.writer.provider = ProviderMode::Subscription,
    }
}

pub(crate) const NAV_W: f32 = 220.;
pub(crate) const CONTENT_PAD_X: f32 = 28.;
/// Padding inside a group card.
pub(crate) const GROUP_PAD: f32 = 16.;
const THEME_CARD_GAP: f32 = 12.;
/// Narrowest style card that still fits the Órbita preview.
const THEME_CARD_MIN_W: f32 = 196.;
/// Settings pages keep a readable column; Inicio is wider.
pub(crate) const COLUMN_W: f32 = 660.;
pub(crate) const DASHBOARD_W: f32 = 900.;

/// Invented levels for the style previews, frozen in time so they never
/// animate: a soft start, a firm word and a quieter tail. Not a recording.
fn preview_levels() -> LevelHistory {
    let start = std::time::Instant::now();
    let mut h = LevelHistory::default();
    let shape = [
        0.0, 0.0, 0.004, 0.012, 0.03, 0.06, 0.09, 0.05, 0.02, 0.04, 0.11, 0.16, 0.12, 0.07, 0.03,
        0.05, 0.08, 0.06, 0.03, 0.015, 0.03, 0.05, 0.04, 0.02,
    ];
    for (i, rms) in shape.into_iter().enumerate() {
        h.push(
            i as u64,
            rms,
            start + std::time::Duration::from_millis(100 * i as u64),
        );
    }
    h.freeze_at(start + std::time::Duration::from_millis(100 * shape.len() as u64));
    h
}

/// A titled card. Cards sit on the grainy page surface, so the grain shows
/// between groups and never under the text inside them.
pub(crate) fn group(heading: impl Into<SharedString>, p: Palette) -> Div {
    group_with(div().child(heading.into()), p)
}

/// [`group`] whose heading carries more than text, e.g. a badge.
pub(crate) fn group_with(heading: Div, p: Palette) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(10.))
        .mt(px(14.))
        .p(px(GROUP_PAD))
        .rounded(px(14.))
        .bg(p.raised)
        .border_1()
        .border_color(p.line)
        .child(
            heading
                .font_family(DISPLAY_FONT)
                .font_weight(FontWeight::SEMIBOLD)
                .text_size(px(13.5)),
        )
}

pub(crate) fn hint(text: impl Into<SharedString>, p: Palette) -> Div {
    div()
        .text_size(px(12.5))
        .line_height(px(18.))
        .text_color(p.text_2)
        .child(text.into())
}

/// "En Hyprland: …" and other session-only notes: same look as [`hint`],
/// shown only when `show` (the session matches).
pub(crate) fn session_hint(show: bool, text: impl Into<SharedString>, p: Palette) -> Option<Div> {
    show.then(|| hint(text, p))
}

/// Backend errors (`AppSnapshot::error`) are user-safe text from the
/// controller. They are shown whole, one paragraph per line and wrapped, never
/// truncated, in ink: the icon and the danger edge carry the alarm.
pub(crate) fn error_block(heading: &'static str, message: &str, p: Palette) -> Div {
    error_block_with(heading, message, None, p)
}

/// [`error_block`] with an action under the message, in the flow so it can
/// never cover the text.
fn error_block_with(
    heading: &'static str,
    message: &str,
    action: Option<AnyElement>,
    p: Palette,
) -> Div {
    div()
        .flex()
        .gap(px(10.))
        .p(px(12.))
        .rounded(px(10.))
        .bg(p.raised)
        .border_1()
        .border_color(p.danger)
        .child(div().pt(px(1.)).child(icon(Icon::Alert, 17., p.danger)))
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(p.text)
                        .child(heading),
                )
                .children(
                    message
                        .lines()
                        .map(str::trim)
                        .filter(|line| !line.is_empty())
                        .map(|line| {
                            div()
                                .min_w(px(0.))
                                .text_size(px(13.))
                                .line_height(px(19.))
                                .text_color(p.text)
                                .child(SharedString::from(line.to_string()))
                        }),
                )
                .when_some(action, |d, action| {
                    d.child(div().flex().pt(px(4.)).ml(px(-10.)).child(action))
                }),
        )
}

/// Errors with a home section (micrófono, credencial) show inline there;
/// every other page, and every other error, gets a global banner until
/// dismissed.
fn global_backend_error<'a>(
    section: Section,
    error: Option<&'a str>,
    origin: Option<ErrorOrigin>,
    dismissed: Option<&str>,
) -> Option<&'a str> {
    let home = origin.and_then(inline_section);
    error.filter(|e| home != Some(section) && Some(*e) != dismissed)
}

pub(crate) fn labelled(label: &'static str, field: &Entity<TextField>, p: Palette) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(5.))
        .child(div().text_size(px(12.5)).text_color(p.text_2).child(label))
        .child(field.clone())
}

/// "mars voice" set like the Mars wordmark: lowercase, wide, the product
/// word lighter.
pub(crate) fn wordmark(p: Palette) -> Div {
    div()
        .flex()
        .items_baseline()
        .gap(px(5.))
        .font_family(DISPLAY_FONT)
        .text_size(px(19.))
        .line_height(px(24.))
        .child(div().font_weight(FontWeight::BOLD).child("mars"))
        .child(
            div()
                .font_weight(FontWeight::MEDIUM)
                .text_color(p.text_2)
                .child("voice"),
        )
}

impl Settings {
    fn nav(&self, p: Palette, height: f32, cx: &mut Context<Self>) -> impl IntoElement {
        let l = lang(cx);
        let hub = self.hub.read(cx);
        let snap = &hub.snap;
        let (terms, learned) = (snap.config.keyterms.len(), snap.corrections.len());
        let counts = move |s: Section| match s {
            Section::Vocabulary => Some(terms.to_string()),
            Section::Corrections => Some(learned.to_string()),
            _ => None,
        };
        let origin = ErrorOrigin::of(snap);
        let flagged = origin.and_then(inline_section);
        let connected = snap.authenticated;
        let demo = snap.demo;
        let style = format!(
            "{} · {}",
            theme_name(hub.theme(), l),
            mode_label(hub.color_mode(), l)
        );
        let mode = l.t(match snap.config.shortcut_mode {
            ShortcutMode::Toggle => Msg::NavModePress,
            ShortcutMode::Hold => Msg::NavModeHold,
        });
        let shortcut = snap.config.toggle_shortcut.clone();
        let reading_live = snap.tts.show && snap.tts.phase.audible();

        let item = |s: Section, cx: &mut Context<Self>| {
            let selected = s == self.section;
            let trailing: Option<AnyElement> = if flagged == Some(s) {
                Some(
                    div()
                        .size(px(8.))
                        .rounded_full()
                        .bg(p.danger)
                        .into_any_element(),
                )
            } else if s == Section::Reading && reading_live {
                Some(
                    div()
                        .size(px(8.))
                        .rounded_full()
                        .bg(p.orange)
                        .into_any_element(),
                )
            } else if s == Section::Provider {
                Some(
                    div()
                        .size(px(8.))
                        .rounded_full()
                        .when(connected, |d| d.bg(p.voice))
                        .when(!connected, |d| d.border_1().border_color(p.control))
                        .into_any_element(),
                )
            } else {
                counts(s).map(|n| count_chip(n, p).into_any_element())
            };
            div()
                // Stable across languages.
                .id(("nav", s as usize))
                .flex()
                .items_center()
                .gap(px(10.))
                .px(px(10.))
                .h(px(34.))
                .rounded(px(10.))
                .cursor_pointer()
                .text_color(if selected { p.text } else { p.text_2 })
                .font_weight(if selected {
                    FontWeight::SEMIBOLD
                } else {
                    FontWeight::MEDIUM
                })
                .when(selected, |d| {
                    d.bg(p.surface).border_1().border_color(p.line)
                })
                .when(!selected, move |d| {
                    d.hover(move |st| st.bg(p.hover()).text_color(p.text))
                })
                // Selected entries light their icon in planet orange.
                .child(icon(
                    s.icon(),
                    17.,
                    if selected { p.orange } else { p.text_2 },
                ))
                .child(div().flex_1().min_w(px(0.)).truncate().child(s.label(l)))
                .children(trailing)
                .on_click(cx.listener(move |this, _, _, cx| this.go(s, cx)))
        };

        let mut groups = Vec::new();
        for (label, sections) in NAV_GROUPS {
            groups.push(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .when(label.is_none(), |d| d.pt(px(10.)))
                    .when_some(label, |d, label| {
                        d.child(
                            div()
                                .px(px(10.))
                                .pt(px(14.))
                                .pb(px(4.))
                                .text_size(px(11.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(p.text_2)
                                .child(l.t(label).to_uppercase()),
                        )
                    })
                    .children(sections.iter().map(|s| item(*s, cx))),
            );
        }

        let status =
            div()
                .id("nav-status")
                .flex()
                .flex_col()
                .gap(px(4.))
                .p(px(12.))
                .rounded(px(12.))
                .bg(p.surface)
                .border_1()
                .border_color(p.line)
                .cursor_pointer()
                .hover(move |s| s.border_color(p.control))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_size(px(13.))
                        .child(div().flex_none().size(px(8.)).rounded_full().map(|d| {
                            match (flagged, connected) {
                                (Some(_), _) => d.bg(p.danger),
                                (None, true) => d.bg(p.voice),
                                (None, false) => d.border_1().border_color(p.control),
                            }
                        }))
                        .child(div().flex_1().min_w(px(0.)).truncate().child(l.t(
                            match (origin, connected, demo) {
                                (Some(ErrorOrigin::Microphone), _, _) => Msg::NavCheckMic,
                                (Some(ErrorOrigin::Credential), _, _) => Msg::NavCheckConnection,
                                (_, true, true) => Msg::NavGrokSimulated,
                                (_, true, false) => Msg::NavGrokConnected,
                                (_, false, _) => Msg::NavGrokDisconnected,
                            },
                        ))),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(5.))
                        .text_size(px(12.))
                        .text_color(p.text_2)
                        .child(kbd(shortcut, p))
                        .child(format!("{mode} · {style}")),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.go(flagged.unwrap_or(Section::Provider), cx)
                }));

        div()
            .relative()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(NAV_W))
            .h_full()
            .bg(p.ground)
            .border_r_1()
            .border_color(p.line)
            .overflow_hidden()
            .child(grain(p, NAV_W, height))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .px(px(18.))
                    .pt(px(20.))
                    .pb(px(6.))
                    .child(
                        img(assets::art(assets::mark(40., p.dark)))
                            .flex_none()
                            .size(px(40.)),
                    )
                    .child(wordmark(p)),
            )
            .child(
                div()
                    .id("nav-scroll")
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .px(px(8.))
                    .pb(px(8.))
                    .children(groups),
            )
            .child(div().flex_none().p(px(10.)).child(status))
    }

    /// Ajustes → General → Idioma de la interfaz. Each language reads in
    /// its own name, so it can be found whatever is on screen.
    fn interface_language(&self, p: Palette, cx: &mut Context<Self>) -> Div {
        let state = ui_state(cx);
        let (l, system) = (state.lang, state.system_lang);
        let current = self.hub.read(cx).snap.config.ui_language.clone();
        let options: [(&'static str, &'static str); 3] = [
            ("auto", l.t(Msg::Automatic)),
            // Endonyms: each language in its own name.
            ("es", "Español"),
            ("en", "English"),
        ];
        let system_name = l.t(match system {
            Lang::Es => Msg::LangSpanish,
            Lang::En => Msg::LangEnglish,
        });
        group(l.t(Msg::UiLanguage), p)
            .child(div().flex().child(segmented(
                options.into_iter().enumerate().map(|(i, (code, label))| {
                    segment(("ui-lang", i), label, current == code, p)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.edit(cx, |c| c.ui_language = code.into())
                        }))
                        .into_any_element()
                }),
                p,
            )))
            .child(hint(
                // "español" mid-sentence, "Spanish" always capitalised.
                match l {
                    Lang::Es => tf!(l, UiLanguageAuto, lang = system_name.to_lowercase()),
                    Lang::En => tf!(l, UiLanguageAuto, lang = system_name),
                },
                p,
            ))
    }

    fn general(&self, p: Palette, cx: &mut Context<Self>) -> Div {
        let l = lang(cx);
        let hub = self.hub.read(cx);
        let c = &hub.snap.config;
        let dictation_lang = c.language.clone();
        let copy = c.copy_on_finish;
        let paste = c.paste_shortcut.clone();
        div()
            .flex()
            .flex_col()
            .child(self.interface_language(p, cx))
            .child(
                group(l.t(Msg::DictationLanguage), p)
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap(px(12.))
                            .child(segmented(
                                [
                                    ("es", Msg::LangSpanish),
                                    ("en", Msg::LangEnglish),
                                    ("pt", Msg::LangPortuguese),
                                ]
                                .into_iter()
                                .map(|(code, label)| {
                                    segment(code, l.t(label), dictation_lang == code, p)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.edit(cx, |c| c.language = code.into())
                                        }))
                                        .into_any_element()
                                }),
                                p,
                            ))
                            .child(div().w(px(110.)).child(self.fields.language.clone())),
                    )
                    .child(hint(l.t(Msg::DictationLanguageHint), p)),
            )
            .child(
                group(l.t(Msg::OnFinish), p)
                    .child(
                        radio_row(
                            "copy",
                            l.t(Msg::CopyToClipboard),
                            Some(l.t(Msg::Default).into()),
                            copy,
                            p,
                        )
                        .on_click(
                            cx.listener(|this, _, _, cx| {
                                this.edit(cx, |c| c.copy_on_finish = true)
                            }),
                        ),
                    )
                    .child(
                        radio_row(
                            "show",
                            l.t(Msg::OnlyShowText),
                            Some(l.t(Msg::OnlyShowTextHint).into()),
                            !copy,
                            p,
                        )
                        .on_click(
                            cx.listener(|this, _, _, cx| {
                                this.edit(cx, |c| c.copy_on_finish = false)
                            }),
                        ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap(px(6.))
                            .child(hint(l.t(Msg::PasteHintBefore), p))
                            .child(kbd(paste, p))
                            .child(hint(l.t(Msg::PasteHintAfter), p)),
                    ),
            )
    }

    fn appearance(&self, p: Palette, width: f32, cx: &mut Context<Self>) -> Div {
        let l = lang(cx);
        let hub = self.hub.read(cx);
        let active = hub.theme();
        let mode = hub.color_mode();
        // Three cards side by side when they fit, else one per row.
        let inner = width.min(COLUMN_W) - GROUP_PAD * 2. - 2.;
        let per_row = if inner >= THEME_CARD_MIN_W * 3. + THEME_CARD_GAP * 2. {
            3
        } else {
            1
        };
        let mut cards = Theme::ALL
            .into_iter()
            .map(|t| self.theme_card(t, t == active, mode, p, cx))
            .collect::<Vec<_>>()
            .into_iter()
            .peekable();
        let mut grid = div().flex().flex_col().gap(px(THEME_CARD_GAP));
        while cards.peek().is_some() {
            grid = grid.child(
                div()
                    .flex()
                    .gap(px(THEME_CARD_GAP))
                    .children(cards.by_ref().take(per_row)),
            );
        }
        div()
            .flex()
            .flex_col()
            .child(
                group(l.t(Msg::Colors), p).child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(12.))
                        .child(segmented(
                            [ColorMode::Light, ColorMode::Dark].into_iter().map(|m| {
                                segment(("mode", m as usize), mode_label(m, l), m == mode, p)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.hub.update(cx, |hub, cx| hub.set_color_mode(m, cx))
                                    }))
                                    .into_any_element()
                            }),
                            p,
                        ))
                        .child(hint(l.t(Msg::ColorsHint), p).flex_1().min_w(px(180.))),
                ),
            )
            .child(
                group(l.t(Msg::VoiceStyle), p)
                    .child(hint(l.t(Msg::VoiceStyleHint), p))
                    .child(grid),
            )
    }

    fn theme_card(
        &self,
        theme: Theme,
        selected: bool,
        mode: ColorMode,
        current: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let l = lang(cx);
        let tp = Palette::new(theme, mode);
        let orbit = theme == Theme::Orbit;
        // The real voice renderer on invented, frozen levels.
        let voice = wave::voice(&preview_levels(), tp, true, false);
        // One stage height for the three, sized for Órbita, so the labels align.
        let stage_h = wave::ORBIT_H + 16.;
        let pill = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(10.))
            .w_full()
            .h(px(stage_h - 16.))
            .rounded(px(tp.radius - 4.))
            .bg(tp.surface)
            .border_1()
            .border_color(tp.line)
            .overflow_hidden()
            .when(!orbit, |d| {
                d.child(
                    img(assets::art(assets::mars(22.)))
                        .flex_none()
                        .size(px(22.)),
                )
            })
            .child(div().flex_1().min_w(px(0.)).child(voice));
        div()
            .id(("theme", theme as usize))
            .flex_1()
            .min_w(px(THEME_CARD_MIN_W))
            .flex()
            .flex_col()
            .rounded(px(12.))
            .overflow_hidden()
            .border_2()
            .border_color(if selected {
                current.orange
            } else {
                current.line
            })
            .cursor_pointer()
            .when(!selected, move |d| {
                d.hover(move |s| s.border_color(current.control))
            })
            .child(
                div()
                    .relative()
                    .h(px(stage_h))
                    .p(px(8.))
                    // The card's mask is rectangular: inner fills and the
                    // planet round to its 10 px inner radius (12 minus border).
                    .rounded_t(px(10.))
                    .bg(tp.ground)
                    .flex()
                    .items_center()
                    .overflow_hidden()
                    .child(grain(tp, 420., stage_h))
                    .when(tp.planet_corner, |d| {
                        d.child(
                            img(assets::art(assets::PLANET_CORNER.clone()))
                                .absolute()
                                .top(px(0.))
                                .right(px(0.))
                                .size(px(34.))
                                .rounded_tr(px(10.)),
                        )
                    })
                    .child(pill),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .p(px(10.))
                    .rounded_b(px(10.))
                    .bg(current.raised)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .font_family(DISPLAY_FONT)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(theme_name(theme, l))
                            .when(selected, |d| {
                                d.child(div().flex_1())
                                    .child(icon(Icon::Check, 15., current.text))
                            }),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .line_height(px(17.))
                            .text_color(current.text_2)
                            .child(theme_blurb(theme, l)),
                    ),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.hub.update(cx, |hub, cx| hub.set_theme(theme, cx));
            }))
            .into_any_element()
    }

    fn data(&self, p: Palette, cx: &mut Context<Self>) -> Div {
        let l = lang(cx);
        let c = &self.hub.read(cx).snap.config;
        let enabled = c.history_enabled;
        let retention = c.audio_retention;
        let stored = self.hub.read(cx).snap.metrics.audio_bytes;
        let options = [
            (AudioRetention::Off, Msg::RetentionOffNote),
            (AudioRetention::Days7, Msg::Retention7Note),
            (AudioRetention::Days30, Msg::Retention30Note),
            (AudioRetention::Forever, Msg::RetentionForeverNote),
        ];
        let confirming = self.confirm == Some(Confirm::ClearHistory);
        div()
            .flex()
            .flex_col()
            .child(
                group(l.t(Msg::TranscriptHistory), p)
                    .child(
                        toggle_row(
                            "history-enabled",
                            l.t(Msg::KeepTranscriptHistory),
                            Some(l.t(Msg::KeepTranscriptHistoryNote)),
                            enabled,
                            p,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.edit(cx, |c| c.history_enabled = !c.history_enabled)
                        })),
                    )
                    .when(!enabled, |d| d.child(hint(l.t(Msg::HistoryOffNote), p))),
            )
            .child(
                group(l.t(Msg::DictationAudio), p)
                    .children(options.into_iter().map(|(r, desc)| {
                        let row = radio_row(
                            ("retention", r as usize),
                            retention_label(r, l),
                            Some(l.t(desc).into()),
                            r == retention,
                            p,
                        );
                        if enabled {
                            row.on_click(cx.listener(move |this, _, _, cx| {
                                this.edit(cx, |c| c.audio_retention = r)
                            }))
                        } else {
                            disabled(row)
                        }
                    }))
                    .when(!enabled, |d| d.child(hint(l.t(Msg::AudioNeedsHistory), p)))
                    .child(hint(l.t(Msg::AudioChangesNext), p))
                    .child(hint(
                        tf!(l, AudioKeptNote, size = crate::format::bytes(stored, l)),
                        p,
                    )),
            )
            .child(
                group(l.t(Msg::Delete), p)
                    .child(hint(l.t(Msg::ClearHistoryNote), p))
                    .child(if confirming {
                        confirm_bar(
                            l.t(Msg::ClearHistoryQuestion),
                            l.t(Msg::ClearHistory),
                            cx.listener(|this, _, _, cx| {
                                this.confirm = None;
                                this.send(cx, Command::ClearHistory);
                                cx.notify();
                            }),
                            cx.listener(|this, _, _, cx| {
                                this.confirm = None;
                                cx.notify();
                            }),
                            l,
                            p,
                        )
                        .into_any_element()
                    } else {
                        div()
                            .flex()
                            .child(
                                icon_label_button(
                                    "clear-history",
                                    Icon::Trash,
                                    l.t(Msg::ClearHistory),
                                    Kind::Secondary,
                                    p,
                                )
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.confirm = Some(Confirm::ClearHistory);
                                        cx.notify();
                                    },
                                )),
                            )
                            .into_any_element()
                    }),
            )
    }

    /// The two account options, API key first and recommended, shared by
    /// Proveedor and Lectura → Cuenta. The subscription opens the consent
    /// card the first time.
    pub(crate) fn account_options(
        &self,
        target: Consent,
        mode: ProviderMode,
        p: Palette,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let l = lang(cx);
        let (api_id, sub_id, sub_desc) = match target {
            Consent::Dictation => ("api", "sub", Msg::ProviderSubDesc),
            Consent::Reading => ("tts-api", "tts-sub", Msg::ReadingSubDesc),
            Consent::Writer => ("writer-api", "writer-sub", Msg::ReadingSubDesc),
        };
        let mut rows = vec![
            radio_row_with(
                api_id,
                l.t(Msg::ProviderApiTitle),
                Some(badge(l.t(Msg::RecommendedBadge), false, p)),
                Some(l.t(Msg::ProviderApiDesc).into()),
                mode == ProviderMode::ApiKey,
                p,
            )
            .on_click(cx.listener(move |this, _, _, cx| this.choose_api_key(target, cx)))
            .into_any_element(),
            radio_row(
                sub_id,
                l.t(Msg::ProviderSubTitle),
                Some(l.t(sub_desc).into()),
                mode == ProviderMode::Subscription,
                p,
            )
            .on_click(cx.listener(move |this, _, _, cx| this.choose_subscription(target, cx)))
            .into_any_element(),
        ];
        if self.consent == Some(target) && mode != ProviderMode::Subscription {
            rows.push(self.consent_card(target, p, cx).into_any_element());
        }
        rows
    }

    /// Explains the experimental subscription before it is used: the OAuth
    /// client is Grok CLI's public one, it may stop working or be blocked,
    /// it may conflict with xAI's terms, the account bears the risk and Mars
    /// voice is not affiliated with xAI.
    fn consent_card(&self, target: Consent, p: Palette, cx: &mut Context<Self>) -> Div {
        let l = lang(cx);
        let points = [
            Msg::ConsentClient,
            Msg::ConsentMayStop,
            Msg::ConsentTerms,
            Msg::ConsentRisk,
            Msg::ConsentNotAffiliated,
        ];
        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .ml(px(36.))
            .p(px(14.))
            .rounded(px(12.))
            .bg(p.surface)
            .border_1()
            .border_color(p.orange_shade)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(icon(Icon::Alert, 16., p.orange_shade))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(l.t(Msg::ConsentTitle)),
                    )
                    .child(badge(l.t(Msg::ExperimentalBadge), true, p)),
            )
            .child(hint(l.t(Msg::ConsentIntro), p))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .children(points.into_iter().map(|m| {
                        div()
                            .flex()
                            .items_start()
                            .gap(px(9.))
                            .child(
                                div()
                                    .flex_none()
                                    .mt(px(7.))
                                    .size(px(5.))
                                    .rounded_full()
                                    .bg(p.text_2),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .text_size(px(13.))
                                    .line_height(px(19.))
                                    .child(l.t(m)),
                            )
                    })),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(8.))
                    .pt(px(2.))
                    .child(
                        button(
                            "consent-accept",
                            l.t(Msg::ConsentAccept),
                            Kind::Secondary,
                            p,
                        )
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.accept_subscription(target, cx)),
                        ),
                    )
                    .child(
                        button("consent-cancel", l.t(Msg::Cancel), Kind::Ghost, p).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.consent = None;
                                cx.notify();
                            }),
                        ),
                    ),
            )
            .child(hint(l.t(Msg::ConsentApiAdvice), p))
    }

    fn provider(&self, p: Palette, cx: &mut Context<Self>) -> Div {
        let l = lang(cx);
        let snap = &self.hub.read(cx).snap;
        let mode = snap.config.provider;
        let authenticated = snap.authenticated;
        let demo = snap.demo;
        let login = snap.login.clone();
        let status = snap.status.clone();
        let origin = ErrorOrigin::of(snap);
        // Only credential errors belong here. A failed dictation (e.g. the
        // microphone) shares `status`/`error` but says nothing about the
        // credential, so it must not paint this row red.
        let failed = origin == Some(ErrorOrigin::Credential);
        let backend_error = snap.error.clone().filter(|_| failed);
        let foreign_error = origin.is_some() && !failed;
        let subscription = mode == ProviderMode::Subscription;

        let state_row = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(8.))
            .p(px(10.))
            .rounded(px(10.))
            .bg(p.raised)
            .border_1()
            .border_color(p.line)
            .child(div().size(px(9.)).rounded_full().bg(if failed {
                p.danger
            } else if authenticated {
                p.voice
            } else {
                p.line
            }))
            .child(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .child(l.t(if authenticated {
                        Msg::CredentialAvailable
                    } else {
                        Msg::NoVerifiedCredential
                    })),
            )
            // Users already on the subscription keep a quiet reminder.
            .when(subscription, |d| {
                d.child(badge(l.t(Msg::ExperimentalBadge), true, p))
            })
            .when(demo, |d| {
                d.child(demo_badge(p)).child(hint(l.t(Msg::Simulated), p))
            })
            .child(
                // Wide enough to wrap onto its own line instead of a sliver.
                div()
                    .flex_1()
                    .min_w(px(220.))
                    .text_size(px(12.5))
                    .line_height(px(18.))
                    .text_color(if failed { p.text } else { p.text_2 })
                    .child(if foreign_error {
                        l.t(Msg::CredentialNotInvolved).to_string()
                    } else {
                        status
                    }),
            )
            .when(authenticated, |d| {
                d.child(
                    button("disconnect", l.t(Msg::Disconnect), Kind::Ghost, p)
                        .on_click(cx.listener(|this, _, _, cx| this.send(cx, Command::Disconnect))),
                )
            });

        let mut body = div().flex().flex_col().child(
            group(l.t(Msg::HowConnects), p)
                .children(self.account_options(Consent::Dictation, mode, p, cx))
                .child(hint(l.t(Msg::NeverSwitches), p))
                .child(state_row)
                .when_some(backend_error, |d, e| {
                    d.child(error_block(ErrorOrigin::Credential.title(l), &e, p))
                })
                .child(hint(l.t(Msg::CoverageChecked), p)),
        );

        body = match mode {
            ProviderMode::Subscription => {
                let heading = div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(l.t(Msg::Subscription))
                    .child(badge(l.t(Msg::ExperimentalBadge), true, p));
                let mut g = group_with(heading, p).child(hint(l.t(SUBSCRIPTION_HINT), p));
                g = match login {
                    Some(prompt) => g.child(self.login_panel(prompt, demo, p, cx)),
                    None if !authenticated => g.child(
                        div().flex().items_center().gap(px(10.)).child(
                            button(
                                "login",
                                l.t(if failed {
                                    Msg::ReconnectGrok
                                } else {
                                    Msg::ConnectGrok
                                }),
                                Kind::Primary,
                                p,
                            )
                            .on_click(
                                cx.listener(|this, _, _, cx| this.send(cx, connect_command())),
                            ),
                        ),
                    ),
                    None => g,
                };
                body.child(g)
            }
            ProviderMode::ApiKey => body.child(
                group(l.t(Msg::ApiKey), p)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(div().flex_1().child(self.fields.api_key.clone()))
                            .child(
                                button("save-key", l.t(Msg::SaveKey), Kind::Primary, p)
                                    .on_click(cx.listener(|this, _, _, cx| this.save_api_key(cx))),
                            ),
                    )
                    .child(hint(l.t(Msg::ApiKeyStored), p))
                    .when(authenticated, |d| {
                        d.child(div().flex().child(
                            button("forget-key", l.t(Msg::DeleteKey), Kind::Ghost, p).on_click(
                                cx.listener(|this, _, _, cx| this.send(cx, Command::Disconnect)),
                            ),
                        ))
                    }),
            ),
        };
        body
    }

    pub(crate) fn login_panel(
        &self,
        prompt: dicta_core::LoginPrompt,
        demo: bool,
        p: Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let l = lang(cx);
        let uri = prompt.verification_uri.clone();
        let code = prompt.user_code.clone();
        let safe = uri.starts_with("https://");
        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .p(px(14.))
            .rounded(px(12.))
            .bg(p.raised)
            .border_1()
            .border_color(p.line)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(l.t(Msg::SignInBrowser))
                    .when(demo, |d| d.child(demo_badge(p))),
            )
            .child(hint(l.t(Msg::LoginStep1), p))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .font_family(MONO_FONT)
                            .text_size(px(13.))
                            .child(uri.clone()),
                    )
                    .child(if safe {
                        icon_label_button(
                            "open-url",
                            Icon::Link,
                            l.t(Msg::OpenLink),
                            Kind::Secondary,
                            p,
                        )
                        .on_click(move |_, _, cx| cx.open_url(&uri))
                    } else {
                        disabled(icon_label_button(
                            "open-url",
                            Icon::Link,
                            l.t(Msg::OpenLink),
                            Kind::Secondary,
                            p,
                        ))
                    }),
            )
            .when(!safe, |d| d.child(hint(l.t(Msg::LinkNotHttps), p)))
            .child(hint(l.t(Msg::LoginStep2), p))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(
                        div()
                            .font_family(MONO_FONT)
                            .text_size(px(24.))
                            .font_weight(FontWeight::MEDIUM)
                            .child(code.clone()),
                    )
                    .child(
                        button("copy-code", l.t(Msg::CopyCode), Kind::Ghost, p).on_click(
                            move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(code.clone()))
                            },
                        ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(div().flex_1().child(hint(l.t(Msg::WaitingConfirmation), p)))
                    .child(
                        button("cancel-login", l.t(Msg::Cancel), Kind::Ghost, p).on_click(
                            cx.listener(|this, _, _, cx| this.send(cx, Command::CancelLogin)),
                        ),
                    ),
            )
    }

    fn writing(&self, p: Palette, cx: &mut Context<Self>) -> Div {
        let l = lang(cx);
        let session = ui_state(cx).session;
        let c = self.hub.read(cx).snap.config.clone();
        div()
            .flex()
            .flex_col()
            .child(
                group(l.t(Msg::Polish), p)
                    .child(
                        toggle_row(
                            "polish",
                            l.t(Msg::PolishToggle),
                            Some(l.t(Msg::PolishToggleNote)),
                            c.polish,
                            p,
                        )
                        .on_click(
                            cx.listener(|this, _, _, cx| this.edit(cx, |c| c.polish = !c.polish)),
                        ),
                    )
                    .child(labelled(l.t(Msg::Model), &self.fields.polish_model, p))
                    .child(labelled(
                        l.t(Msg::Instructions),
                        &self.fields.instructions,
                        p,
                    ))
                    .child(hint(
                        tf!(l, InstructionsSave, key = session.with_mod("Enter")),
                        p,
                    )),
            )
            .child(
                group(l.t(Msg::Transcription), p)
                    .child(
                        toggle_row(
                            "numbers",
                            l.t(Msg::FormatNumbers),
                            Some(l.t(Msg::FormatNumbersNote)),
                            c.format_numbers,
                            p,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.edit(cx, |c| c.format_numbers = !c.format_numbers)
                        })),
                    )
                    .child(
                        toggle_row(
                            "fillers",
                            l.t(Msg::KeepFillers),
                            Some(l.t(Msg::KeepFillersNote)),
                            c.filler_words,
                            p,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.edit(cx, |c| c.filler_words = !c.filler_words)
                        })),
                    )
                    .child(div().w(px(240.)).child(labelled(
                        l.t(Msg::EndpointingLabel),
                        &self.fields.endpointing,
                        p,
                    ))),
            )
    }

    fn vocabulary(&self, p: Palette, cx: &mut Context<Self>) -> Div {
        let l = lang(cx);
        let terms = self.hub.read(cx).snap.config.keyterms.clone();
        div().flex().flex_col().child(
            group(tf!(l, TermsCount, n = terms.len()), p)
                .child(hint(l.t(Msg::TermsHint), p))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(div().flex_1().child(self.fields.keyterm.clone()))
                        .child(
                            button("add-term", l.t(Msg::Add), Kind::Secondary, p)
                                .on_click(cx.listener(|this, _, _, cx| this.add_keyterm(cx))),
                        ),
                )
                .child(div().flex().flex_wrap().gap(px(6.)).children(
                    terms.into_iter().enumerate().map(|(i, term)| {
                        div()
                            .flex()
                            .items_center()
                            .gap(px(2.))
                            .pl(px(10.))
                            .pr(px(2.))
                            .h(px(28.))
                            .rounded(px(8.))
                            .bg(p.raised)
                            .border_1()
                            .border_color(p.line)
                            .child(term.clone())
                            .child(icon_button(("rm-term", i), Icon::Close, p).on_click(
                                cx.listener(move |this, _, _, cx| {
                                    let term = term.clone();
                                    this.edit(cx, |c| c.keyterms.retain(|t| *t != term));
                                }),
                            ))
                    }),
                )),
        )
    }

    fn corrections(&self, p: Palette, cx: &mut Context<Self>) -> Div {
        let l = lang(cx);
        let list = self.hub.read(cx).snap.corrections.clone();
        div()
            .flex()
            .flex_col()
            .child(hint(l.t(Msg::CorrectionsIntro), p).mt(px(6.)))
            .child(
                group(l.t(Msg::AddManually), p).child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(div().flex_1().child(self.fields.corr_from.clone()))
                        .child(div().text_color(p.text_2).child("→"))
                        .child(div().flex_1().child(self.fields.corr_to.clone()))
                        .child(
                            button("remember", l.t(Msg::Remember), Kind::Secondary, p)
                                .on_click(cx.listener(|this, _, _, cx| this.remember_manual(cx))),
                        ),
                ),
            )
            .child(
                group(tf!(l, LearnedCount, n = list.len()), p)
                    .when(list.is_empty(), |d| {
                        d.child(hint(l.t(Msg::NoCorrections), p))
                    })
                    .children(list.into_iter().map(|c| {
                        let id = c.id;
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .py(px(6.))
                            .border_b_1()
                            .border_color(p.line)
                            .child(div().flex_1().text_color(p.text_2).child(c.from))
                            .child(div().text_color(p.text_2).child("→"))
                            .child(div().flex_1().font_weight(FontWeight::MEDIUM).child(c.to))
                            .child(
                                button(("forget", id as usize), l.t(Msg::Forget), Kind::Ghost, p)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.send(cx, Command::ForgetCorrection(id))
                                    })),
                            )
                    })),
            )
    }

    fn microphone(&self, p: Palette, cx: &mut Context<Self>) -> Div {
        let l = lang(cx);
        let snap = &self.hub.read(cx).snap;
        let selected = snap.config.microphone.clone();
        let devices = snap.devices.clone();
        let missing = selected
            .clone()
            .filter(|id| !devices.iter().any(|d| &d.id == id));
        let mic_error = snap
            .error
            .clone()
            .filter(|_| ErrorOrigin::of(snap) == Some(ErrorOrigin::Microphone));
        let saves_audio = snap.config.saves_audio();
        div()
            .flex()
            .flex_col()
            .when_some(mic_error, |d, e| {
                d.child(
                    error_block(
                        ErrorOrigin::Microphone.title(l),
                        &ErrorOrigin::Microphone.body(&e),
                        p,
                    )
                    .mt(px(14.)),
                )
                .child(hint(l.t(Msg::MicPickHint), p).mt(px(8.)))
            })
            .child(
                group(l.t(Msg::Input), p)
                    .child(
                        radio_row(
                            "mic-default",
                            l.t(Msg::SystemDefault),
                            Some(l.t(Msg::SystemDefaultNote).into()),
                            selected.is_none(),
                            p,
                        )
                        .on_click(
                            cx.listener(|this, _, _, cx| this.edit(cx, |c| c.microphone = None)),
                        ),
                    )
                    .children(devices.into_iter().enumerate().map(|(i, d)| {
                        let id = d.id.clone();
                        let is_sel = selected.as_deref() == Some(d.id.as_str());
                        radio_row(("mic", i), d.name, Some(d.id.into()), is_sel, p).on_click(
                            cx.listener(move |this, _, _, cx| {
                                let id = id.clone();
                                this.edit(cx, |c| c.microphone = Some(id));
                            }),
                        )
                    }))
                    .when_some(missing, |d, id| {
                        d.child(radio_row(
                            "mic-missing",
                            l.t(Msg::MicMissing),
                            Some(tf!(l, MicMissingNote, id = id).into()),
                            true,
                            p,
                        ))
                    })
                    .child(
                        div().flex().child(
                            icon_label_button(
                                "refresh",
                                Icon::Refresh,
                                l.t(Msg::RefreshList),
                                Kind::Secondary,
                                p,
                            )
                            .on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.send(cx, Command::RefreshDevices)
                                }),
                            ),
                        ),
                    ),
            )
            .child(
                hint(
                    l.t(if saves_audio {
                        Msg::MicSavesAudio
                    } else {
                        Msg::MicNoAudio
                    }),
                    p,
                )
                .mt(px(12.)),
            )
    }

    fn shortcuts(&self, p: Palette, cx: &mut Context<Self>) -> Div {
        let l = lang(cx);
        let hyprland = ui_state(cx).session.is_hyprland();
        let config = &self.hub.read(cx).snap.config;
        let mode = config.shortcut_mode;
        let copy = config.copy_on_finish;
        let key = config.toggle_shortcut.clone();
        div()
            .flex()
            .flex_col()
            .child(
                group(l.t(Msg::HowShortcutWorks), p)
                    .child(
                        radio_row(
                            "mode-toggle",
                            l.t(Msg::ModeToggle),
                            Some(l.t(Msg::ModeToggleNote).into()),
                            mode == ShortcutMode::Toggle,
                            p,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.edit(cx, |c| c.shortcut_mode = ShortcutMode::Toggle)
                        })),
                    )
                    .child(
                        radio_row(
                            "mode-hold",
                            l.t(Msg::ModeHold),
                            Some(l.t(Msg::ModeHoldNote).into()),
                            mode == ShortcutMode::Hold,
                            p,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.edit(cx, |c| c.shortcut_mode = ShortcutMode::Hold)
                        })),
                    )
                    .child(
                        div()
                            .flex()
                            .items_start()
                            .gap(px(8.))
                            .child(kbd(key, p))
                            .child(
                                hint(
                                    l.t(match (mode, copy) {
                                        (ShortcutMode::Hold, true) => Msg::HoldCopy,
                                        (ShortcutMode::Hold, false) => Msg::HoldShow,
                                        (ShortcutMode::Toggle, true) => Msg::ToggleCopy,
                                        (ShortcutMode::Toggle, false) => Msg::ToggleShow,
                                    }),
                                    p,
                                )
                                .flex_1()
                                .min_w(px(0.)),
                            ),
                    )
                    .child(hint(l.t(Msg::CopyIsInGeneral), p))
                    .when(mode == ShortcutMode::Hold, |d| {
                        d.child(hint(l.t(Msg::HoldNeedsRelease), p))
                            .children(session_hint(hyprland, l.t(Msg::HyprHoldBinds), p))
                    }),
            )
            .child(
                group(l.t(Msg::Combinations), p)
                    .child(
                        div()
                            .flex()
                            .gap(px(12.))
                            .child(div().flex_1().child(labelled(
                                l.t(match mode {
                                    ShortcutMode::Toggle => Msg::StartOrFinish,
                                    ShortcutMode::Hold => Msg::HoldToTalk,
                                }),
                                &self.fields.toggle_shortcut,
                                p,
                            )))
                            .child(div().flex_1().child(labelled(
                                l.t(Msg::PasteLast),
                                &self.fields.paste_shortcut,
                                p,
                            ))),
                    )
                    .child(div().w(px(260.)).child(labelled(
                        l.t(Msg::PasteKeys),
                        &self.fields.paste_keys,
                        p,
                    )))
                    .child(hint(l.t(Msg::PasteKeysHint), p))
                    .child(hint(l.t(Msg::NoDesktopChanges), p))
                    .children(session_hint(hyprland, l.t(Msg::HyprShortcutBinds), p)),
            )
    }

    fn overlay(&self, p: Palette, cx: &mut Context<Self>) -> Div {
        let l = lang(cx);
        let hyprland = ui_state(cx).session.is_hyprland();
        let hub = self.hub.read(cx);
        let show_idle = hub.snap.config.show_idle_overlay;
        let centered = hub.snap.config.overlay_centered;
        let center = ((self.display_w - OVERLAY_WIDTH) / 2.).round().max(0.);
        let presets: [(Msg, f32, f32); 5] = [
            (Msg::TopLeft, 24., 24.),
            (Msg::TopCentre, center, 24.),
            (Msg::TopRight, -24., 24.),
            (Msg::BottomCentre, center, -48.),
            (Msg::BottomRight, -24., -48.),
        ];
        let (x, y) = (hub.snap.config.overlay_x, hub.snap.config.overlay_y);
        div()
            .flex()
            .flex_col()
            .child(
                group(l.t(Msg::Visibility), p)
                    .child(
                        toggle_row(
                            "idle",
                            l.t(Msg::ShowIdle),
                            Some(l.t(Msg::ShowIdleNote)),
                            show_idle,
                            p,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.edit(cx, |c| c.show_idle_overlay = !c.show_idle_overlay)
                        })),
                    )
                    .child(
                        div().flex().child(
                            button("show-overlay", l.t(Msg::ShowOverlayNow), Kind::Secondary, p)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.send(cx, Command::ShowOverlay)
                                })),
                        ),
                    ),
            )
            .child(
                group(l.t(Msg::Position), p)
                    .child(hint(l.t(Msg::OverlayDragHint), p))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(px(6.))
                            .child(
                                segment("center", l.t(Msg::Centre), centered, p)
                                    .border_1()
                                    .border_color(p.line)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.edit(cx, |c| c.overlay_centered = true)
                                    })),
                            )
                            .children(presets.into_iter().enumerate().map(
                                |(i, (label, px_, py_))| {
                                    let selected =
                                        !centered && (x - px_).abs() < 0.5 && (y - py_).abs() < 0.5;
                                    segment(("preset", i), l.t(label), selected, p)
                                        .border_1()
                                        .border_color(p.line)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.edit(cx, |c| {
                                                c.overlay_centered = false;
                                                c.overlay_x = px_;
                                                c.overlay_y = py_;
                                            })
                                        }))
                                },
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(px(12.))
                            .child(div().w(px(140.)).child(labelled(
                                l.t(Msg::Horizontal),
                                &self.fields.overlay_x,
                                p,
                            )))
                            .child(div().w(px(140.)).child(labelled(
                                l.t(Msg::Vertical),
                                &self.fields.overlay_y,
                                p,
                            ))),
                    )
                    .child(hint(l.t(Msg::OverlayOffsetsHint), p))
                    .child(hint(l.t(Msg::DesktopMayPlace), p))
                    .children(session_hint(
                        hyprland,
                        tf!(l, HyprOverlayRules, class = OVERLAY_APP_ID),
                        p,
                    )),
            )
    }

    fn background(&self, p: Palette, cx: &mut Context<Self>) -> Div {
        let l = lang(cx);
        div().flex().flex_col().child(
            group(l.t(Msg::Windows), p)
                .child(hint(l.t(Msg::BackgroundHint), p))
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(px(8.))
                        .child(
                            button(
                                "close-settings",
                                l.t(Msg::CloseSettings),
                                Kind::Secondary,
                                p,
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
                        )
                        .child(
                            button("quit", l.t(Msg::QuitApp), Kind::Ghost, p).on_click(
                                |_, window, cx| window.dispatch_action(Box::new(Quit), cx),
                            ),
                        ),
                ),
        )
    }
}

/// Name of an audio retention option; `AudioRetention::label` is Spanish.
pub(crate) fn retention_label(r: AudioRetention, l: Lang) -> &'static str {
    l.t(match r {
        AudioRetention::Off => Msg::RetentionOff,
        AudioRetention::Days7 => Msg::Retention7,
        AudioRetention::Days30 => Msg::Retention30,
        AudioRetention::Forever => Msg::RetentionForever,
    })
}

impl Render for Settings {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        perf::bump(&perf::SETTINGS_RENDERS);
        let p = ui_state(cx).palette;
        let l = lang(cx);
        // Position presets measure against the display showing this window.
        self.display_w = window
            .display(cx)
            .map(|d| f32::from(d.bounds().size.width))
            .unwrap_or_else(|| crate::display_bounds(cx).size.width);
        if let Some(section) = self.hub.update(cx, |hub, _| hub.settings_section.take()) {
            self.section = section;
        }
        let demo = self.hub.read(cx).snap.demo;
        let viewport = window.viewport_size();
        let (vw, vh) = (f32::from(viewport.width), f32::from(viewport.height));
        let column = (vw - NAV_W - 1. - CONTENT_PAD_X * 2.).max(0.);
        let content = match self.section {
            Section::Home => self.home(p, column, cx).max_w(px(DASHBOARD_W)),
            Section::History => self.history(p, (vw - NAV_W - 1.).max(0.), cx),
            Section::Appearance => self.appearance(p, column, cx).max_w(px(COLUMN_W)),
            Section::Data => self.data(p, cx).max_w(px(COLUMN_W)),
            Section::General => self.general(p, cx),
            Section::Provider => self.provider(p, cx),
            Section::Writing => self.writing(p, cx),
            Section::Vocabulary => self.vocabulary(p, cx),
            Section::Corrections => self.corrections(p, cx),
            Section::Microphone => self.microphone(p, cx),
            Section::Shortcuts => self.shortcuts(p, cx),
            Section::Overlay => self.overlay(p, cx),
            Section::Background => self.background(p, cx),
            Section::Reading => self.reading(p, column, cx),
            Section::Writer => self.writer(p, cx),
        };
        let content = if matches!(
            self.section,
            Section::Home | Section::History | Section::Appearance | Section::Data
        ) {
            content
        } else {
            content.max_w(px(COLUMN_W))
        };
        let error = self.error.clone();
        let snap = &self.hub.read(cx).snap;
        let origin = ErrorOrigin::of(snap);
        let backend_error = global_backend_error(
            self.section,
            snap.error.as_deref(),
            origin,
            self.dismissed_backend_error.as_deref(),
        )
        .map(str::to_string);
        let origin = origin.unwrap_or(ErrorOrigin::App);
        let notice = self.notice.clone();
        let section = self.section;
        div()
            .size_full()
            .flex()
            .bg(p.surface)
            .text_color(p.text)
            .font_family(UI_FONT)
            .text_size(px(14.))
            .on_action(cx.listener(|this, _: &CloseWindow, _, cx| this.close(cx)))
            .child(self.nav(p, vh, cx))
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_col()
                    .flex_1()
                    // Unwrapped text sets the min-content width; without
                    // this the pane grows past a tiled window's edge.
                    .min_w(px(0.))
                    .h_full()
                    .overflow_hidden()
                    // Clean reading surface; Regolito keeps its soft grain.
                    .when(p.coarse_grain, |d| {
                        d.child(grain(p, (vw - NAV_W).max(0.), vh))
                    })
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .items_start()
                            .gap(px(12.))
                            .px(px(CONTENT_PAD_X))
                            .pt(px(22.))
                            .pb(px(14.))
                            .border_b_1()
                            .border_color(p.line)
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .flex()
                                    .flex_col()
                                    .gap(px(3.))
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap(px(10.))
                                            .font_family(DISPLAY_FONT)
                                            .font_weight(FontWeight::BOLD)
                                            .text_size(px(23.))
                                            .line_height(px(28.))
                                            .child(section.label(l))
                                            .when(demo, |d| d.child(demo_badge(p))),
                                    )
                                    .child(hint(section.subtitle(l), p).text_size(px(13.))),
                            )
                            .child(
                                icon_button("close", Icon::Close, p)
                                    .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
                            ),
                    )
                    .child(
                        div()
                            .id("content")
                            .flex_1()
                            .min_h(px(0.))
                            .flex()
                            .flex_col()
                            .when(!section.full_bleed(), |d| {
                                d.overflow_y_scroll()
                                    .px(px(CONTENT_PAD_X))
                                    .pt(px(4.))
                                    .pb(px(28.))
                            })
                            .when_some(error, |d, e| {
                                d.child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(10.))
                                        .mt(px(14.))
                                        .max_w(px(COLUMN_W))
                                        .m(px(if section.full_bleed() { 14. } else { 0. }))
                                        .p(px(10.))
                                        .rounded(px(10.))
                                        .bg(p.raised)
                                        .border_1()
                                        .border_color(p.danger)
                                        .child(icon(Icon::Alert, 16., p.danger))
                                        .child(div().flex_1().child(e))
                                        .child(
                                            icon_button("dismiss-error", Icon::Close, p).on_click(
                                                cx.listener(|this, _, _, cx| {
                                                    this.error = None;
                                                    cx.notify();
                                                }),
                                            ),
                                        ),
                                )
                            })
                            .when_some(backend_error, |d, e| {
                                let dismissed = e.clone();
                                let home = inline_section(origin);
                                let action = home.map(|home| {
                                    icon_label_button(
                                        "go-home",
                                        home.icon(),
                                        tf!(l, GoTo, section = home.label(l)),
                                        Kind::Ghost,
                                        p,
                                    )
                                    .on_click(cx.listener(move |this, _, _, cx| this.go(home, cx)))
                                    .into_any_element()
                                });
                                d.child(
                                    error_block_with(origin.title(l), &origin.body(&e), action, p)
                                        .relative()
                                        .mt(px(14.))
                                        .when(section.full_bleed(), |d| d.mx(px(CONTENT_PAD_X)))
                                        .max_w(px(COLUMN_W))
                                        .pr(px(44.))
                                        .child(
                                            div().absolute().top(px(8.)).right(px(8.)).child(
                                                icon_button(
                                                    "dismiss-backend-error",
                                                    Icon::Close,
                                                    p,
                                                )
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.dismissed_backend_error =
                                                        Some(dismissed.clone());
                                                    cx.notify();
                                                })),
                                            ),
                                        ),
                                )
                            })
                            .when_some(notice, |d, n| {
                                d.child(
                                    div()
                                        .mt(px(14.))
                                        .flex()
                                        .items_center()
                                        .gap(px(8.))
                                        .child(icon(Icon::Check, 15., p.text_2))
                                        .child(hint(n, p)),
                                )
                            })
                            .child(content),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_accept_comma_and_negatives() {
        let l = Lang::Es;
        assert_eq!(parse_offset(" -48 ", l), Ok(-48.));
        assert_eq!(parse_offset("12,5", l), Ok(12.5));
        assert_eq!(parse_offset("12.5", Lang::En), Ok(12.5));
        assert!(parse_offset("abajo", l).is_err());
        assert!(parse_offset("NaN", l).is_err());
        assert!(
            parse_offset("down", Lang::En)
                .unwrap_err()
                .contains("pixels")
        );
    }

    /// Width inside a group card for a settings window `width` px wide
    /// (nav border and the card's own border included).
    fn card_inner_width(width: f32) -> f32 {
        width - NAV_W - 1. - CONTENT_PAD_X * 2. - GROUP_PAD * 2. - 2.
    }

    #[test]
    fn three_style_cards_fit_side_by_side_in_the_default_window() {
        assert_eq!(Theme::ALL.len(), 3);
        let three = THEME_CARD_MIN_W * 3. + THEME_CARD_GAP * 2.;
        // 1040 px default window: one row of three.
        assert!(three <= card_inner_width(1040.).min(COLUMN_W - GROUP_PAD * 2. - 2.));
        // The minimum window falls back to one per row without overflow.
        assert!(THEME_CARD_MIN_W <= card_inner_width(crate::MAIN_MIN_W));
    }

    fn test_hub(snap: dicta_core::AppSnapshot) -> Hub {
        Hub::new(snap, std::sync::mpsc::channel().0)
    }

    fn demo_note(text: &str) -> dicta_core::Note {
        dicta_core::Note {
            id: 7,
            created_at_ms: 1,
            original: text.into(),
            text: text.into(),
            word_count: 2,
            captured_ms: 1000,
            sent_ms: 900,
            provider: ProviderMode::Subscription,
            status: dicta_core::NoteStatus::Completed,
            error: None,
            audio_filename: None,
            audio_bytes: 0,
            input_tokens: None,
            output_tokens: None,
        }
    }

    #[test]
    fn digest_sees_same_length_edits_and_ignores_audio_levels() {
        let mut snap = dicta_core::AppSnapshot::new(AppConfig::default());
        snap.notes = vec![demo_note("a b")].into();
        snap.selected_note = Some(demo_note("a b"));
        let mut hub = test_hub(snap);
        let before = digest(&hub);

        // Audio Level events: new level and sequence, nothing the window shows.
        hub.snap.level = 0.4;
        hub.snap.level_seq += 1;
        hub.snap.elapsed_ms += 100;
        hub.snap.partial = "hola".into();
        hub.levels
            .push(hub.snap.level_seq, 0.4, std::time::Instant::now());
        assert_eq!(digest(&hub), before);

        // Same length, same word count, different text.
        std::sync::Arc::make_mut(&mut hub.snap.notes)[0].text = "c d".into();
        let edited = digest(&hub);
        assert_ne!(edited, before);
        hub.snap.selected_note.as_mut().unwrap().text = "c d".into();
        assert_ne!(digest(&hub), edited);

        let base = digest(&hub);
        std::sync::Arc::make_mut(&mut hub.snap.notes)[0].original = "x y".into();
        assert_ne!(digest(&hub), base);
        let base = digest(&hub);
        std::sync::Arc::make_mut(&mut hub.snap.notes)[0].error = Some("e".into());
        assert_ne!(digest(&hub), base);
        let base = digest(&hub);
        std::sync::Arc::make_mut(&mut hub.snap.notes)[0].output_tokens = Some(3);
        assert_ne!(digest(&hub), base);
    }

    #[test]
    fn digest_ignores_reading_progress_but_sees_readings() {
        let mut snap = dicta_core::AppSnapshot::new(AppConfig::default());
        snap.tts.phase = dicta_core::TtsPhase::Playing;
        let mut hub = test_hub(snap);
        let before = digest(&hub);
        hub.snap.tts.position_ms = 12_000;
        hub.snap.tts.current_word = Some(40);
        hub.snap.tts.level_seq += 1;
        hub.snap.tts.chars_sent = 500;
        assert_eq!(digest(&hub), before, "the main window never shows progress");
        hub.snap.readings = std::sync::Arc::new(vec![dicta_core::Reading {
            id: 1,
            created_at_ms: 1,
            text: "Hola".into(),
            chars: 4,
            chars_sent: 4,
            duration_ms: 300,
            voice: "eve".into(),
            language: "auto".into(),
            provider: ProviderMode::Subscription,
            status: dicta_core::NoteStatus::Completed,
            error: None,
            audio_filename: None,
            audio_bytes: 0,
        }]);
        let listed = digest(&hub);
        assert_ne!(listed, before);
        std::sync::Arc::make_mut(&mut hub.snap.readings)[0].audio_filename = Some("a.wav".into());
        assert_ne!(digest(&hub), listed);
    }

    #[test]
    fn connect_works_with_empty_client_id() {
        // Empty config: no manual client ID, the button still starts login.
        assert!(AppConfig::default().oauth_client_id.trim().is_empty());
        assert!(matches!(connect_command(), Command::StartLogin));
        for l in Lang::ALL {
            assert!(!l.t(SUBSCRIPTION_HINT).contains("ID"));
        }
    }

    #[test]
    fn backend_errors_show_inline_in_their_section_and_global_elsewhere() {
        let e = "El inicio de sesión fue rechazado.\nVolvé a conectar con Grok.";
        let cred = Some(ErrorOrigin::Credential);
        assert_eq!(
            global_backend_error(Section::Provider, Some(e), cred, None),
            None
        );
        assert_eq!(
            global_backend_error(Section::General, Some(e), cred, None),
            Some(e)
        );
        // Dismissing hides that error only; a new one shows again.
        assert_eq!(
            global_backend_error(Section::General, Some(e), cred, Some(e)),
            None
        );
        assert_eq!(
            global_backend_error(Section::Writing, Some("Otro error"), cred, Some(e)),
            Some("Otro error")
        );
        assert_eq!(
            global_backend_error(Section::General, None, None, None),
            None
        );
        // A microphone failure lives in Micrófono and is a banner on
        // Proveedor, never mistaken for a credential problem there.
        let mic = "No se pudo capturar audio: El micrófono se desconectó.";
        let origin = Some(ErrorOrigin::Microphone);
        assert_eq!(
            global_backend_error(Section::Microphone, Some(mic), origin, None),
            None
        );
        assert_eq!(
            global_backend_error(Section::Provider, Some(mic), origin, None),
            Some(mic)
        );
        // Errors without a home section are global everywhere.
        let net = Some(ErrorOrigin::Dictation);
        assert_eq!(
            global_backend_error(Section::Provider, Some("Sin red"), net, None),
            Some("Sin red")
        );
    }

    #[test]
    fn the_main_window_opens_on_inicio_and_lists_history_first() {
        assert_eq!(&NAV_GROUPS[0].1[..2], &[Section::Home, Section::History]);
        assert!(
            NAV_GROUPS[0].1.contains(&Section::Reading),
            "Lectura is a page of its own"
        );
        assert!(
            NAV_GROUPS[0].1.contains(&Section::Writer),
            "Escritura is a page of its own"
        );
        assert!(Section::History.full_bleed() && !Section::Home.full_bleed());
    }

    #[test]
    fn sidebar_reaches_every_section_once() {
        let listed: Vec<Section> = NAV_GROUPS
            .iter()
            .flat_map(|(_, sections)| sections.iter().copied())
            .collect();
        assert_eq!(listed.len(), Section::ALL.len());
        for s in Section::ALL {
            assert_eq!(listed.iter().filter(|x| **x == s).count(), 1, "{s:?}");
        }
    }

    #[test]
    fn endpointing_is_bounded_like_config_validation() {
        assert_eq!(parse_endpointing("400", Lang::Es), Ok(400));
        assert!(parse_endpointing("5001", Lang::Es).is_err());
        assert!(parse_endpointing("-1", Lang::En).is_err());
    }

    #[test]
    fn the_subscription_needs_consent_once_and_api_key_comes_first() {
        let mut c = AppConfig::default();
        assert_eq!(
            c.provider,
            ProviderMode::ApiKey,
            "public builds start on the API key"
        );
        assert!(!c.subscription_acknowledged);
        // Accepting sets the mode and the acknowledgement in one config.
        c.subscription_acknowledged = true;
        subscribe(&mut c, Consent::Dictation);
        assert_eq!(c.provider, ProviderMode::Subscription);
        assert_eq!(
            c.tts.provider,
            ProviderMode::ApiKey,
            "reading keeps its own mode"
        );
        subscribe(&mut c, Consent::Reading);
        assert_eq!(c.tts.provider, ProviderMode::Subscription);
        assert_eq!(
            c.writer.provider,
            ProviderMode::ApiKey,
            "Escritura keeps its own mode"
        );
        subscribe(&mut c, Consent::Writer);
        assert_eq!(c.writer.provider, ProviderMode::Subscription);
        c.validate().unwrap();
    }
}
