//! Native GPUI interface for Mars voice (internally Dicta): a small dictation
//! overlay and the main window (Inicio, Historial, Ajustes). The UI owns no audio, network or credentials. It renders the latest
//! [`AppSnapshot`] and turns user actions into [`Command`]s for the controller.
//! See `docs/UI-NATIVE.md` for the overlay placement rules and limitations.

mod i18n;

mod assets;
// The headless wgpu renderer of the gallery and the perf harness is only
// wired for Linux here; other targets build the app without them.
#[cfg(all(feature = "screenshots", target_os = "linux"))]
pub mod bench;
mod corrections;
mod errors;
mod format;
#[cfg(all(feature = "screenshots", target_os = "linux"))]
pub mod gallery;
mod history;
mod home;
mod lectura;
mod overlay;
pub mod perf;
mod reader;
pub mod session;
mod settings;
mod text_field;
mod theme;
mod wave;
mod widgets;

use std::{
    sync::{
        OnceLock,
        mpsc::{Receiver, Sender},
    },
    time::Instant,
};

use dicta_core::{AppConfig, AppSnapshot, ColorMode, Command, Phase, Theme, TtsSnapshot};
use futures::StreamExt;
use gpui::{
    App, AppContext, Bounds, Entity, Global, KeyBinding, Pixels, Point, Size, TitlebarOptions,
    WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowKind, WindowOptions, actions,
    point, px, size,
};

pub use crate::i18n::Lang;
use crate::{
    i18n::Msg,
    overlay::Overlay,
    reader::Reader,
    session::Session,
    settings::{Section, Settings},
    theme::{Palette, VoiceStyle},
    wave::LevelHistory,
};

actions!(dicta, [Quit, CloseWindow]);

/// Window kind of the overlay and the reader. Linux keeps a floating
/// toplevel that compositor rules place and the user drags. macOS and
/// Windows get `PopUp`: a non-activating NSPanel above other apps, or a
/// topmost tool window without a taskbar button, so opening it never takes
/// focus from the app being dictated into or read from.
#[cfg(any(target_os = "macos", target_os = "windows"))]
const FLOATING_KIND: WindowKind = WindowKind::PopUp;
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const FLOATING_KIND: WindowKind = WindowKind::Floating;

pub const OVERLAY_WIDTH: f32 = 404.;
/// Tallest card (Ready). The window has no margin around the card, so the
/// compositor never has a large transparent area to blur or shade.
pub const OVERLAY_MAX_HEIGHT: f32 = 318.;
/// Wayland app_id / X11 class that compositor rules match.
pub const OVERLAY_APP_ID: &str = "dicta-overlay";
/// Name shown to the user. The app id, binary and paths stay "dicta".
pub const APP_NAME: &str = "Mars voice";
/// Narrowest main window; every section lays out without hidden controls.
pub const MAIN_MIN_W: f32 = 720.;
/// Desktop id of the main window; kept for launcher and compositor rules.
pub const APP_ID: &str = "io.github.francoescob.Dicta";
/// Read-aloud window: a floating card like the overlay, wider for the
/// transport. Its tallest card (with text) is [`READER_MAX_HEIGHT`].
pub const READER_WIDTH: f32 = 440.;
pub const READER_MAX_HEIGHT: f32 = 440.;
/// Wayland app_id / X11 class of the read-aloud window, separate from
/// [`OVERLAY_APP_ID`] so both can be open and ruled independently.
pub const READER_APP_ID: &str = "dicta-reader";

/// Hyprland 0.56 ignores client-side resizes of floating toplevels (checked
/// live: a 404x100 window stayed 404x100 after `resize`), so on Wayland the
/// window keeps the tallest card height and the card changes height inside
/// it. The input region is limited to the card, so the transparent rest lets
/// clicks through. Input regions only exist on Wayland: X11, Windows and
/// macOS size the window to the card instead and resize it per phase.
pub fn fixed_surface() -> bool {
    cfg!(not(any(target_os = "macos", target_os = "windows")))
        && std::env::var_os("WAYLAND_DISPLAY").is_some()
}

/// Read-mostly view state shared by every window.
pub struct UiState {
    pub palette: Palette,
    pub reduce_motion: bool,
    /// Interface language now on screen.
    pub lang: Lang,
    /// Language of the system locale, what `ui_language = "auto"` means.
    pub system_lang: Lang,
    /// Desktop the copy adapts its hints to.
    pub session: Session,
}
impl Global for UiState {}

impl UiState {
    pub fn new(palette: Palette, lang: Lang) -> Self {
        Self {
            palette,
            reduce_motion: false,
            lang,
            system_lang: lang,
            session: Session::detect(),
        }
    }
}

pub fn ui_state(cx: &App) -> &UiState {
    cx.global::<UiState>()
}

/// The interface language now on screen.
pub(crate) fn lang(cx: &App) -> Lang {
    cx.global::<UiState>().lang
}

/// Where the overlay window opens. It is a borderless floating toplevel
/// (`app_id` [`OVERLAY_APP_ID`]) that the user drags natively from the card
/// header; a drag is never undone by later snapshots or phase changes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OverlayPlacement {
    pub centered: bool,
    /// Offsets in logical pixels, see [`overlay_origin`]. Zero when centered,
    /// so editing hidden offsets does not reopen a centered overlay.
    pub x: f32,
    pub y: f32,
}

impl OverlayPlacement {
    pub fn from_config(config: &AppConfig) -> Self {
        if config.overlay_centered {
            Self {
                centered: true,
                x: 0.,
                y: 0.,
            }
        } else {
            Self {
                centered: false,
                x: config.overlay_x,
                y: config.overlay_y,
            }
        }
    }

    /// Placement of the read-aloud window, from `config.tts`.
    pub fn for_reader(config: &AppConfig) -> Self {
        let t = &config.tts;
        if t.window_centered {
            Self {
                centered: true,
                x: 0.,
                y: 0.,
            }
        } else {
            Self {
                centered: false,
                x: t.window_x,
                y: t.window_y,
            }
        }
    }

    /// Bottom-anchored offsets keep the card glued to the bottom of a
    /// fixed-height window, so it grows upwards like the configured edge.
    pub fn card_at_bottom(self) -> bool {
        !self.centered && self.y < 0.
    }
}

type Placer = Box<dyn Fn(Point<f32>) + Send + Sync>;
static PLACER: OnceLock<Placer> = OnceLock::new();

/// Lets the host move the overlay where clients cannot place their own
/// windows (Wayland). Called right after the overlay opens with
/// `overlay_centered = false`, with the top-left corner in logical pixels
/// relative to the monitor; centered overlays rely on the compositor rule.
/// Only the first call has effect.
pub fn set_overlay_placer(placer: impl Fn(Point<f32>) + Send + Sync + 'static) {
    let _ = PLACER.set(Box::new(placer));
}

static READER_PLACER: OnceLock<Placer> = OnceLock::new();

/// Like [`set_overlay_placer`] for the read-aloud window ([`READER_APP_ID`]):
/// called right after it opens with `tts.window_centered = false`, with the
/// top-left corner of a [`READER_WIDTH`]×[`READER_MAX_HEIGHT`] surface in
/// logical pixels relative to the monitor. Only the first call has effect.
pub fn set_reader_placer(placer: impl Fn(Point<f32>) + Send + Sync + 'static) {
    let _ = READER_PLACER.set(Box::new(placer));
}

/// Latest controller state plus UI-only bookkeeping.
pub struct Hub {
    pub snap: AppSnapshot,
    tx: Sender<Command>,
    pub levels: LevelHistory,
    /// Output levels of the reading being played, for the reader's voice.
    pub tts_levels: LevelHistory,
    /// Style and colour mode chosen in the UI but not yet confirmed by a
    /// snapshot.
    pub pending_theme: Option<Theme>,
    pub pending_mode: Option<ColorMode>,
    settings_dismissed: bool,
    overlay: Option<(WindowHandle<Overlay>, OverlayPlacement, f32)>,
    reader: Option<(WindowHandle<Reader>, OverlayPlacement, f32)>,
    settings: Option<WindowHandle<Settings>>,
    /// Section the next settings render should show, e.g. Micrófono after
    /// the overlay's "Elegir micrófono". Consumed by `Settings`.
    pub(crate) settings_section: Option<Section>,
}

impl Hub {
    pub(crate) fn new(snap: AppSnapshot, tx: Sender<Command>) -> Self {
        Self {
            snap,
            tx,
            levels: LevelHistory::default(),
            tts_levels: LevelHistory::default(),
            pending_theme: None,
            pending_mode: None,
            settings_dismissed: false,
            overlay: None,
            reader: None,
            settings: None,
            settings_section: None,
        }
    }

    pub fn send(&self, command: Command) {
        // A closed channel means the controller is shutting down; the
        // snapshot stream ends right after and the UI quits with it.
        let _ = self.tx.send(command);
    }

    /// Style shown now; legacy Night is shown as Señal.
    pub fn theme(&self) -> Theme {
        match self.pending_theme.unwrap_or(self.snap.config.theme) {
            Theme::Night => Theme::Signal,
            t => t,
        }
    }

    /// Colour mode shown now; a legacy Night configuration means dark.
    pub fn color_mode(&self) -> ColorMode {
        match (self.pending_mode, self.snap.config.theme) {
            (Some(mode), _) => mode,
            (None, Theme::Night) => ColorMode::Dark,
            (None, _) => self.snap.config.color_mode,
        }
    }

    pub fn palette(&self) -> Palette {
        Palette::new(self.theme(), self.color_mode())
    }

    /// Puts the configured interface language on screen. A change refreshes
    /// every window, cached views included, like a palette change.
    fn apply_language(&self, cx: &mut App) {
        let state = cx.global::<UiState>();
        let lang = Lang::resolve(&self.snap.config.ui_language, state.system_lang);
        if state.lang != lang {
            cx.global_mut::<UiState>().lang = lang;
            cx.refresh_windows();
        }
    }

    pub fn set_theme(&mut self, theme: Theme, cx: &mut gpui::Context<Self>) {
        self.pending_theme = Some(theme);
        self.send(Command::SetTheme(theme));
        apply_palette(self.palette(), cx);
        cx.notify();
    }

    pub fn set_color_mode(&mut self, mode: ColorMode, cx: &mut gpui::Context<Self>) {
        self.pending_mode = Some(mode);
        self.send(Command::SetColorMode(mode));
        apply_palette(self.palette(), cx);
        cx.notify();
    }

    /// Validates and sends a configuration change, applying it locally until
    /// the controller confirms it with the next snapshot.
    pub fn update_config(
        &mut self,
        cx: &mut gpui::Context<Self>,
        edit: impl FnOnce(&mut AppConfig),
    ) -> Result<(), String> {
        let mut config = self.snap.config.clone();
        edit(&mut config);
        if config == self.snap.config {
            return Ok(());
        }
        config.validate().map_err(|e| e.to_string())?;
        self.snap.config = config.clone();
        self.send(Command::UpdateConfig(config));
        self.apply_language(cx);
        cx.notify();
        Ok(())
    }

    /// Placement of the open overlay, None while hidden.
    pub fn overlay_placement(&self) -> Option<OverlayPlacement> {
        self.overlay.map(|(_, placement, _)| placement)
    }

    /// Hides settings immediately and tells the controller, so a later
    /// `ShowSettings` (tray, `dicta ctl settings`) reopens the window.
    /// Opens (or raises) settings on `section`.
    pub(crate) fn show_settings_at(&mut self, section: Section, cx: &mut gpui::Context<Self>) {
        self.settings_section = Some(section);
        self.settings_dismissed = false;
        self.send(Command::ShowSettings);
        cx.notify();
    }

    pub fn close_settings(&mut self, cx: &mut gpui::Context<Self>) {
        self.settings_dismissed = true;
        self.send(Command::HideSettings);
        cx.notify();
    }

    /// `at` is when the snapshot left the controller channel, so readings
    /// coalesced into one UI update keep their own times.
    ///
    /// GPUI redraws every window that read the hub during its last frame
    /// whenever the hub notifies. While recording, the controller publishes
    /// up to 50 snapshots per second that only carry audio levels, the clock
    /// and the live transcript. Those must not redraw the main window, so
    /// they are applied without notifying the hub: only the overlay is
    /// invalidated, and not even that when nothing visible changed (silence
    /// with the envelope at rest and the same second on the timer).
    fn apply(&mut self, snap: AppSnapshot, at: Instant, cx: &mut gpui::Context<Self>) {
        perf::bump(&perf::HUB_APPLIES);
        if snap.phase == Phase::Recording && self.snap.phase != Phase::Recording {
            self.levels.clear();
        }
        let mut level_added = false;
        if snap.phase == Phase::Recording {
            // Only audio Level events bump `level_seq`; snapshots sent for
            // partials or the clock repeat it and add nothing.
            level_added = self.levels.push(snap.level_seq, snap.level, at);
        }
        if snap.tts.reading != self.snap.tts.reading {
            self.tts_levels.clear();
        }
        let mut tts_level_added = false;
        if snap.tts.phase == dicta_core::TtsPhase::Playing {
            tts_level_added = self.tts_levels.push(snap.tts.level_seq, snap.tts.level, at);
        }
        self.settings_dismissed = settings_still_dismissed(
            self.settings_dismissed,
            self.snap.show_settings,
            snap.show_settings,
        );
        if self.pending_theme == Some(snap.config.theme) {
            self.pending_theme = None;
        }
        if self.pending_mode == Some(snap.config.color_mode) && snap.config.theme != Theme::Night {
            self.pending_mode = None;
        }
        let live_only = same_main_state(&self.snap, &snap);
        let overlay_visible =
            live_change_visible(&self.snap, &snap) || (level_added && !self.levels.is_silent(at));
        // Position, word, generation and output level only move the reader.
        let reader_visible = !same_reader_state(&self.snap.tts, &snap.tts)
            || (tts_level_added && !self.tts_levels.is_silent(at));
        let before = (self.theme(), self.color_mode());
        self.snap = snap;
        if (self.theme(), self.color_mode()) != before {
            apply_palette(self.palette(), cx);
        }
        // Configuration changes are never live-only, so this runs before
        // the main window is notified.
        if !live_only {
            self.apply_language(cx);
        }
        if !live_only {
            perf::bump(&perf::HUB_NOTIFIES);
            cx.notify();
            return;
        }
        if overlay_visible {
            perf::bump(&perf::LIVE_TICKS);
            self.tick_overlay(cx);
        }
        if reader_visible {
            perf::bump(&perf::READER_TICKS);
            self.tick_reader(cx);
        }
        if !overlay_visible && !reader_visible {
            perf::bump(&perf::SKIPPED_TICKS);
        }
    }

    /// Redraws the overlay (and nothing else) for a live-only snapshot.
    fn tick_overlay(&self, cx: &mut App) {
        if let Some((handle, _, _)) = self.overlay {
            handle.update(cx, |_, _, cx| cx.notify()).ok();
        }
    }

    /// Redraws the reader (and nothing else) while a reading plays; its
    /// text only when the sentence or word changed.
    fn tick_reader(&self, cx: &mut App) {
        if let Some((handle, _, _)) = self.reader {
            let marks = reader::Marks::of(&self.snap.tts);
            handle
                .update(cx, |reader, _, cx| reader.tick(marks, cx))
                .ok();
        }
    }
}

/// True when `a` and `b` differ at most in the live fields: audio level and
/// its sequence, the clock, the revision and the live transcript. Everything
/// the main window or window management can show is compared; collections
/// without `PartialEq` are compared field by field. The shared notes are
/// first compared by identity (the controller keeps the collection between
/// publications and replaces it when it changes), then by content.
fn same_main_state(a: &AppSnapshot, b: &AppSnapshot) -> bool {
    fn note_eq(x: &dicta_core::Note, y: &dicta_core::Note) -> bool {
        x.id == y.id
            && x.created_at_ms == y.created_at_ms
            && x.word_count == y.word_count
            && x.captured_ms == y.captured_ms
            && x.sent_ms == y.sent_ms
            && x.audio_bytes == y.audio_bytes
            && x.input_tokens == y.input_tokens
            && x.output_tokens == y.output_tokens
            && x.audio_filename == y.audio_filename
            && x.error == y.error
            && std::mem::discriminant(&x.status) == std::mem::discriminant(&y.status)
            && std::mem::discriminant(&x.provider) == std::mem::discriminant(&y.provider)
            && x.text == y.text
            && x.original == y.original
    }
    let m = (&a.metrics, &b.metrics);
    a.phase == b.phase
        && a.show_settings == b.show_settings
        && a.show_overlay == b.show_overlay
        && a.authenticated == b.authenticated
        && a.demo == b.demo
        && a.status == b.status
        && a.error == b.error
        && a.final_text == b.final_text
        && a.original_text == b.original_text
        && a.config == b.config
        && (std::sync::Arc::ptr_eq(&a.notes, &b.notes)
            || (a.notes.len() == b.notes.len()
                && a.notes
                    .iter()
                    .zip(b.notes.iter())
                    .all(|(x, y)| note_eq(x, y))))
        && a.notes_limit == b.notes_limit
        && a.notes_has_more == b.notes_has_more
        && a.history_query == b.history_query
        && a.active_note_id == b.active_note_id
        && a.audio_playing_note_id == b.audio_playing_note_id
        && std::mem::discriminant(&a.usage_period) == std::mem::discriminant(&b.usage_period)
        && m.0.dictations == m.1.dictations
        && m.0.words == m.1.words
        && m.0.captured_ms == m.1.captured_ms
        && m.0.sent_ms == m.1.sent_ms
        && m.0.estimated_stt_usd.to_bits() == m.1.estimated_stt_usd.to_bits()
        && m.0.input_tokens == m.1.input_tokens
        && m.0.output_tokens == m.1.output_tokens
        && m.0.audio_bytes == m.1.audio_bytes
        && match (&a.selected_note, &b.selected_note) {
            (None, None) => true,
            (Some(x), Some(y)) => note_eq(x, y),
            _ => false,
        }
        && match (&a.login, &b.login) {
            (None, None) => true,
            (Some(x), Some(y)) => {
                x.verification_uri == y.verification_uri && x.user_code == y.user_code
            }
            _ => false,
        }
        && a.devices.len() == b.devices.len()
        && a.devices
            .iter()
            .zip(&b.devices)
            .all(|(x, y)| x.id == y.id && x.name == y.name)
        && a.corrections.len() == b.corrections.len()
        && a.corrections
            .iter()
            .zip(&b.corrections)
            .all(|(x, y)| x.id == y.id && x.from == y.from && x.to == y.to)
        && same_tts_main_state(&a.tts, &b.tts)
        && same_readings(a, b)
}

/// What of the reading the main window and window management use: whether
/// the reader shows and its card (phase, messages), the voices and outputs
/// of Ajustes → Lectura and the account state.
fn same_tts_main_state(a: &TtsSnapshot, b: &TtsSnapshot) -> bool {
    a.show == b.show
        && a.phase == b.phase
        && reader::has_audio(a) == reader::has_audio(b)
        && a.error == b.error
        && a.notice == b.notice
        && a.confirm == b.confirm
        && a.account_ready == b.account_ready
        && a.previewing_voice == b.previewing_voice
        && (std::sync::Arc::ptr_eq(&a.voices, &b.voices) || a.voices == b.voices)
        && a.outputs.len() == b.outputs.len()
        && a.outputs
            .iter()
            .zip(&b.outputs)
            .all(|(x, y)| x.id == y.id && x.name == y.name)
}

/// The readings history and its metrics.
fn same_readings(a: &AppSnapshot, b: &AppSnapshot) -> bool {
    fn reading_eq(x: &dicta_core::Reading, y: &dicta_core::Reading) -> bool {
        x.id == y.id
            && x.created_at_ms == y.created_at_ms
            && x.chars == y.chars
            && x.chars_sent == y.chars_sent
            && x.duration_ms == y.duration_ms
            && x.voice == y.voice
            && x.language == y.language
            && x.provider == y.provider
            && std::mem::discriminant(&x.status) == std::mem::discriminant(&y.status)
            && x.error == y.error
            && x.audio_filename == y.audio_filename
            && x.audio_bytes == y.audio_bytes
            && x.text == y.text
    }
    let m = (&a.reading_metrics, &b.reading_metrics);
    a.history_kind == b.history_kind
        && (std::sync::Arc::ptr_eq(&a.readings, &b.readings)
            || (a.readings.len() == b.readings.len()
                && a.readings
                    .iter()
                    .zip(b.readings.iter())
                    .all(|(x, y)| reading_eq(x, y))))
        && a.readings_has_more == b.readings_has_more
        && a.active_reading_id == b.active_reading_id
        && match (&a.selected_reading, &b.selected_reading) {
            (None, None) => true,
            (Some(x), Some(y)) => reading_eq(x, y),
            _ => false,
        }
        && m.0.readings == m.1.readings
        && m.0.chars_sent == m.1.chars_sent
        && m.0.duration_ms == m.1.duration_ms
        && m.0.estimated_tts_usd.to_bits() == m.1.estimated_tts_usd.to_bits()
        && m.0.audio_bytes == m.1.audio_bytes
}

/// Everything the reader draws except the output level, which is judged
/// on its envelope like the dictation wave. Shared collections compare by
/// identity first: the controller keeps them between position updates.
fn same_reader_state(a: &TtsSnapshot, b: &TtsSnapshot) -> bool {
    use std::sync::Arc;
    a.phase == b.phase
        && a.show == b.show
        && a.reading == b.reading
        && a.position_ms == b.position_ms
        && a.total_ms == b.total_ms
        && a.total_exact == b.total_exact
        && a.generating == b.generating
        && a.current_sentence == b.current_sentence
        && a.current_word == b.current_word
        && a.rate.to_bits() == b.rate.to_bits()
        && a.volume.to_bits() == b.volume.to_bits()
        && a.voice == b.voice
        && a.versions == b.versions
        && a.active_version == b.active_version
        && a.chars == b.chars
        && a.chars_sent == b.chars_sent
        && a.estimated_usd.to_bits() == b.estimated_usd.to_bits()
        && a.confirm == b.confirm
        && a.queued == b.queued
        && a.status == b.status
        && a.error == b.error
        && a.notice == b.notice
        && (Arc::ptr_eq(&a.generated, &b.generated) || a.generated == b.generated)
        && (Arc::ptr_eq(&a.text, &b.text) || a.text == b.text)
        && (Arc::ptr_eq(&a.sentences, &b.sentences) || a.sentences == b.sentences)
        && (Arc::ptr_eq(&a.words, &b.words) || a.words == b.words)
        && (Arc::ptr_eq(&a.voices, &b.voices) || a.voices == b.voices)
}

/// Among the live fields, what the overlay actually draws: the transcript
/// and the timer's seconds. Levels are judged by the caller on the envelope.
fn live_change_visible(a: &AppSnapshot, b: &AppSnapshot) -> bool {
    a.partial != b.partial
        || a.confirmed != b.confirmed
        || a.elapsed_ms / 1000 != b.elapsed_ms / 1000
}

fn apply_palette(palette: Palette, cx: &mut App) {
    cx.global_mut::<UiState>().palette = palette;
    widgets::prewarm_grain(palette, cx);
    cx.refresh_windows();
}

/// Honours `DICTA_REDUCE_MOTION=1` and, on Linux, the desktop
/// "enable-animations" key. macOS and Windows settings come with the
/// platform layer (MP-12).
fn detect_reduce_motion() -> bool {
    if let Some(v) = std::env::var_os("DICTA_REDUCE_MOTION") {
        return v != "0";
    }
    if cfg!(any(target_os = "macos", target_os = "windows")) {
        return false;
    }
    std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "enable-animations"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .is_some_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "false")
}

/// Runs the UI on the calling (main) thread until the user quits or the
/// controller drops its snapshot sender.
pub fn run(
    initial: AppSnapshot,
    command_tx: Sender<Command>,
    snapshot_rx: Receiver<AppSnapshot>,
) -> anyhow::Result<()> {
    // Linux defaults to quitting when the last window closes; Dicta stays
    // resident with no windows (settings closed, idle overlay hidden).
    let app = gpui_platform::application()
        .with_assets(assets::Assets)
        .with_quit_mode(gpui::QuitMode::Explicit);
    let mut init_error = None;
    let error_slot = std::rc::Rc::new(std::cell::RefCell::new(None));
    let slot = error_slot.clone();
    app.run(move |cx: &mut App| {
        let system_lang = Lang::system();
        let lang = Lang::resolve(&initial.config.ui_language, system_lang);
        if let Err(e) = assets::load_fonts(cx) {
            *slot.borrow_mut() = Some(e.context(lang.t(Msg::FontsFailed)));
            cx.quit();
            return;
        }
        let mut first = initial.config.clone();
        first.normalize();
        cx.set_global(UiState {
            reduce_motion: detect_reduce_motion(),
            system_lang,
            ..UiState::new(Palette::new(first.theme, first.color_mode), lang)
        });
        let reduce = cx.global::<UiState>().reduce_motion;
        cx.set_reduce_motion(reduce);
        // Rasterise the grain of the current style now, in the background,
        // so the first overlay does not open flat and fill in later.
        widgets::prewarm_grain(cx.global::<UiState>().palette, cx);
        text_field::bind_keys(cx);
        reader::bind_keys(cx);
        // `secondary-` is Cmd on macOS and Ctrl elsewhere.
        cx.bind_keys([
            KeyBinding::new("secondary-q", Quit, None),
            KeyBinding::new("secondary-w", CloseWindow, None),
        ]);

        let hub = cx.new(|_| Hub::new(initial, command_tx));

        cx.on_action({
            let hub = hub.clone();
            move |_: &Quit, cx| quit(&hub, cx)
        });

        // Windows closed by the compositor or the window button.
        cx.on_window_closed({
            let hub = hub.downgrade();
            move |cx, id| {
                let Some(hub) = hub.upgrade() else { return };
                hub.update(cx, |hub, cx| {
                    if hub.settings.is_some_and(|w| w.window_id() == id) {
                        hub.settings = None;
                        hub.settings_dismissed = true;
                        hub.send(Command::HideSettings);
                    }
                    if hub.overlay.is_some_and(|(w, _, _)| w.window_id() == id) {
                        hub.overlay = None;
                        hub.send(Command::HideOverlay);
                    }
                    // Closed by the compositor: like ×, stop the reading.
                    if hub.reader.is_some_and(|(w, _, _)| w.window_id() == id) {
                        hub.reader = None;
                        hub.send(Command::TtsStop);
                    }
                    cx.notify();
                });
            }
        })
        .detach();

        cx.observe(&hub, |hub, cx| sync_windows(&hub, cx)).detach();

        // Snapshot bridge: a plain thread blocks on the std receiver and
        // forwards into an async channel, so the main thread never polls.
        let (async_tx, mut async_rx) =
            futures::channel::mpsc::unbounded::<(AppSnapshot, Instant)>();
        std::thread::Builder::new()
            .name("dicta-ui-snapshots".into())
            .spawn(move || {
                while let Ok(snap) = snapshot_rx.recv() {
                    if async_tx.unbounded_send((snap, Instant::now())).is_err() {
                        break;
                    }
                }
            })
            .expect("spawn snapshot thread");
        cx.spawn({
            let hub = hub.clone();
            async move |cx| {
                while let Some(snap) = async_rx.next().await {
                    // Coalesce bursts but keep every level for the wave.
                    let mut batch = vec![snap];
                    while let Ok(next) = async_rx.try_recv() {
                        batch.push(next);
                    }
                    cx.update(|cx| {
                        hub.update(cx, |hub, cx| {
                            for (snap, at) in batch {
                                hub.apply(snap, at, cx);
                            }
                        })
                    });
                }
                cx.update(|cx| cx.quit());
            }
        })
        .detach();

        sync_windows(&hub, cx);
    });
    if let Some(e) = error_slot.borrow_mut().take() {
        init_error = Some(e);
    }
    match init_error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

pub fn quit(hub: &Entity<Hub>, cx: &mut App) {
    hub.read(cx).send(Command::Quit);
    cx.quit();
}

/// Card height for the current phase. Long text scrolls inside the card.
pub(crate) fn overlay_height(snap: &AppSnapshot) -> f32 {
    match snap.phase {
        Phase::Idle => {
            if snap.authenticated || snap.demo {
                72.
            } else {
                112.
            }
        }
        Phase::Connecting => 132.,
        // Room for five lines of live transcript.
        Phase::Recording | Phase::Finalizing | Phase::Polishing => 262.,
        Phase::Ready => OVERLAY_MAX_HEIGHT,
        // Title, up to three lines of message and the actions.
        Phase::Error => 184.,
    }
}

/// A local close hides settings at once; only a fresh false→true
/// `show_settings` from the controller clears it. A stale snapshot sent
/// before the controller handled `HideSettings` cannot reopen the window.
fn settings_still_dismissed(dismissed: bool, shown_before: bool, shown_now: bool) -> bool {
    dismissed && !(shown_now && !shown_before)
}

fn wants_overlay(snap: &AppSnapshot) -> bool {
    snap.show_overlay
        && (snap.phase != Phase::Idle || snap.config.show_idle_overlay || snap.error.is_some())
}

/// Converts configured offsets into a top-left origin on a display.
/// Non-negative values measure from the top/left edge, negative values from
/// the bottom/right edge, so bottom-anchored overlays grow upwards.
pub fn overlay_origin(x: f32, y: f32, overlay: Size<f32>, display: Bounds<f32>) -> Point<f32> {
    let ox = if x >= 0. {
        x
    } else {
        display.size.width - overlay.width + x
    };
    let oy = if y >= 0. {
        y
    } else {
        display.size.height - overlay.height + y
    };
    let max_x = (display.size.width - overlay.width).max(0.);
    let max_y = (display.size.height - overlay.height).max(0.);
    point(
        display.origin.x + ox.clamp(0., max_x),
        display.origin.y + oy.clamp(0., max_y),
    )
}

/// Top-left corner for a placement on a display: the display centre, or the
/// configured offsets.
pub fn placement_origin(
    placement: OverlayPlacement,
    overlay: Size<f32>,
    display: Bounds<f32>,
) -> Point<f32> {
    if placement.centered {
        point(
            display.origin.x + ((display.size.width - overlay.width) / 2.).max(0.).round(),
            display.origin.y
                + ((display.size.height - overlay.height) / 2.)
                    .max(0.)
                    .round(),
        )
    } else {
        overlay_origin(placement.x, placement.y, overlay, display)
    }
}

/// Last resort when the platform reports no display at all.
const FALLBACK_DISPLAY: Bounds<f32> = Bounds {
    origin: Point { x: 0., y: 0. },
    size: Size {
        width: 1920.,
        height: 1080.,
    },
};

fn to_f32(b: Bounds<Pixels>) -> Bounds<f32> {
    Bounds {
        origin: point(f32::from(b.origin.x), f32::from(b.origin.y)),
        size: size(f32::from(b.size.width), f32::from(b.size.height)),
    }
}

/// The display to place against: the platform's primary one, else the one
/// showing one of our windows, else the top-left-most display (the order of
/// `displays()` is not stable on Wayland), else 1920×1080.
fn pick_display(
    primary: Option<Bounds<f32>>,
    of_window: Option<Bounds<f32>>,
    all: &[Bounds<f32>],
) -> Bounds<f32> {
    primary
        .or(of_window)
        .or_else(|| {
            all.iter().copied().min_by(|a, b| {
                (a.origin.y, a.origin.x)
                    .partial_cmp(&(b.origin.y, b.origin.x))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
        })
        .unwrap_or(FALLBACK_DISPLAY)
}

/// Bounds of the display windows are placed on, in logical pixels. Wayland
/// has no primary display at this GPUI revision (`primary_display()` is
/// always None), so the displays are asked directly instead of falling back
/// to 1920×1080.
pub(crate) fn display_bounds(cx: &mut App) -> Bounds<f32> {
    let primary = cx.primary_display().map(|d| to_f32(d.bounds()));
    let of_window = if primary.is_some() {
        None
    } else {
        cx.windows().into_iter().find_map(|w| {
            w.update(cx, |_, window, cx| window.display(cx))
                .ok()
                .flatten()
                .map(|d| to_f32(d.bounds()))
        })
    };
    let all: Vec<Bounds<f32>> = cx.displays().iter().map(|d| to_f32(d.bounds())).collect();
    pick_display(primary, of_window, &all)
}

fn sync_windows(hub: &Entity<Hub>, cx: &mut App) {
    sync_reader(hub, cx);
    let h = hub.read(cx);
    let want_settings = h.snap.show_settings && !h.settings_dismissed;
    let settings_open = h.settings;
    let want_overlay = wants_overlay(&h.snap);
    let height = overlay_height(&h.snap);
    let placement = OverlayPlacement::from_config(&h.snap.config);
    let current = h.overlay;

    match (want_settings, settings_open) {
        (true, None) => {
            if let Ok(handle) = open_settings(hub.clone(), cx) {
                hub.update(cx, |h, _| h.settings = Some(handle));
            }
        }
        (false, Some(handle)) => {
            hub.update(cx, |h, _| h.settings = None);
            handle
                .update(cx, |_, window, _| window.remove_window())
                .ok();
        }
        _ => {}
    }

    match overlay_action(
        want_overlay,
        current.map(|(_, p, h)| (p, h)),
        placement,
        height,
    ) {
        OverlayAction::Keep => {}
        OverlayAction::Open => open_overlay_window(hub, placement, height, cx),
        OverlayAction::Close | OverlayAction::Reopen => {
            let Some((handle, _, _)) = current else {
                return;
            };
            hub.update(cx, |h, _| h.overlay = None);
            handle
                .update(cx, |_, window, _| window.remove_window())
                .ok();
            if overlay_action(want_overlay, None, placement, height) == OverlayAction::Open {
                open_overlay_window(hub, placement, height, cx);
            }
        }
        OverlayAction::Resize => {
            let Some((handle, _, _)) = current else {
                return;
            };
            handle
                .update(cx, |_, window, _| {
                    window.resize(size(px(OVERLAY_WIDTH), px(height)));
                })
                .ok();
            hub.update(cx, |h, _| {
                if let Some(o) = h.overlay.as_mut() {
                    o.2 = height;
                }
            });
        }
    }
}

/// Opens, closes, reopens (placement changed in Ajustes) or resizes (X11)
/// the read-aloud window, like the overlay.
fn sync_reader(hub: &Entity<Hub>, cx: &mut App) {
    let orbit = cx.global::<UiState>().palette.voice_style == VoiceStyle::Orbit;
    let h = hub.read(cx);
    let want = h.snap.tts.show;
    let height = reader::reader_height(&h.snap, orbit);
    let placement = OverlayPlacement::for_reader(&h.snap.config);
    let current = h.reader;
    match overlay_action(want, current.map(|(_, p, h)| (p, h)), placement, height) {
        OverlayAction::Keep => {}
        OverlayAction::Open => open_reader_window(hub, placement, height, cx),
        OverlayAction::Close | OverlayAction::Reopen => {
            let Some((handle, _, _)) = current else {
                return;
            };
            hub.update(cx, |h, _| h.reader = None);
            handle
                .update(cx, |_, window, _| window.remove_window())
                .ok();
            if want {
                open_reader_window(hub, placement, height, cx);
            }
        }
        OverlayAction::Resize => {
            let Some((handle, _, _)) = current else {
                return;
            };
            handle
                .update(cx, |_, window, _| {
                    window.resize(size(px(READER_WIDTH), px(height)));
                })
                .ok();
            hub.update(cx, |h, _| {
                if let Some(r) = h.reader.as_mut() {
                    r.2 = height;
                }
            });
        }
    }
}

fn open_reader_window(hub: &Entity<Hub>, placement: OverlayPlacement, height: f32, cx: &mut App) {
    if let Ok(handle) = open_reader(hub.clone(), placement, height, cx) {
        hub.update(cx, |h, cx| {
            h.reader = Some((handle, placement, height));
            cx.notify();
        });
    }
}

/// Opens the reader like the overlay: a borderless floating toplevel that
/// never takes focus on open (the user is reading in another app) and is
/// dragged natively from its header.
fn open_reader(
    hub: Entity<Hub>,
    placement: OverlayPlacement,
    height: f32,
    cx: &mut App,
) -> anyhow::Result<WindowHandle<Reader>> {
    let fixed = fixed_surface();
    let height = if fixed { READER_MAX_HEIGHT } else { height };
    let reader_size = size(READER_WIDTH, height);
    let display = display_bounds(cx);
    let origin = placement_origin(placement, reader_size, display);
    let bounds: Bounds<Pixels> = Bounds {
        origin: point(px(origin.x), px(origin.y)),
        size: size(px(reader_size.width), px(reader_size.height)),
    };
    let handle = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: None,
            focus: false,
            show: true,
            kind: FLOATING_KIND,
            is_movable: true,
            is_resizable: false,
            is_minimizable: false,
            window_background: WindowBackgroundAppearance::Transparent,
            app_id: Some(READER_APP_ID.into()),
            ..Default::default()
        },
        move |window, cx| cx.new(|cx| Reader::new(hub, placement, window, cx)),
    )?;
    if fixed
        && !placement.centered
        && let Some(placer) = READER_PLACER.get()
    {
        let local = Bounds {
            origin: point(0., 0.),
            size: display.size,
        };
        placer(placement_origin(placement, reader_size, local));
    }
    Ok(handle)
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum OverlayAction {
    Keep,
    Open,
    Close,
    /// Only an explicit placement change (Ajustes) reopens the window; phase
    /// changes keep it, so a position the user dragged to survives.
    Reopen,
    Resize,
}

fn overlay_action(
    want: bool,
    open: Option<(OverlayPlacement, f32)>,
    placement: OverlayPlacement,
    height: f32,
) -> OverlayAction {
    match (want, open) {
        (false, None) => OverlayAction::Keep,
        (false, Some(_)) => OverlayAction::Close,
        (true, None) => OverlayAction::Open,
        (true, Some((open_placement, _))) if open_placement != placement => OverlayAction::Reopen,
        (true, Some((_, open_height))) if open_height != height && !fixed_surface() => {
            OverlayAction::Resize
        }
        (true, Some(_)) => OverlayAction::Keep,
    }
}

fn open_overlay_window(hub: &Entity<Hub>, placement: OverlayPlacement, height: f32, cx: &mut App) {
    if let Ok(handle) = open_overlay(hub.clone(), placement, height, cx) {
        hub.update(cx, |h, cx| {
            h.overlay = Some((handle, placement, height));
            cx.notify();
        });
    }
}

/// Opens the overlay as a borderless floating toplevel sized to the card.
/// Wayland gives `xdg_toplevel.move` through `start_window_move`, so the
/// compositor drags it natively; placement comes from compositor rules
/// (centre) or the host placer (offsets). X11 honours the origin directly.
fn open_overlay(
    hub: Entity<Hub>,
    placement: OverlayPlacement,
    height: f32,
    cx: &mut App,
) -> anyhow::Result<WindowHandle<Overlay>> {
    let fixed = fixed_surface();
    let height = if fixed { OVERLAY_MAX_HEIGHT } else { height };
    let overlay_size = size(OVERLAY_WIDTH, height);
    let display = display_bounds(cx);
    let origin = placement_origin(placement, overlay_size, display);
    let bounds: Bounds<Pixels> = Bounds {
        origin: point(px(origin.x), px(origin.y)),
        size: size(px(overlay_size.width), px(overlay_size.height)),
    };
    let handle = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: None,
            // Live phases must never take focus from the app being dictated
            // into; Ready gets it when the user clicks the text.
            focus: false,
            show: true,
            kind: FLOATING_KIND,
            is_movable: true,
            is_resizable: false,
            is_minimizable: false,
            window_background: WindowBackgroundAppearance::Transparent,
            app_id: Some(OVERLAY_APP_ID.into()),
            ..Default::default()
        },
        move |window, cx| cx.new(|cx| Overlay::new(hub, placement, window, cx)),
    )?;
    if fixed
        && !placement.centered
        && let Some(placer) = PLACER.get()
    {
        let local = Bounds {
            origin: point(0., 0.),
            size: display.size,
        };
        placer(placement_origin(placement, overlay_size, local));
    }
    Ok(handle)
}

fn open_settings(hub: Entity<Hub>, cx: &mut App) -> anyhow::Result<WindowHandle<Settings>> {
    let bounds = Bounds::centered(None, size(px(1040.), px(720.)), cx);
    let handle = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some(APP_NAME.into()),
                ..Default::default()
            }),
            focus: true,
            kind: WindowKind::Normal,
            app_id: Some(APP_ID.into()),
            window_min_size: Some(size(px(MAIN_MIN_W), px(540.))),
            ..Default::default()
        },
        move |window, cx| cx.new(|cx| Settings::new(hub, window, cx)),
    )?;
    Ok(handle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn display() -> Bounds<f32> {
        Bounds {
            origin: point(0., 0.),
            size: size(1920., 1080.),
        }
    }

    #[test]
    fn positive_offsets_measure_from_top_left() {
        let o = overlay_origin(24., 30., size(400., 200.), display());
        assert_eq!((o.x, o.y), (24., 30.));
    }

    #[test]
    fn negative_offsets_measure_from_bottom_right_and_stay_on_screen() {
        let o = overlay_origin(-24., -48., size(400., 200.), display());
        assert_eq!((o.x, o.y), (1920. - 400. - 24., 1080. - 200. - 48.));
        let off = overlay_origin(5000., -5000., size(400., 200.), display());
        assert_eq!((off.x, off.y), (1520., 0.));
    }

    #[test]
    fn default_config_opens_centered_on_the_display() {
        let placement = OverlayPlacement::from_config(&AppConfig::default());
        assert!(placement.centered);
        let display = Bounds {
            origin: point(2560., 0.),
            size: size(1920., 1080.),
        };
        let o = placement_origin(placement, size(404., 318.), display);
        assert_eq!((o.x, o.y), (2560. + 758., 381.));
    }

    #[test]
    fn hidden_offsets_do_not_change_a_centered_placement() {
        let mut config = AppConfig::default();
        let before = OverlayPlacement::from_config(&config);
        config.overlay_x = -24.;
        config.overlay_y = -48.;
        assert_eq!(OverlayPlacement::from_config(&config), before);
        config.overlay_centered = false;
        let edge = OverlayPlacement::from_config(&config);
        assert_ne!(edge, before);
        assert!(edge.card_at_bottom());
    }

    #[test]
    fn phase_changes_keep_the_dragged_window() {
        let placement = OverlayPlacement::from_config(&AppConfig::default());
        let mut snap = AppSnapshot::new(AppConfig::default());
        let mut open = None;
        for phase in [
            Phase::Idle,
            Phase::Connecting,
            Phase::Recording,
            Phase::Finalizing,
            Phase::Polishing,
            Phase::Ready,
            Phase::Error,
            Phase::Idle,
        ] {
            snap.phase = phase;
            let height = overlay_height(&snap);
            let action = overlay_action(true, open, placement, height);
            if open.is_none() {
                assert_eq!(action, OverlayAction::Open);
            } else {
                // Never reopened (which would recenter): kept or resized.
                assert!(
                    matches!(action, OverlayAction::Keep | OverlayAction::Resize),
                    "{phase:?}: {action:?}"
                );
            }
            open = Some((placement, height));
        }
        let moved = OverlayPlacement {
            centered: false,
            x: 24.,
            y: 24.,
        };
        assert_eq!(
            overlay_action(true, open, moved, 72.),
            OverlayAction::Reopen
        );
        assert_eq!(
            overlay_action(false, open, placement, 72.),
            OverlayAction::Close
        );
    }

    #[test]
    fn every_card_fits_the_fixed_wayland_window() {
        let mut snap = AppSnapshot::new(AppConfig::default());
        for phase in [
            Phase::Idle,
            Phase::Connecting,
            Phase::Recording,
            Phase::Ready,
            Phase::Error,
        ] {
            snap.phase = phase;
            assert!(overlay_height(&snap) <= OVERLAY_MAX_HEIGHT, "{phase:?}");
        }
    }

    #[test]
    fn closed_settings_reopen_only_after_controller_hides_and_shows_again() {
        // Closed locally; an in-flight snapshot still says shown.
        let dismissed = settings_still_dismissed(true, true, true);
        assert!(dismissed);
        // Controller confirms HideSettings.
        let dismissed = settings_still_dismissed(dismissed, true, false);
        assert!(dismissed);
        // Tray or `dicta ctl settings` sends ShowSettings.
        assert!(!settings_still_dismissed(dismissed, false, true));
    }

    fn hub_with(config: AppConfig) -> Hub {
        let (tx, _rx) = std::sync::mpsc::channel();
        Hub::new(AppSnapshot::new(config), tx)
    }

    #[test]
    fn legacy_night_shows_as_signal_dark_until_the_user_picks_again() {
        let mut hub = hub_with(AppConfig {
            theme: Theme::Night,
            color_mode: ColorMode::Light,
            ..Default::default()
        });
        assert_eq!(
            (hub.theme(), hub.color_mode()),
            (Theme::Signal, ColorMode::Dark)
        );
        assert!(hub.palette().dark);
        // Picking a style keeps the dark colours the old Night had.
        hub.pending_theme = Some(Theme::Orbit);
        assert_eq!(
            (hub.theme(), hub.color_mode()),
            (Theme::Orbit, ColorMode::Dark)
        );
        hub.pending_mode = Some(ColorMode::Light);
        assert_eq!(hub.color_mode(), ColorMode::Light);
    }

    #[test]
    fn style_and_colour_mode_are_independent() {
        for theme in Theme::ALL {
            for mode in [ColorMode::Light, ColorMode::Dark] {
                let hub = hub_with(AppConfig {
                    theme,
                    color_mode: mode,
                    ..Default::default()
                });
                let p = hub.palette();
                assert_eq!((p.theme, p.mode), (theme, mode));
            }
        }
    }

    fn recording(confirmed: &str, partial: &str, elapsed_ms: u64, seq: u64) -> AppSnapshot {
        let mut s = AppSnapshot::new(AppConfig::default());
        s.phase = Phase::Recording;
        s.confirmed = confirmed.into();
        s.partial = partial.into();
        s.elapsed_ms = elapsed_ms;
        s.level_seq = seq;
        s.level = 0.1;
        s.revision = seq;
        s
    }

    #[test]
    fn level_clock_and_transcript_snapshots_are_live_only() {
        let a = recording("hola", "mun", 1_200, 7);
        let b = recording("hola mundo", "y", 1_950, 8);
        assert!(same_main_state(&a, &b));
        assert!(live_change_visible(&a, &b), "transcript grew");
        // Same transcript, same second, only a level: invisible by itself.
        let c = recording("hola", "mun", 1_900, 8);
        assert!(same_main_state(&a, &c));
        assert!(!live_change_visible(&a, &c));
        // The timer ticks a second.
        let d = recording("hola", "mun", 2_000, 8);
        assert!(live_change_visible(&a, &d));
    }

    #[test]
    fn anything_the_main_window_shows_notifies_the_hub() {
        let a = recording("hola", "", 0, 1);
        let mut b = a.clone();
        b.phase = Phase::Finalizing;
        assert!(!same_main_state(&a, &b), "phase");
        let mut b = a.clone();
        b.status = "otro".into();
        assert!(!same_main_state(&a, &b), "status");
        let mut b = a.clone();
        b.config.theme = Theme::Orbit;
        assert!(!same_main_state(&a, &b), "config");
        let mut b = a.clone();
        b.notes = std::sync::Arc::new(vec![dicta_core::Note {
            id: 1,
            created_at_ms: 1,
            original: "x".into(),
            text: "x".into(),
            word_count: 1,
            captured_ms: 1,
            sent_ms: 1,
            provider: dicta_core::ProviderMode::Subscription,
            status: dicta_core::NoteStatus::Completed,
            error: None,
            audio_filename: None,
            audio_bytes: 0,
            input_tokens: None,
            output_tokens: None,
        }]);
        assert!(!same_main_state(&a, &b), "new notes collection");
        // An equal collection in a fresh allocation (e.g. a reloaded page)
        // is still the same main state.
        let mut b = a.clone();
        b.notes = std::sync::Arc::new(a.notes.as_ref().clone());
        assert!(same_main_state(&a, &b), "same notes, new Arc");
        let mut b = a.clone();
        b.error = Some("x".into());
        assert!(!same_main_state(&a, &b), "error");
        let mut b = a.clone();
        b.show_overlay = false;
        assert!(!same_main_state(&a, &b), "overlay visibility");
        let mut b = a.clone();
        b.metrics.words += 1;
        assert!(!same_main_state(&a, &b), "metrics");
        let mut b = a.clone();
        b.devices.push(dicta_core::AudioDevice {
            id: "m".into(),
            name: "Mic".into(),
        });
        assert!(!same_main_state(&a, &b), "devices");
        // The shared notes compare by identity: a clone is the same list.
        let b = a.clone();
        assert!(same_main_state(&a, &b));
    }

    #[test]
    fn silence_is_detected_on_the_envelope() {
        use std::time::Duration;
        let start = Instant::now();
        let mut h = LevelHistory::default();
        h.push(1, 0.0005, start);
        assert!(h.is_silent(start), "room noise draws nothing");
        h.push(2, 0.1, start + Duration::from_millis(50));
        assert!(!h.is_silent(start + Duration::from_millis(50)));
        // A zero reading right after speech still releases: not silent yet.
        h.push(3, 0., start + Duration::from_millis(100));
        assert!(!h.is_silent(start + Duration::from_millis(100)));
        for k in 4..30 {
            h.push(k, 0., start + Duration::from_millis(100 * k));
        }
        assert!(h.is_silent(start + Duration::from_millis(3_000)));
    }

    fn playing(position_ms: u64, word: usize, seq: u64) -> AppSnapshot {
        use dicta_core::tts::{TtsPhase, WordTiming};
        let mut s = AppSnapshot::new(AppConfig::default());
        s.tts.phase = TtsPhase::Playing;
        s.tts.show = true;
        s.tts.reading = 3;
        s.tts.text = std::sync::Arc::new("Hola mundo.".into());
        s.tts.words = std::sync::Arc::new(vec![
            WordTiming {
                span: dicta_core::tts::TextSpan { start: 0, end: 4 },
                start_ms: 0,
                end_ms: 400,
            },
            WordTiming {
                span: dicta_core::tts::TextSpan { start: 5, end: 10 },
                start_ms: 450,
                end_ms: 900,
            },
        ]);
        s.tts.generated = std::sync::Arc::new(vec![(0, 900)]);
        s.tts.total_ms = 900;
        s.tts.position_ms = position_ms;
        s.tts.current_word = Some(word);
        s.tts.level_seq = seq;
        s.tts.level = 0.1;
        s
    }

    #[test]
    fn reading_progress_moves_only_the_reader() {
        let a = playing(100, 0, 1);
        let mut b = a.clone();
        b.tts.position_ms = 500;
        b.tts.current_word = Some(1);
        b.tts.level_seq = 2;
        b.tts.level = 0.3;
        b.tts.chars_sent = 11;
        // The main window and window management see nothing new...
        assert!(same_main_state(&a, &b));
        // ...the reader does.
        assert!(!same_reader_state(&a.tts, &b.tts));
        // A level alone is judged on the envelope, not as a reader change.
        let mut c = a.clone();
        c.tts.level_seq = 9;
        c.tts.level = 0.5;
        assert!(same_reader_state(&a.tts, &c.tts));
        // Equal shared text in a fresh allocation is the same reader state.
        let mut d = a.clone();
        d.tts.text = std::sync::Arc::new(a.tts.text.as_ref().clone());
        assert!(same_reader_state(&a.tts, &d.tts));
    }

    #[test]
    fn reader_visibility_card_and_history_reach_the_main_state() {
        use dicta_core::tts::TtsPhase;
        let a = playing(100, 0, 1);
        let mut b = a.clone();
        b.tts.show = false;
        assert!(!same_main_state(&a, &b), "open or close the reader");
        let mut b = a.clone();
        b.tts.phase = TtsPhase::Paused;
        assert!(!same_main_state(&a, &b), "phase changes the card");
        let mut b = a.clone();
        b.tts.error = Some("x".into());
        assert!(!same_main_state(&a, &b), "error");
        let mut b = a.clone();
        b.config.tts.compact = true;
        assert!(!same_main_state(&a, &b), "compact is configuration");
        let mut b = a.clone();
        b.history_kind = dicta_core::HistoryKind::Readings;
        assert!(!same_main_state(&a, &b), "history kind");
        let mut b = a.clone();
        b.reading_metrics.readings = 4;
        assert!(!same_main_state(&a, &b), "reading metrics");
        let mut b = a.clone();
        b.tts.account_ready = true;
        assert!(!same_main_state(&a, &b), "account for Ajustes → Lectura");
    }

    #[test]
    fn the_reader_opens_centered_and_honours_its_own_offsets() {
        let mut config = AppConfig::default();
        let reader = OverlayPlacement::for_reader(&config);
        assert!(reader.centered);
        // Overlay offsets never move the reader, and the reverse.
        config.overlay_centered = false;
        config.overlay_x = -24.;
        assert_eq!(OverlayPlacement::for_reader(&config), reader);
        config.tts.window_centered = false;
        config.tts.window_x = -24.;
        config.tts.window_y = -48.;
        let edge = OverlayPlacement::for_reader(&config);
        assert!(!edge.centered && edge.card_at_bottom());
        assert_ne!(OverlayPlacement::from_config(&config), edge);
        let o = placement_origin(edge, size(READER_WIDTH, READER_MAX_HEIGHT), display());
        assert_eq!(
            (o.x, o.y),
            (1920. - READER_WIDTH - 24., 1080. - READER_MAX_HEIGHT - 48.)
        );
        assert_ne!(READER_APP_ID, OVERLAY_APP_ID);
    }

    #[test]
    fn reading_states_keep_the_dragged_reader_window() {
        use dicta_core::tts::TtsPhase;
        let mut snap = playing(0, 0, 0);
        let placement = OverlayPlacement::for_reader(&snap.config);
        let mut open = None;
        for phase in [
            TtsPhase::Reading,
            TtsPhase::Confirm,
            TtsPhase::Connecting,
            TtsPhase::Playing,
            TtsPhase::Buffering,
            TtsPhase::Paused,
            TtsPhase::Finished,
            TtsPhase::Error,
        ] {
            snap.tts.phase = phase;
            let height = reader::reader_height(&snap, false);
            let action = overlay_action(snap.tts.show, open, placement, height);
            if open.is_none() {
                assert_eq!(action, OverlayAction::Open);
            } else {
                assert!(
                    matches!(action, OverlayAction::Keep | OverlayAction::Resize),
                    "{phase:?}: {action:?}"
                );
            }
            open = Some((placement, height));
        }
        snap.tts.show = false;
        assert_eq!(
            overlay_action(snap.tts.show, open, placement, 100.),
            OverlayAction::Close
        );
    }

    fn bounds(x: f32, y: f32, w: f32, h: f32) -> Bounds<f32> {
        Bounds {
            origin: point(x, y),
            size: size(w, h),
        }
    }

    #[test]
    fn wayland_places_against_a_real_display_not_a_guess() {
        let laptop = bounds(0., 0., 1536., 960.);
        let external = bounds(1536., 0., 2560., 1440.);
        // Wayland: no primary display. The display showing one of our
        // windows wins over the list.
        assert_eq!(
            pick_display(None, Some(external), &[laptop, external]),
            external
        );
        // No window yet: the top-left-most display, whatever the list order.
        assert_eq!(pick_display(None, None, &[external, laptop]), laptop);
        assert_eq!(pick_display(None, None, &[laptop, external]), laptop);
        // X11, Windows and macOS report a primary display.
        assert_eq!(
            pick_display(Some(external), Some(laptop), &[laptop]),
            external
        );
        // Nothing reported at all.
        assert_eq!(pick_display(None, None, &[]), FALLBACK_DISPLAY);
        // Negative offsets measure from the real bottom-right edge.
        let o = overlay_origin(-24., -48., size(OVERLAY_WIDTH, 72.), laptop);
        assert_eq!((o.x, o.y), (1536. - OVERLAY_WIDTH - 24., 960. - 72. - 48.));
    }

    #[test]
    fn a_language_change_is_a_main_window_change() {
        // Live-only snapshots skip the main window; switching the language
        // must not be mistaken for one, or Ajustes would keep the old copy.
        let a = recording("hola", "", 0, 1);
        let mut b = a.clone();
        b.config.ui_language = "en".into();
        assert!(!same_main_state(&a, &b));
    }

    #[test]
    fn idle_overlay_follows_configuration() {
        let mut snap = AppSnapshot::new(AppConfig::default());
        snap.config.show_idle_overlay = false;
        assert!(!wants_overlay(&snap));
        snap.phase = Phase::Recording;
        assert!(wants_overlay(&snap));
        snap.show_overlay = false;
        assert!(!wants_overlay(&snap));
    }
}

/// Real GPUI windows on the test platform (no GPU): needs GPUI's
/// `test-support`, so it runs with `--features screenshots`.
#[cfg(all(test, feature = "screenshots"))]
mod render_tests {
    use std::{cell::RefCell, rc::Rc, sync::mpsc};

    use gpui::{IntoElement, Render, StyleRefinement, TestAppContext, Window, div, prelude::*};

    use super::*;

    /// Records the language each render sees.
    struct Probe(Rc<RefCell<Vec<Lang>>>);

    impl Render for Probe {
        fn render(&mut self, _: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
            let l = lang(cx);
            self.0.borrow_mut().push(l);
            div().child(l.t(Msg::Settings))
        }
    }

    /// Embeds the probe as a cached view, like the reader's text.
    struct Host(Entity<Probe>);

    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
            div().size_full().child(
                self.0
                    .clone()
                    .cached(StyleRefinement::default().size_full()),
            )
        }
    }

    fn draw(cx: &mut TestAppContext, window: gpui::AnyWindowHandle) {
        cx.update(|cx| {
            cx.update_window(window, |_, window, cx| {
                let arena = window.draw(cx);
                arena.clear(cx);
            })
        })
        .unwrap();
    }

    #[test]
    fn switching_language_rerenders_every_window_including_cached_views() {
        let mut cx = TestAppContext::single();
        cx.update(|cx| {
            cx.set_global(UiState::new(
                Palette::new(Theme::Signal, ColorMode::Light),
                Lang::Es,
            ));
            text_field::bind_keys(cx);
        });
        let (tx, rx) = mpsc::channel();
        let config = AppConfig {
            ui_language: "es".into(),
            ..Default::default()
        };
        let hub = cx.new(|_| Hub::new(AppSnapshot::new(config), tx));
        let seen = Rc::new(RefCell::new(Vec::new()));
        let host = cx.add_window({
            let seen = seen.clone();
            move |_, cx| Host(cx.new(|_| Probe(seen)))
        });
        let overlay = cx.add_window({
            let hub = hub.clone();
            move |window, cx| {
                Overlay::new(
                    hub,
                    OverlayPlacement::from_config(&AppConfig::default()),
                    window,
                    cx,
                )
            }
        });
        let settings = cx.add_window({
            let hub = hub.clone();
            move |window, cx| Settings::new(hub, window, cx).with_section(Section::General)
        });
        for w in [host.into(), overlay.into(), settings.into()] {
            draw(&mut cx, w);
        }
        assert_eq!(seen.borrow().as_slice(), [Lang::Es]);
        // Nothing changed: the cached text is reused, not rendered again.
        draw(&mut cx, host.into());
        assert_eq!(seen.borrow().len(), 1);

        // The controller confirms English.
        let mut snap = hub.read_with(&cx, |h, _| h.snap.clone());
        snap.config.ui_language = "en".into();
        cx.update(|cx| hub.update(cx, |h, cx| h.apply(snap, Instant::now(), cx)));
        cx.run_until_parked();
        assert_eq!(cx.read_global::<UiState, _>(|s, _| s.lang), Lang::En);
        let before = perf::Counters::now();
        for w in [host.into(), overlay.into(), settings.into()] {
            draw(&mut cx, w);
        }
        assert_eq!(
            seen.borrow().last(),
            Some(&Lang::En),
            "cached view refreshed"
        );
        let work = perf::Counters::now().since(before);
        assert!(work.OVERLAY_RENDERS >= 1 && work.SETTINGS_RENDERS >= 1);

        // Picking a language in Ajustes switches at once and is persisted
        // through the controller like any setting.
        cx.update(|cx| {
            hub.update(cx, |h, cx| {
                h.update_config(cx, |c| c.ui_language = "es".into())
            })
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(cx.read_global::<UiState, _>(|s, _| s.lang), Lang::Es);
        assert!(
            rx.try_iter()
                .any(|c| matches!(c, Command::UpdateConfig(c) if c.ui_language == "es")),
            "UpdateConfig with the new language"
        );
        draw(&mut cx, host.into());
        assert_eq!(seen.borrow().last(), Some(&Lang::Es));

        // "auto" follows the system language.
        cx.update(|cx| cx.global_mut::<UiState>().system_lang = Lang::En);
        cx.update(|cx| {
            hub.update(cx, |h, cx| {
                h.update_config(cx, |c| c.ui_language = "auto".into())
            })
        })
        .unwrap();
        assert_eq!(cx.read_global::<UiState, _>(|s, _| s.lang), Lang::En);
    }
}
