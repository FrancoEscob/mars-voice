//! Read-aloud window ("lector"): a floating card, sibling of the dictation
//! overlay, that shows what the controller reads with Grok and turns every
//! control into a [`Command`]. It never plays audio, opens the network or
//! touches the clipboard; it renders [`TtsSnapshot`]s.
//!
//! Full layout: header · text · voice · progress · transport · info · footer.
//! Compact hides the text. The card fills the window edge to edge and is
//! dragged natively from its header; on Wayland the window keeps
//! [`READER_MAX_HEIGHT`] and only the card takes input, like the overlay.
//!
//! Text can be long (the default limit is 20 000 characters). Every
//! paragraph is one `StyledText` whose runs never change, so GPUI's shaping
//! cache keeps it while the reading moves; the current sentence and word are
//! quads painted behind the glyphs, and offscreen glyphs are culled by the
//! content mask. Clicking a word seeks there. The view follows the word
//! unless the user scrolled it out of sight, like the live dictation.

use std::{
    cell::{Cell, RefCell},
    ops::Range,
    path::PathBuf,
    rc::Rc,
    sync::Arc,
};

use dicta_core::{
    AppSnapshot, Command,
    tts::{
        RATE_MAX, RATE_MIN, TextSpan, TtsConfirm, TtsPhase, TtsSnapshot, TtsSource, TtsVoice,
        estimate_usd,
    },
};
use gpui::{
    AnyElement, App, Bounds, BoxShadow, Context, CursorStyle, Div, Entity, FocusHandle, FontWeight,
    HitboxBehavior, KeyBinding, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    ScrollHandle, SharedString, Stateful, StyledText, TextLayout, Window, actions, canvas, div,
    fill, img, point, prelude::*, px, size,
};

use crate::{
    Hub, OverlayPlacement, READER_MAX_HEIGHT,
    assets::{self, DISPLAY_FONT, Icon, MONO_FONT, UI_FONT},
    fixed_surface, format,
    i18n::{Lang, Msg, tf},
    lang,
    overlay::{Overlay, draggable},
    perf,
    settings::{Section, hint},
    theme::{Palette, VoiceStyle},
    ui_state,
    wave::{self, voice},
    widgets::{Kind, button, demo_badge, disabled, grain, icon, icon_button, icon_label_button},
};

actions!(
    dicta_reader,
    [
        TogglePlay,
        SkipBack,
        SkipForward,
        PrevSentence,
        NextSentence,
        Slower,
        Faster,
        CloseReader,
    ]
);

const CONTEXT: &str = "DictaReader";

/// Keys while the reader has focus (the user clicked it; it never takes
/// focus on its own).
pub fn bind_keys(cx: &mut App) {
    let c = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("space", TogglePlay, c),
        KeyBinding::new("left", SkipBack, c),
        KeyBinding::new("right", SkipForward, c),
        KeyBinding::new("shift-left", PrevSentence, c),
        KeyBinding::new("shift-right", NextSentence, c),
        KeyBinding::new("[", Slower, c),
        KeyBinding::new("]", Faster, c),
        KeyBinding::new("escape", CloseReader, c),
    ]);
}

// ------------------------------------------------------------------ layout

const HEADER_H: f32 = 44.;
/// Regolito's planet in the corner, as in the overlay; the header keeps its
/// controls clear of it.
const PLANET: f32 = 62.;
const PLANET_HEADER_PAD: f32 = 60.;
const PAD_X: f32 = 12.;
const PAD_TOP: f32 = 10.;
const PAD_BOTTOM: f32 = 8.;
const PROGRESS_H: f32 = 20.;
const TRANSPORT_H: f32 = 36.;
/// One line of status, notice or error; errors may take two.
const INFO_H: f32 = 18.;
const FOOTER_H: f32 = 28.;
const GAP_VOICE: f32 = 6.;
const GAP_PROGRESS: f32 = 8.;
const GAP_TRANSPORT: f32 = 6.;
const GAP_INFO: f32 = 4.;
const WAITING_H: f32 = 118.;
const CONFIRM_H: f32 = 318.;
/// Error card without audio: header, message and actions. The message
/// takes as many 19 px lines as it needs, up to [`FAILED_MAX_LINES`].
const FAILED_BASE_H: f32 = HEADER_H + 12. + 10. + 32. + 12.;
const FAILED_MAX_LINES: usize = 7;
/// Characters per line of the error text at 13,5 px in the card.
const FAILED_LINE_CHARS: usize = 58;

/// Rate presets of the plan; the stepper reaches every 0,05 in between.
pub(crate) const RATE_PRESETS: [f32; 6] = [0.75, 1., 1.25, 1.5, 1.75, 2.];
const RATE_STEP: f32 = 0.05;
/// `[` and `]` change the rate by this much.
const KEY_RATE_STEP: f32 = 0.25;
const VOLUME_STEP: f32 = 0.1;
const RATE_MENU_W: f32 = 324.;
/// The followed word is kept between these fractions of the text area.
const BAND_TOP: f32 = 0.06;
const BAND_BOTTOM: f32 = 0.72;
const BAND_TARGET: f32 = 0.3;

/// What the card shows for a snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReaderMode {
    /// Reading the selection, or nothing to read.
    Waiting,
    /// A long text waits for the user to accept the estimated cost.
    Confirm,
    /// Failed before any audio existed: message and next steps.
    Failed,
    /// Text, progress and transport. Errors with audio stay here.
    Player,
}

/// Some audio of the active version exists, so it can be played and saved.
pub(crate) fn has_audio(t: &TtsSnapshot) -> bool {
    t.generated.iter().any(|(a, b)| b > a)
}

pub(crate) fn reader_mode(t: &TtsSnapshot) -> ReaderMode {
    match t.phase {
        TtsPhase::Idle | TtsPhase::Reading => ReaderMode::Waiting,
        TtsPhase::Confirm => ReaderMode::Confirm,
        TtsPhase::Error if !has_audio(t) => ReaderMode::Failed,
        _ => ReaderMode::Player,
    }
}

fn voice_row_h(orbit: bool) -> f32 {
    if orbit {
        wave::ORBIT_H
    } else {
        wave::VOICE_HEIGHT
    }
}

/// Height of everything under the text: voice, progress, transport, the
/// info line (`info_lines` of them) and the footer, with paddings.
fn controls_h(orbit: bool, info_lines: u8) -> f32 {
    let info = if info_lines == 0 {
        0.
    } else {
        INFO_H * info_lines as f32 + GAP_INFO
    };
    PAD_TOP
        + voice_row_h(orbit)
        + GAP_VOICE
        + PROGRESS_H
        + GAP_PROGRESS
        + TRANSPORT_H
        + GAP_TRANSPORT
        + info
        + FOOTER_H
        + PAD_BOTTOM
}

fn failed_height(message: &str) -> f32 {
    let paragraphs: Vec<&str> = message
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let lines: usize = paragraphs
        .iter()
        .map(|l| l.chars().count().div_ceil(FAILED_LINE_CHARS).max(1))
        .sum::<usize>()
        .clamp(1, FAILED_MAX_LINES);
    let gaps = paragraphs.len().saturating_sub(1) as f32 * 4.;
    FAILED_BASE_H + lines as f32 * 19. + gaps + 4.
}

/// Card height for the snapshot. Full players take the whole surface and
/// the text absorbs the rest; compact ones reserve the info line so the card
/// does not jump when a notice comes and goes.
pub(crate) fn reader_height(snap: &AppSnapshot, orbit: bool) -> f32 {
    let t = &snap.tts;
    match reader_mode(t) {
        ReaderMode::Waiting => WAITING_H,
        ReaderMode::Confirm => CONFIRM_H,
        ReaderMode::Failed => failed_height(t.error.as_deref().unwrap_or(&t.status)),
        ReaderMode::Player if snap.config.tts.compact => {
            let lines = if t.error.is_some() { 2 } else { 1 };
            (HEADER_H + controls_h(orbit, lines)).min(READER_MAX_HEIGHT)
        }
        ReaderMode::Player => READER_MAX_HEIGHT,
    }
}

// --------------------------------------------------------------- wording

/// "1×", "1,25×", "0,75×" (or "1.25×" in English).
pub(crate) fn fmt_rate(rate: f32, l: Lang) -> String {
    let hundredths = (rate * 100.).round() as i64;
    let (int, frac) = (hundredths / 100, hundredths % 100);
    let sep = l.decimal();
    match frac {
        0 => format!("{int}×"),
        f if f % 10 == 0 => format!("{int}{sep}{}×", f / 10),
        f => format!("{int}{sep}{f:02}×"),
    }
}

/// Name of a reading phase; `TtsPhase::label` is Spanish.
pub(crate) fn phase_label(phase: TtsPhase, l: Lang) -> &'static str {
    l.t(match phase {
        TtsPhase::Idle => Msg::TtsIdle,
        TtsPhase::Reading => Msg::TtsReading,
        TtsPhase::Confirm => Msg::TtsConfirm,
        TtsPhase::Connecting => Msg::TtsConnecting,
        TtsPhase::Buffering => Msg::TtsBuffering,
        TtsPhase::Playing => Msg::TtsPlaying,
        TtsPhase::Paused => Msg::TtsPaused,
        TtsPhase::Finished => Msg::TtsFinished,
        TtsPhase::Error => Msg::TtsError,
    })
}

/// "0:00", "4:38", "1:02:03".
pub(crate) fn fmt_time(ms: u64) -> String {
    let s = ms / 1000;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// The total, marked "≈" while it is still an estimate.
pub(crate) fn total_label(ms: u64, exact: bool) -> String {
    if exact {
        fmt_time(ms)
    } else {
        format!("≈ {}", fmt_time(ms))
    }
}

/// Estimated cost, always labelled as an estimate, never as a charge.
pub(crate) fn cost_label(usd: f64, l: Lang) -> String {
    if format::below_a_cent(usd) {
        tf!(l, CostBelowEstimate, amount = format::usd_cent(l))
    } else {
        tf!(l, CostEstimate, amount = format::usd(usd, l))
    }
}

/// `rate + delta` on the 0,05 grid, within the documented bounds.
pub(crate) fn step_rate(rate: f32, delta: f32) -> f32 {
    let r = ((rate + delta) / RATE_STEP).round() * RATE_STEP;
    ((r * 100.).round() / 100.).clamp(RATE_MIN, RATE_MAX)
}

fn step_volume(volume: f32, delta: f32) -> f32 {
    (((volume + delta) * 10.).round() / 10.).clamp(0., 1.)
}

/// Display name of a voice id; unknown ids read as themselves.
pub(crate) fn voice_name(voices: &[TtsVoice], id: &str, l: Lang) -> String {
    voices
        .iter()
        .find(|v| v.id == id)
        .map(|v| v.name.clone())
        .unwrap_or_else(|| {
            let mut c = id.chars();
            match c.next() {
                Some(first) => first.to_uppercase().chain(c).collect(),
                None => l.t(Msg::VoiceFallback).into(),
            }
        })
}

/// What the reader says while the controller takes the text, by source.
pub(crate) fn taking_text(source: TtsSource, restore_clipboard: bool, l: Lang) -> &'static str {
    l.t(match (source, restore_clipboard) {
        (TtsSource::CopySelection, true) => Msg::TakingCopyRestore,
        (TtsSource::CopySelection, false) => Msg::TakingCopyKeep,
        (TtsSource::Clipboard, _) => Msg::TakingClipboard,
        _ => Msg::TakingSelection,
    })
}

/// First characters of a text for the confirmation, on a word boundary.
fn excerpt(text: &str, max_chars: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max_chars {
        return flat;
    }
    let cut = flat
        .char_indices()
        .nth(max_chars)
        .map_or(flat.len(), |(i, _)| i);
    let cut = flat[..cut].rfind(' ').filter(|i| *i > 0).unwrap_or(cut);
    format!("{}…", &flat[..cut])
}

// ------------------------------------------------------------------ text

/// Non-empty lines of `text` with their byte offsets. The controller joins
/// paragraphs with a blank line, so each item is a paragraph.
pub(crate) fn split_paragraphs(text: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut start = 0;
    for line in text.split('\n') {
        if !line.trim().is_empty() {
            out.push((start, line));
        }
        start += line.len() + 1;
    }
    out
}

/// `span` (bytes of the whole text) inside a paragraph that starts at
/// `start` and is `len` bytes long, in paragraph bytes.
pub(crate) fn local_range(span: TextSpan, start: usize, len: usize) -> Option<Range<usize>> {
    let a = span.start.max(start);
    let b = span.end.min(start + len);
    (a < b).then(|| a - start..b - start)
}

/// Pieces of `range` on each visual line of a wrapped paragraph. `lines`
/// holds, per visual line, its first byte and the x of that byte in the
/// unwrapped line; `len` and `width` close the last one. Returns the line
/// index and the x span relative to the line start.
pub(crate) fn line_pieces(
    lines: &[(usize, f32)],
    len: usize,
    width: f32,
    x_for: impl Fn(usize) -> f32,
    range: Range<usize>,
) -> Vec<(usize, f32, f32)> {
    let mut out = Vec::new();
    for (k, &(s, x0)) in lines.iter().enumerate() {
        let (e, x_end) = lines.get(k + 1).copied().unwrap_or((len, width));
        let a = range.start.max(s);
        let b = range.end.min(e);
        if a >= b {
            continue;
        }
        let xa = x_for(a) - x0;
        let xb = if b >= e { x_end - x0 } else { x_for(b) - x0 };
        if xb > xa {
            out.push((k, xa, xb));
        }
    }
    out
}

/// Window rectangles covering `range` of a laid-out paragraph.
fn range_rects(layout: &TextLayout, range: Range<usize>) -> Vec<Bounds<Pixels>> {
    let Some(line) = layout.line_layout_for_index(range.start) else {
        return Vec::new();
    };
    let origin = layout.bounds().origin;
    let lh = layout.line_height();
    let unwrapped = &line.unwrapped_layout;
    let mut lines = vec![(0usize, 0f32)];
    for b in line.wrap_boundaries() {
        if let Some(g) = unwrapped
            .runs
            .get(b.run_ix)
            .and_then(|r| r.glyphs.get(b.glyph_ix))
        {
            lines.push((g.index, f32::from(g.position.x)));
        }
    }
    line_pieces(
        &lines,
        unwrapped.len,
        f32::from(unwrapped.width),
        |i| f32::from(unwrapped.x_for_index(i)),
        range,
    )
    .into_iter()
    .map(|(k, xa, xb)| {
        Bounds::new(
            point(origin.x + px(xa), origin.y + lh * k as f32),
            size(px(xb - xa), lh),
        )
    })
    .collect()
}

/// Whether the view follows the reading.
#[derive(Debug, Default)]
struct Follow {
    /// The user scrolled the text; following resumes when the word comes
    /// back to the comfortable band or after an explicit jump.
    user_scrolled: bool,
    /// The followed word was inside the text area at the last paint.
    word_visible: bool,
}

/// Keeps `target` inside the comfortable band of the text area unless the
/// user scrolled away. Runs during paint; changes apply on the next frame.
fn follow_into_view(
    target: Bounds<Pixels>,
    scroll: &ScrollHandle,
    follow: &RefCell<Follow>,
    window: &mut Window,
) {
    let view = scroll.bounds();
    let h = view.size.height;
    if h <= px(1.) {
        return;
    }
    let mut f = follow.borrow_mut();
    let visible = target.top() >= view.top() && target.bottom() <= view.bottom();
    let in_band = target.top() >= view.top() + h * BAND_TOP
        && target.bottom() <= view.top() + h * BAND_BOTTOM;
    let mut redraw = false;
    if f.user_scrolled && in_band {
        f.user_scrolled = false;
        redraw = true;
    }
    if visible != f.word_visible {
        f.word_visible = visible;
        redraw = true;
    }
    if !f.user_scrolled && !in_band {
        let offset = scroll.offset();
        let wanted = view.top() + h * BAND_TARGET;
        let y = (offset.y + (wanted - target.top()))
            .max(-scroll.max_offset().y)
            .min(px(0.));
        if (y - offset.y).abs() > px(0.5) {
            scroll.set_offset(point(offset.x, y));
            redraw = true;
        }
    }
    if redraw {
        window.request_animation_frame();
    }
}

struct Paragraph {
    start: usize,
    text: SharedString,
}

/// A small panel over the card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Popover {
    Voice,
    Rate,
}

pub struct Reader {
    hub: Entity<Hub>,
    placement: OverlayPlacement,
    focus: FocusHandle,
    scroll: ScrollHandle,
    follow: Rc<RefCell<Follow>>,
    /// The text of the reading, a cached view of its own.
    text_view: Entity<ReaderText>,
    /// The reading the view state belongs to.
    reading: u64,
    popover: Option<Popover>,
    /// Voice picked for the next Regenerar; None keeps the current one.
    voice_choice: Option<String>,
    /// Position under the pointer while scrubbing, in ms.
    scrub: Rc<Cell<Option<u64>>>,
    /// UI-only message, e.g. the save dialog could not open.
    local_notice: Option<String>,
    input_region: Option<Bounds<Pixels>>,
    _subs: Vec<gpui::Subscription>,
}

impl Reader {
    pub fn new(
        hub: Entity<Hub>,
        placement: OverlayPlacement,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let reading = hub.read(cx).snap.tts.reading;
        let subs = vec![cx.observe(&hub, |_, _, cx| cx.notify())];
        let scroll = ScrollHandle::new();
        let follow = Rc::new(RefCell::new(Follow::default()));
        let text_view = cx.new(|_| ReaderText {
            hub: hub.clone(),
            scroll: scroll.clone(),
            follow: follow.clone(),
            text: None,
            paragraphs: Vec::new(),
            shown: None,
        });
        Self {
            hub,
            placement,
            focus: cx.focus_handle(),
            scroll,
            follow,
            text_view,
            reading,
            popover: None,
            voice_choice: None,
            scrub: Rc::new(Cell::new(None)),
            local_notice: None,
            input_region: None,
            _subs: subs,
        }
    }

    /// Gallery: open with a panel already shown.
    #[cfg_attr(not(feature = "screenshots"), allow(dead_code))]
    pub(crate) fn with_popover(mut self, popover: Popover) -> Self {
        self.popover = Some(popover);
        self
    }

    /// Gallery: a voice picked but not generated yet.
    #[cfg_attr(not(feature = "screenshots"), allow(dead_code))]
    pub(crate) fn with_voice_choice(mut self, voice: &str) -> Self {
        self.voice_choice = Some(voice.into());
        self
    }

    fn send(&self, cx: &App, command: Command) {
        self.hub.read(cx).send(command);
    }

    /// Navigation the user asked for: the view follows the reading again.
    fn navigate(&mut self, command: Command, cx: &mut Context<Self>) {
        self.follow.borrow_mut().user_scrolled = false;
        self.send(cx, command);
        self.text_view.update(cx, |_, cx| cx.notify());
        cx.notify();
    }

    /// A snapshot moved the reading: the text redraws only when what it
    /// shows (text, sentence, word, seekability) changed.
    /// Runs inside the hub's update, so it gets the marks instead of
    /// reading the hub.
    pub(crate) fn tick(&mut self, marks: Marks, cx: &mut Context<Self>) {
        self.text_view.update(cx, |text, cx| {
            if text.shown != Some(marks) {
                cx.notify();
            }
        });
        cx.notify();
    }

    fn rate(&self, cx: &App) -> f32 {
        self.hub.read(cx).snap.tts.rate
    }

    fn set_rate(&mut self, rate: f32, cx: &mut Context<Self>) {
        let rate = rate.clamp(RATE_MIN, RATE_MAX);
        if (rate - self.rate(cx)).abs() > f32::EPSILON {
            self.send(cx, Command::TtsSetRate(rate));
        }
        cx.notify();
    }

    fn toggle_popover(&mut self, popover: Popover, cx: &mut Context<Self>) {
        self.popover = if self.popover == Some(popover) {
            None
        } else {
            Some(popover)
        };
        cx.notify();
    }

    fn open_settings(&mut self, cx: &mut Context<Self>) {
        self.popover = None;
        self.hub
            .update(cx, |hub, cx| hub.show_settings_at(Section::Reading, cx));
    }

    /// Asks where to save the active version and sends `TtsExport`. The
    /// portal dialog may be missing; then the footer says so.
    fn save_audio(&mut self, cx: &mut Context<Self>) {
        let l = lang(cx);
        let linux = ui_state(cx).session.is_linux();
        // HOME on Unix, USERPROFILE on Windows.
        let dir = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/"));
        let answer = cx.prompt_for_new_path(&dir, Some(l.t(Msg::WavFileName)));
        cx.spawn(async move |this, cx| {
            let answer = answer.await;
            this.update(cx, |this, cx| {
                match answer {
                    Ok(Ok(Some(path))) => {
                        this.local_notice = None;
                        this.send(cx, Command::TtsExport(wav_path(path)));
                    }
                    Ok(Ok(None)) => {}
                    _ => {
                        let mut notice = l.t(Msg::SaveDialogFailed).to_string();
                        if linux {
                            notice.push(' ');
                            notice.push_str(l.t(Msg::LinuxSaveDialogPortal));
                        }
                        this.local_notice = Some(notice);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Resets per-reading view state.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let t = &self.hub.read(cx).snap.tts;
        let (reading, voice) = (t.reading, t.voice.clone());
        if reading != self.reading {
            self.reading = reading;
            self.scroll.set_offset(point(px(0.), px(0.)));
            *self.follow.borrow_mut() = Follow::default();
            self.voice_choice = None;
            self.popover = None;
            self.local_notice = None;
            self.scrub.set(None);
            self.text_view.update(cx, |_, cx| cx.notify());
        }
        if self.voice_choice.as_deref() == Some(voice.as_str()) {
            self.voice_choice = None;
        }
    }

    fn header(
        &self,
        t: &TtsSnapshot,
        mode: ReaderMode,
        demo: bool,
        compact: bool,
        p: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let l = lang(cx);
        let failed = t.phase == TtsPhase::Error;
        let player = mode == ReaderMode::Player;
        let queued = t.queued;
        let drag = draggable(
            div()
                .flex_1()
                .min_w(px(0.))
                .h_full()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(Overlay::grip(p))
                .child(
                    img(assets::art(assets::mark(20., p.dark)))
                        .flex_none()
                        .size(px(20.)),
                )
                .when(failed, |d| {
                    d.child(div().flex_none().size(px(8.)).rounded_full().bg(p.danger))
                })
                .child(
                    div()
                        .min_w(px(0.))
                        .truncate()
                        .font_family(DISPLAY_FONT)
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_size(px(13.))
                        .child(phase_label(t.phase, l)),
                )
                .when(demo, |d| d.child(demo_badge(p)))
                .when(queued > 0, |d| {
                    d.child(
                        div()
                            .flex_none()
                            .px(px(6.))
                            .h(px(18.))
                            .flex()
                            .items_center()
                            .rounded_full()
                            .bg(p.selected)
                            .text_size(px(11.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.text_2)
                            .child(tf!(l, Queued, n = queued)),
                    )
                }),
        );
        div()
            .id("reader-header")
            .flex()
            .flex_none()
            .items_center()
            .gap(px(4.))
            .h(px(HEADER_H))
            .pl(px(8.))
            .pr(px(if p.planet_corner {
                PLANET_HEADER_PAD
            } else {
                8.
            }))
            .border_b_1()
            .border_color(p.line)
            .child(drag)
            .when(player, |d| {
                d.child(self.voice_chip(t, p, cx)).child(
                    icon_button(
                        "reader-compact",
                        if compact { Icon::Expand } else { Icon::Compact },
                        p,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.popover = None;
                        let result = this.hub.update(cx, |hub, cx| {
                            hub.update_config(cx, |c| c.tts.compact = !c.tts.compact)
                        });
                        if let Err(e) = result {
                            this.local_notice = Some(e);
                        }
                        cx.notify();
                    })),
                )
            })
            .child(
                icon_button("reader-close", Icon::Close, p)
                    .on_click(cx.listener(|this, _, _, cx| this.send(cx, Command::TtsStop))),
            )
            .into_any_element()
    }

    fn voice_chip(&self, t: &TtsSnapshot, p: Palette, cx: &mut Context<Self>) -> Stateful<Div> {
        let l = lang(cx);
        let shown = self.voice_choice.clone().unwrap_or_else(|| t.voice.clone());
        let pending = self.voice_choice.is_some();
        let open = self.popover == Some(Popover::Voice);
        div()
            .id("reader-voice")
            .flex()
            .flex_none()
            .items_center()
            .gap(px(5.))
            .h(px(28.))
            .max_w(px(150.))
            .pl(px(9.))
            .pr(px(5.))
            .mr(px(2.))
            .rounded(px(8.))
            .bg(p.raised)
            .border_1()
            .border_color(if open { p.control } else { p.line })
            .cursor_pointer()
            .hover(move |s| s.border_color(p.control))
            .text_size(px(12.5))
            .font_weight(FontWeight::MEDIUM)
            .when(pending, |d| {
                d.child(div().flex_none().size(px(6.)).rounded_full().bg(p.orange))
            })
            .child(
                div()
                    .min_w(px(0.))
                    .truncate()
                    .child(voice_name(&t.voices, &shown, l)),
            )
            .child(icon(Icon::ChevronDown, 14., p.text_2))
            .on_click(cx.listener(|this, _, _, cx| this.toggle_popover(Popover::Voice, cx)))
    }

    /// The style's voice on the output level. With reduced motion, a still
    /// segmented meter: a continuous bar would read as a second progress bar.
    fn voice_row(&self, t: &TtsSnapshot, orbit: bool, p: Palette, cx: &App) -> Div {
        let reduce = ui_state(cx).reduce_motion;
        let live = t.phase == TtsPhase::Playing;
        let levels = &self.hub.read(cx).tts_levels;
        let row = div().flex_none().h(px(voice_row_h(orbit)));
        if reduce {
            let level = if live { levels.current() } else { 0. };
            return row
                .flex()
                .items_center()
                .justify_center()
                .child(meter(level, p));
        }
        row.child(voice(levels, p, live, false))
    }

    fn scrubber(&self, t: &TtsSnapshot, enabled: bool, p: Palette) -> impl IntoElement {
        let total = t.total_ms;
        let dragging = self.scrub.get();
        let position = dragging.unwrap_or(t.position_ms);
        let generated = t.generated.clone();
        let (scrub, hub, follow) = (self.scrub.clone(), self.hub.clone(), self.follow.clone());
        canvas(
            move |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
            move |bounds, hitbox, window, _| {
                let w = bounds.size.width;
                let mid = bounds.center().y;
                let x_at = |ms: u64| {
                    let f = if total == 0 {
                        0.
                    } else {
                        (ms as f32 / total as f32).clamp(0., 1.)
                    };
                    bounds.left() + w * f
                };
                let mut ahead = p.control;
                ahead.a = 0.55;
                bar(window, bounds.left(), bounds.right(), mid, 4., p.line);
                for &(a, b) in generated.iter() {
                    if b > a {
                        bar(window, x_at(a), x_at(b), mid, 4., ahead);
                    }
                }
                let at = x_at(position);
                bar(window, bounds.left(), at, mid, 4., p.voice);
                if total > 0 {
                    let r = if dragging.is_some() { 7. } else { 6. };
                    dot(window, at, mid, r + 2., p.surface);
                    dot(window, at, mid, r, p.voice);
                }
                if !enabled || total == 0 {
                    return;
                }
                window.set_cursor_style(CursorStyle::PointingHand, &hitbox);
                let left = bounds.left();
                let ms_at = move |x: Pixels| -> u64 {
                    let f = ((x - left) / w).clamp(0., 1.) as f64;
                    (f * total as f64).round() as u64
                };
                window.on_mouse_event({
                    let scrub = scrub.clone();
                    move |e: &MouseDownEvent, phase, window, cx| {
                        if phase.bubble()
                            && e.button == MouseButton::Left
                            && hitbox.is_hovered(window)
                        {
                            scrub.set(Some(ms_at(e.position.x)));
                            window.refresh();
                            cx.stop_propagation();
                        }
                    }
                });
                window.on_mouse_event({
                    let scrub = scrub.clone();
                    move |e: &MouseMoveEvent, phase, window, _| {
                        if phase.bubble() && scrub.get().is_some() {
                            if e.pressed_button == Some(MouseButton::Left) {
                                scrub.set(Some(ms_at(e.position.x)));
                            } else {
                                scrub.set(None);
                            }
                            window.refresh();
                        }
                    }
                });
                window.on_mouse_event(move |e: &MouseUpEvent, phase, window, cx| {
                    if phase.bubble()
                        && e.button == MouseButton::Left
                        && let Some(ms) = scrub.take()
                    {
                        follow.borrow_mut().user_scrolled = false;
                        hub.read(cx).send(Command::TtsSeek(ms));
                        window.refresh();
                    }
                });
            },
        )
        .size_full()
    }

    fn progress(&self, t: &TtsSnapshot, p: Palette) -> Div {
        let dragging = self.scrub.get();
        let position = dragging.unwrap_or(t.position_ms);
        let label = |text: String, color| {
            div()
                .flex_none()
                .min_w(px(42.))
                .font_family(MONO_FONT)
                .text_size(px(12.))
                .text_color(color)
                .child(text)
        };
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(10.))
            .h(px(PROGRESS_H))
            .child(label(
                fmt_time(position),
                if dragging.is_some() { p.text } else { p.text_2 },
            ))
            .child(div().flex_1().min_w(px(0.)).h_full().child(self.scrubber(
                t,
                has_audio(t) || t.total_ms > 0,
                p,
            )))
            .child(
                label(total_label(t.total_ms, t.total_exact), p.text_2)
                    .flex()
                    .justify_end(),
            )
    }

    fn transport_button(
        &self,
        id: &'static str,
        glyph: Icon,
        seconds: Option<u8>,
        make: impl Fn() -> Command + 'static,
        p: Palette,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        div()
            .id(id)
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .gap(px(1.))
            .h(px(32.))
            .min_w(px(32.))
            .px(px(4.))
            .rounded(px(8.))
            .cursor_pointer()
            .hover(move |s| s.bg(p.hover()))
            .active(move |s| s.bg(p.selected))
            .child(icon(glyph, 18., p.text))
            .when_some(seconds, |d, s| {
                d.child(
                    div()
                        .font_family(MONO_FONT)
                        .text_size(px(11.))
                        .text_color(p.text_2)
                        .child(s.to_string()),
                )
            })
            .on_click(cx.listener(move |this, _, _, cx| this.navigate(make(), cx)))
    }

    fn transport(&self, t: &TtsSnapshot, skip: u8, p: Palette, cx: &mut Context<Self>) -> Div {
        let l = lang(cx);
        let glyph = match t.phase {
            TtsPhase::Finished => Icon::Retry,
            phase if phase.audible() => Icon::Pause,
            _ => Icon::Play,
        };
        let skip_ms = i32::from(skip);
        let open = self.popover == Some(Popover::Rate);
        let regen_voice = self.voice_choice.clone().unwrap_or_else(|| t.voice.clone());
        let pending = self.voice_choice.is_some();
        let regen = icon_label_button(
            "reader-regenerate",
            Icon::Retry,
            l.t(Msg::Regenerate),
            Kind::Secondary,
            p,
        )
        .h(px(30.))
        .px(px(10.))
        .text_size(px(13.))
        .when(pending, |d| {
            d.child(div().flex_none().size(px(6.)).rounded_full().bg(p.orange))
        });
        let regen = if regen_voice.is_empty() {
            disabled(regen)
        } else {
            regen.on_click(cx.listener(move |this, _, _, cx| {
                this.voice_choice = None;
                let voice = regen_voice.clone();
                this.navigate(Command::TtsRegenerate { voice }, cx);
            }))
        };
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(2.))
            .h(px(TRANSPORT_H))
            .child(self.transport_button(
                "reader-prev",
                Icon::PrevSentence,
                None,
                || Command::TtsSentence(-1),
                p,
                cx,
            ))
            .child(self.transport_button(
                "reader-back",
                Icon::SkipBack,
                Some(skip),
                move || Command::TtsSkip(-skip_ms),
                p,
                cx,
            ))
            .child(
                div()
                    .id("reader-play")
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .mx(px(2.))
                    .size(px(40.))
                    .rounded_full()
                    .bg(p.orange)
                    .cursor_pointer()
                    .hover(move |s| s.bg(p.orange_hover))
                    .active(move |s| s.bg(p.orange_pressed))
                    .child(icon(glyph, 18., p.on_orange))
                    .on_click(cx.listener(|this, _, _, cx| this.navigate(Command::TtsToggle, cx))),
            )
            .child(self.transport_button(
                "reader-forward",
                Icon::SkipForward,
                Some(skip),
                move || Command::TtsSkip(skip_ms),
                p,
                cx,
            ))
            .child(self.transport_button(
                "reader-next",
                Icon::NextSentence,
                None,
                || Command::TtsSentence(1),
                p,
                cx,
            ))
            .child(div().flex_1().min_w(px(4.)))
            .child(
                div()
                    .id("reader-rate")
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(3.))
                    .h(px(30.))
                    .pl(px(9.))
                    .pr(px(5.))
                    .mr(px(4.))
                    .rounded(px(8.))
                    .bg(p.raised)
                    .border_1()
                    .border_color(if open { p.control } else { p.line })
                    .cursor_pointer()
                    .hover(move |s| s.border_color(p.control))
                    .font_family(MONO_FONT)
                    .text_size(px(12.5))
                    .child(fmt_rate(t.rate, l))
                    .child(icon(Icon::ChevronDown, 13., p.text_2))
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_popover(Popover::Rate, cx))),
            )
            .child(regen)
    }

    /// One line under the transport: the error, a notice, the generation
    /// status, a pending voice or, when nothing else, what was sent.
    fn info(
        &self,
        t: &TtsSnapshot,
        demo: bool,
        show_cost: bool,
        l: Lang,
    ) -> Option<(bool, String)> {
        if t.phase == TtsPhase::Error {
            let error = t.error.clone().unwrap_or_else(|| t.status.clone());
            if !error.trim().is_empty() {
                return Some((true, error));
            }
        }
        if let Some(n) = self.local_notice.clone().or_else(|| t.notice.clone()) {
            return Some((false, n));
        }
        if matches!(t.phase, TtsPhase::Connecting | TtsPhase::Buffering) {
            let status = if t.status.trim().is_empty() {
                l.t(match t.phase {
                    TtsPhase::Connecting => Msg::GeneratingWithGrok,
                    _ => Msg::PlaybackCaughtUp,
                })
                .to_string()
            } else {
                t.status.clone()
            };
            return Some((false, status));
        }
        if let Some(v) = &self.voice_choice {
            return Some((
                false,
                tf!(l, PickedVoice, voice = voice_name(&t.voices, v, l)),
            ));
        }
        if t.generating && !t.status.trim().is_empty() {
            return Some((false, t.status.clone()));
        }
        if t.versions.len() > 1 {
            return Some((false, tf!(l, VersionsCount, n = t.versions.len())));
        }
        if demo {
            return Some((false, l.t(Msg::ReaderDemo).into()));
        }
        (show_cost && t.chars_sent > 0).then(|| {
            (
                false,
                tf!(l, CharsSentToGrok, n = format::count(t.chars_sent, l)),
            )
        })
    }

    fn footer(&self, t: &TtsSnapshot, show_cost: bool, p: Palette, cx: &mut Context<Self>) -> Div {
        let l = lang(cx);
        let small = |id: &'static str, label: &'static str| {
            button(id, label, Kind::Ghost, p)
                .h(px(26.))
                .px(px(8.))
                .text_size(px(12.5))
        };
        let save = small("reader-save", l.t(Msg::SaveAudio));
        let save = if has_audio(t) {
            save.on_click(cx.listener(|this, _, _, cx| this.save_audio(cx)))
        } else {
            disabled(save)
        };
        let copy = small("reader-copy", l.t(Msg::CopyText));
        let copy = if t.text.is_empty() {
            disabled(copy)
        } else {
            copy.on_click(cx.listener(|this, _, _, cx| this.send(cx, Command::TtsCopyText)))
        };
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(2.))
            .h(px(FOOTER_H))
            .ml(px(-6.))
            .child(copy)
            .child(save)
            .child(
                small("reader-settings", l.t(Msg::Settings))
                    .on_click(cx.listener(|this, _, _, cx| this.open_settings(cx))),
            )
            .child(div().flex_1().min_w(px(6.)))
            .when(show_cost, |d| {
                d.child(
                    div()
                        .min_w(px(0.))
                        .truncate()
                        .text_size(px(12.))
                        .text_color(p.text_2)
                        .child(cost_label(t.estimated_usd, l)),
                )
            })
    }

    fn player(
        &self,
        t: &TtsSnapshot,
        compact: bool,
        flags: (bool, bool, u8),
        p: Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let (demo, show_cost, skip) = flags;
        let orbit = p.voice_style == VoiceStyle::Orbit;
        let info = self.info(t, demo, show_cost, lang(cx));
        // Compact cards keep the line (and a second one for errors) so the
        // height stays put; full ones let the text take the room.
        let info_el = match (&info, compact) {
            (None, false) => None,
            (info, _) => {
                let lines = if t.error.is_some() { 2 } else { 1 };
                let (error, text) = info.clone().unwrap_or((false, String::new()));
                Some(
                    div()
                        .flex()
                        .flex_none()
                        .items_start()
                        .gap(px(6.))
                        .when(compact, |d| d.h(px(INFO_H * lines as f32)))
                        .text_size(px(12.))
                        .line_height(px(INFO_H))
                        .text_color(if error { p.text } else { p.text_2 })
                        .when(error, |d| {
                            d.child(
                                div()
                                    .flex_none()
                                    .mt(px(5.))
                                    .size(px(7.))
                                    .rounded_full()
                                    .bg(p.danger),
                            )
                        })
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .when(error, |d| d.line_clamp(2))
                                .when(!error, |d| d.truncate())
                                .child(text),
                        ),
                )
            }
        };
        let controls = div()
            .flex()
            .flex_col()
            .flex_none()
            .px(px(PAD_X))
            .pt(px(PAD_TOP))
            .pb(px(PAD_BOTTOM))
            .child(self.voice_row(t, orbit, p, cx))
            .child(div().h(px(GAP_VOICE)))
            .child(self.progress(t, p))
            .child(div().h(px(GAP_PROGRESS)))
            .child(self.transport(t, skip, p, cx))
            .child(div().h(px(GAP_TRANSPORT)))
            .when_some(info_el, |d, info| {
                d.child(info).child(div().h(px(GAP_INFO)))
            })
            .child(self.footer(t, show_cost, p, cx));
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .when(!compact, |d| {
                d.child(
                    self.text_view.clone().cached(
                        gpui::StyleRefinement::default()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_h(px(0.))
                            .w_full(),
                    ),
                )
            })
            .child(controls)
    }

    fn mars(&self) -> Div {
        div().flex_none().child(
            img(assets::art(assets::mars(30.)))
                .flex_none()
                .size(px(30.)),
        )
    }

    fn waiting(
        &self,
        t: &TtsSnapshot,
        shortcut: String,
        p: Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let l = lang(cx);
        let reading = t.phase == TtsPhase::Reading;
        let (source, restore) = {
            let c = &self.hub.read(cx).snap.config.tts;
            (c.source, c.restore_clipboard)
        };
        // While reading the selection the header already says so.
        let (title, detail): (Option<&str>, String) = if reading {
            (None, taking_text(source, restore, l).into())
        } else {
            (
                Some(l.t(Msg::NothingToRead)),
                tf!(l, SelectAndPress, key = shortcut),
            )
        };
        let action = if reading {
            button("reader-cancel", l.t(Msg::Cancel), Kind::Ghost, p)
                .on_click(cx.listener(|this, _, _, cx| this.send(cx, Command::TtsStop)))
        } else {
            icon_label_button(
                "reader-clipboard",
                Icon::Paste,
                l.t(Msg::ReadClipboard),
                Kind::Secondary,
                p,
            )
            .on_click(cx.listener(|this, _, _, cx| this.send(cx, Command::ReadClipboard)))
        };
        div()
            .flex()
            .flex_1()
            .items_center()
            .gap(px(10.))
            .px(px(PAD_X))
            .child(self.mars())
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .when_some(title, |d, title| {
                        d.child(
                            div()
                                .font_weight(FontWeight::MEDIUM)
                                .truncate()
                                .child(title),
                        )
                    })
                    .child(
                        div()
                            .text_size(px(13.))
                            .line_height(px(18.))
                            .text_color(p.text_2)
                            .line_clamp(2)
                            .child(detail),
                    ),
            )
            .child(action)
    }

    fn confirm(
        &self,
        t: &TtsSnapshot,
        demo: bool,
        max_chars: u32,
        p: Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let l = lang(cx);
        let c = t.confirm.unwrap_or(TtsConfirm {
            chars: t.chars,
            estimated_usd: estimate_usd(t.chars),
            truncated: false,
        });
        let preview = excerpt(&t.text, 360);
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .gap(px(10.))
            .px(px(14.))
            .pt(px(12.))
            .pb(px(12.))
            .when(!preview.is_empty(), |d| {
                d.child(
                    div()
                        .flex_none()
                        .p(px(10.))
                        .rounded(px(10.))
                        .bg(p.raised)
                        .border_1()
                        .border_color(p.line)
                        .text_size(px(13.5))
                        .line_height(px(19.))
                        .text_color(p.text_2)
                        .line_clamp(4)
                        .text_ellipsis()
                        .child(preview),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_baseline()
                    .gap_x(px(10.))
                    .child(
                        div()
                            .font_family(DISPLAY_FONT)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_size(px(18.))
                            .child(tf!(l, Characters, n = format::count(c.chars, l))),
                    )
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(p.text_2)
                            .child(cost_label(c.estimated_usd, l)),
                    ),
            )
            .when(c.truncated, |d| {
                d.child(hint(
                    tf!(l, OverLimit, n = format::count(u64::from(max_chars), l)),
                    p,
                ))
            })
            .child(hint(
                l.t(if demo {
                    Msg::ConfirmDemo
                } else {
                    Msg::ConfirmCost
                }),
                p,
            ))
            .child(div().flex_1())
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        icon_label_button(
                            "reader-confirm",
                            Icon::Read,
                            l.t(Msg::Read),
                            Kind::Primary,
                            p,
                        )
                        .on_click(
                            cx.listener(|this, _, _, cx| this.send(cx, Command::ConfirmReading)),
                        ),
                    )
                    .child(
                        button("reader-confirm-cancel", l.t(Msg::Cancel), Kind::Ghost, p).on_click(
                            cx.listener(|this, _, _, cx| this.send(cx, Command::TtsStop)),
                        ),
                    ),
            )
    }

    fn failed(
        &self,
        t: &TtsSnapshot,
        fallback_voice: String,
        p: Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let l = lang(cx);
        let message = t
            .error
            .clone()
            .filter(|e| !e.trim().is_empty())
            .unwrap_or_else(|| {
                if t.status.trim().is_empty() {
                    l.t(Msg::ReadingFailed).into()
                } else {
                    t.status.clone()
                }
            });
        let first = if t.text.is_empty() {
            icon_label_button(
                "reader-clipboard",
                Icon::Paste,
                l.t(Msg::ReadClipboard),
                Kind::Secondary,
                p,
            )
            .on_click(cx.listener(|this, _, _, cx| this.send(cx, Command::ReadClipboard)))
        } else {
            let voice = if t.voice.is_empty() {
                fallback_voice
            } else {
                t.voice.clone()
            };
            icon_label_button(
                "reader-retry",
                Icon::Retry,
                l.t(Msg::Retry),
                Kind::Secondary,
                p,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                let voice = voice.clone();
                this.send(cx, Command::TtsRegenerate { voice })
            }))
        };
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .gap(px(10.))
            .px(px(PAD_X))
            .py(px(12.))
            .child(
                div()
                    .id("reader-error")
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .text_size(px(13.5))
                    .line_height(px(19.))
                    .children(
                        message
                            .lines()
                            .map(str::trim)
                            .filter(|l| !l.is_empty())
                            .map(|l| div().child(SharedString::from(l.to_string())))
                            .collect::<Vec<_>>(),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(8.))
                    .child(first)
                    .child(
                        button(
                            "reader-open-settings",
                            l.t(Msg::ReadingSettings),
                            Kind::Ghost,
                            p,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.open_settings(cx))),
                    ),
            )
    }

    /// `info_lines`: lines the info row takes under the transport (0 when
    /// hidden), so the rate panel opens right above the transport.
    fn popover(
        &self,
        t: &TtsSnapshot,
        card_h: f32,
        (compact, info_lines): (bool, u8),
        p: Palette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let which = self.popover?;
        let scrim = div()
            .id("reader-scrim")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .occlude()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.popover = None;
                    cx.notify();
                }),
            );
        let panel = div()
            .absolute()
            .right(px(8.))
            .occlude()
            .flex()
            .flex_col()
            .rounded(px(12.))
            .bg(p.raised)
            .border_1()
            .border_color(p.control)
            .shadow(vec![shadow(p)]);
        let panel = match which {
            Popover::Voice => panel
                .top(px(HEADER_H + 4.))
                .w(px(240.))
                .max_h(px((card_h - HEADER_H - 12.).max(80.)))
                .child(self.voice_menu(t, !compact, p, cx)),
            // Above the transport; compact cards are short, so the panel
            // hangs from the top and covers the header instead.
            Popover::Rate if compact => panel
                .top(px(4.))
                .w(px(RATE_MENU_W))
                .child(self.rate_menu(t, p, cx)),
            Popover::Rate => {
                let bottom = PAD_BOTTOM
                    + FOOTER_H
                    + if info_lines > 0 {
                        INFO_H * info_lines as f32 + GAP_INFO
                    } else {
                        0.
                    }
                    + GAP_TRANSPORT
                    + TRANSPORT_H
                    + 2.;
                panel
                    .bottom(px(bottom))
                    .w(px(RATE_MENU_W))
                    .child(self.rate_menu(t, p, cx))
            }
        };
        Some(
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .child(scrim)
                .child(panel)
                .into_any_element(),
        )
    }

    /// `footer`: the cost reminder under the list; compact cards need the
    /// room for the list itself.
    fn voice_menu(&self, t: &TtsSnapshot, footer: bool, p: Palette, cx: &mut Context<Self>) -> Div {
        let l = lang(cx);
        let chosen = self.voice_choice.clone().unwrap_or_else(|| t.voice.clone());
        let label = |text: &'static str| {
            div()
                .px(px(8.))
                .pt(px(8.))
                .pb(px(2.))
                .text_size(px(11.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(p.text_2)
                .child(text.to_uppercase())
        };
        let versions = (t.versions.len() > 1).then(|| {
            t.versions
                .iter()
                .map(|v| {
                    let id = v.id;
                    menu_row(
                        ("version", id as usize),
                        v.label.clone(),
                        None,
                        v.id == t.active_version,
                        p,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.popover = None;
                        this.voice_choice = None;
                        this.navigate(Command::TtsUseVersion(id), cx);
                    }))
                    .into_any_element()
                })
                .collect::<Vec<_>>()
        });
        let mut voices: Vec<TtsVoice> = t.voices.as_ref().clone();
        if !voices.iter().any(|v| v.id == chosen) && !chosen.is_empty() {
            voices.insert(
                0,
                TtsVoice {
                    id: chosen.clone(),
                    name: voice_name(&[], &chosen, l),
                    language: String::new(),
                    custom: false,
                },
            );
        }
        let rows: Vec<AnyElement> = voices
            .into_iter()
            .enumerate()
            .map(|(i, v)| {
                let detail = if v.custom {
                    Some(l.t(Msg::Custom).to_string())
                } else if v.language.is_empty() {
                    None
                } else {
                    Some(v.language.clone())
                };
                let id = v.id.clone();
                menu_row(("voice", i), v.name, detail, v.id == chosen, p)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let current = this.hub.read(cx).snap.tts.voice.clone();
                        this.voice_choice = (id != current).then(|| id.clone());
                        this.popover = None;
                        cx.notify();
                    }))
                    .into_any_element()
            })
            .collect();
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .child(
                div()
                    .id("reader-voice-list")
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .p(px(6.))
                    .flex()
                    .flex_col()
                    .gap(px(1.))
                    .when_some(versions, |d, rows| {
                        d.child(label(l.t(Msg::VersionsOfReading)))
                            .children(rows)
                            .child(
                                div()
                                    .px(px(8.))
                                    .pb(px(4.))
                                    .text_size(px(11.5))
                                    .text_color(p.text_2)
                                    .child(l.t(Msg::VersionSwitchFree)),
                            )
                    })
                    .child(label(l.t(Msg::VoiceToRegenerate)))
                    .children(rows),
            )
            .when(footer, |d| {
                d.child(
                    div()
                        .flex_none()
                        .px(px(12.))
                        .py(px(8.))
                        .border_t_1()
                        .border_color(p.line)
                        .text_size(px(11.5))
                        .line_height(px(16.))
                        .text_color(p.text_2)
                        .child(l.t(Msg::VoiceMenuHint)),
                )
            })
    }

    fn rate_menu(&self, t: &TtsSnapshot, p: Palette, cx: &mut Context<Self>) -> Div {
        let l = lang(cx);
        let rate = t.rate;
        let volume = t.volume;
        let row = |label: &'static str, stepper: Div| {
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(
                    div()
                        .flex_1()
                        .text_size(px(12.5))
                        .font_weight(FontWeight::MEDIUM)
                        .child(label),
                )
                .child(stepper)
        };
        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .p(px(10.))
            .child(row(
                l.t(Msg::Speed),
                stepper(
                    "rate",
                    fmt_rate(rate, l),
                    rate > RATE_MIN,
                    rate < RATE_MAX,
                    cx.listener(move |this, _, _, cx| {
                        this.set_rate(step_rate(rate, -RATE_STEP), cx)
                    }),
                    cx.listener(move |this, _, _, cx| {
                        this.set_rate(step_rate(rate, RATE_STEP), cx)
                    }),
                    p,
                ),
            ))
            .child(div().flex().gap(px(4.)).children(RATE_PRESETS.map(|r| {
                let selected = (r - rate).abs() < 0.001;
                div()
                    .id(("preset", (r * 100.) as usize))
                    .flex_1()
                    .flex()
                    .justify_center()
                    .py(px(3.))
                    .rounded(px(7.))
                    .border_1()
                    .border_color(if selected { p.control } else { p.line })
                    .when(selected, |d| d.bg(p.selected))
                    .cursor_pointer()
                    .hover(move |s| s.bg(p.hover()))
                    .font_family(MONO_FONT)
                    .text_size(px(12.))
                    .text_color(if selected { p.text } else { p.text_2 })
                    .child(fmt_rate(r, l))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_rate(r, cx);
                        this.popover = None;
                    }))
            })))
            .child(row(
                l.t(Msg::Volume),
                stepper(
                    "volume",
                    tf!(l, PercentValue, v = (volume * 100.).round() as i32),
                    volume > 0.,
                    volume < 1.,
                    cx.listener(move |this, _, _, cx| {
                        this.send(cx, Command::TtsSetVolume(step_volume(volume, -VOLUME_STEP)))
                    }),
                    cx.listener(move |this, _, _, cx| {
                        this.send(cx, Command::TtsSetVolume(step_volume(volume, VOLUME_STEP)))
                    }),
                    p,
                ),
            ))
    }
}

/// What the text draws besides the glyphs; a tick that changes it redraws
/// the text, any other tick reuses the cached view.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Marks {
    reading: u64,
    text: usize,
    sentence: Option<TextSpan>,
    word: Option<TextSpan>,
    seekable: bool,
}

impl Marks {
    pub(crate) fn of(t: &TtsSnapshot) -> Self {
        Self {
            reading: t.reading,
            text: Arc::as_ptr(&t.text) as usize,
            sentence: t.current_sentence.and_then(|i| t.sentences.get(i)).copied(),
            word: t.current_word.and_then(|i| t.words.get(i)).map(|w| w.span),
            seekable: t.phase.has_reading() || has_audio(t),
        }
    }
}

/// The text of the reading as its own view, embedded with
/// `Entity::cached`: wave animation frames and position ticks reuse its
/// layout and paint, so a 20 000 character text costs nothing between words.
pub(crate) struct ReaderText {
    hub: Entity<Hub>,
    scroll: ScrollHandle,
    follow: Rc<RefCell<Follow>>,
    /// The text the paragraphs were cut from, by identity.
    text: Option<Arc<String>>,
    paragraphs: Vec<Paragraph>,
    /// Marks of the last render.
    shown: Option<Marks>,
}

impl ReaderText {
    /// Recuts the paragraphs when the text changes.
    fn sync(&mut self, text: &Arc<String>) {
        let same = self
            .text
            .as_ref()
            .is_some_and(|k| Arc::ptr_eq(k, text) || k.as_str() == text.as_str());
        if !same {
            self.paragraphs = split_paragraphs(text)
                .into_iter()
                .map(|(start, s)| Paragraph {
                    start,
                    text: SharedString::from(s.to_string()),
                })
                .collect();
        }
        self.text = Some(text.clone());
    }

    fn paragraph(
        &self,
        i: usize,
        para: &Paragraph,
        marks: (Option<TextSpan>, Option<TextSpan>),
        seekable: bool,
        p: Palette,
    ) -> AnyElement {
        let len = para.text.len();
        let (sentence, word) = marks;
        let follow_sentence = word.is_none();
        let s = sentence.and_then(|sp| local_range(sp, para.start, len));
        let w = word.and_then(|sp| local_range(sp, para.start, len));
        let styled = StyledText::new(para.text.clone());
        let layout = styled.layout().clone();
        let mut d = div().id(("para", i)).relative().w_full();
        if s.is_some() || w.is_some() {
            let (scroll, follow, layout) =
                (self.scroll.clone(), self.follow.clone(), layout.clone());
            d = d.child(
                canvas(
                    |_, _, _| (),
                    move |_, (), window, _| {
                        if let Some(r) = s.clone() {
                            let rects = range_rects(&layout, r);
                            for b in &rects {
                                window.paint_quad(
                                    fill(
                                        Bounds::new(
                                            point(b.left() - px(1.), b.top() + px(1.)),
                                            size(b.size.width + px(2.), b.size.height - px(2.)),
                                        ),
                                        p.mark_sentence,
                                    )
                                    .corner_radii(px(4.)),
                                );
                            }
                            if follow_sentence && let Some(first) = rects.first() {
                                follow_into_view(*first, &scroll, &follow, window);
                            }
                        }
                        if let Some(r) = w.clone() {
                            let rects = range_rects(&layout, r);
                            for b in &rects {
                                window.paint_quad(
                                    fill(
                                        Bounds::new(
                                            point(b.left() - px(2.), b.top() + px(1.)),
                                            size(b.size.width + px(4.), b.size.height - px(2.)),
                                        ),
                                        p.mark_word,
                                    )
                                    .corner_radii(px(5.)),
                                );
                            }
                            if let Some(first) = rects.first() {
                                follow_into_view(*first, &scroll, &follow, window);
                            }
                        }
                    },
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            );
        }
        if seekable {
            let (hub, follow, start) = (self.hub.clone(), self.follow.clone(), para.start);
            d = d.cursor_pointer().on_click(move |event, _, cx| {
                let ix = match layout.index_for_position(event.position()) {
                    Ok(ix) | Err(ix) => ix.min(len),
                };
                follow.borrow_mut().user_scrolled = false;
                hub.read(cx).send(Command::TtsSeekText(start + ix));
            });
        }
        d.child(styled).into_any_element()
    }

    fn text_area(&self, t: &TtsSnapshot, p: Palette, cx: &mut Context<Self>) -> AnyElement {
        let l = lang(cx);
        let sentence = t.current_sentence.and_then(|i| t.sentences.get(i)).copied();
        let word = t.current_word.and_then(|i| t.words.get(i)).map(|w| w.span);
        let seekable = t.phase.has_reading() || has_audio(t);
        let lost = {
            let f = self.follow.borrow();
            f.user_scrolled && !f.word_visible && (sentence.is_some() || word.is_some())
        };
        perf::bump(&perf::READER_TEXTS);
        let items: Vec<AnyElement> = if self.paragraphs.is_empty() {
            vec![
                div()
                    .text_color(p.text_2)
                    .child(l.t(Msg::NoTextToShow))
                    .into_any_element(),
            ]
        } else {
            self.paragraphs
                .iter()
                .enumerate()
                .map(|(i, para)| self.paragraph(i, para, (sentence, word), seekable, p))
                .collect()
        };
        div()
            .relative()
            .flex_1()
            .min_h(px(0.))
            .flex()
            .flex_col()
            .border_b_1()
            .border_color(p.line)
            .child(
                div()
                    .id("reader-text")
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .on_scroll_wheel(cx.listener(|this, _, _, _| {
                        this.follow.borrow_mut().user_scrolled = true;
                    }))
                    .px(px(16.))
                    .py(px(12.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(10.))
                            .text_size(px(15.))
                            .line_height(px(23.))
                            .children(items),
                    ),
            )
            .when(lost, |d| {
                d.child(
                    div().absolute().right(px(12.)).bottom(px(10.)).child(
                        div()
                            .id("follow-reading")
                            .flex()
                            .items_center()
                            .gap(px(5.))
                            .h(px(26.))
                            .px(px(10.))
                            .rounded_full()
                            .bg(p.raised)
                            .border_1()
                            .border_color(p.control)
                            .shadow(vec![shadow(p)])
                            .cursor_pointer()
                            .hover(move |s| s.bg(p.selected))
                            .text_size(px(12.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(icon(Icon::Read, 13., p.text))
                            .child(l.t(Msg::FollowReading))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.follow.borrow_mut().user_scrolled = false;
                                cx.notify();
                            })),
                    ),
                )
            })
            .into_any_element()
    }
}

impl Render for ReaderText {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = ui_state(cx).palette;
        let t = self.hub.read(cx).snap.tts.clone();
        self.sync(&t.text);
        self.shown = Some(Marks::of(&t));
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(self.text_area(&t, p, cx))
    }
}

/// Picks `.wav` when the user typed a name without extension.
fn wav_path(path: PathBuf) -> PathBuf {
    if path.extension().is_none() {
        path.with_extension("wav")
    } else {
        path
    }
}

const METER_SEGMENTS: usize = 12;

/// Lit segments for a 0..=1 level; any audible level lights one.
pub(crate) fn meter_lit(level: f32) -> usize {
    if level <= 0. || !level.is_finite() {
        0
    } else {
        ((level.min(1.) * METER_SEGMENTS as f32).ceil() as usize).clamp(1, METER_SEGMENTS)
    }
}

/// Still output meter for reduced motion: a speaker and twelve segments.
fn meter(level: f32, p: Palette) -> Div {
    let lit = meter_lit(level);
    let mut off = p.voice;
    off.a = 0.18;
    div()
        .flex()
        .items_center()
        .gap(px(10.))
        .child(icon(Icon::Read, 16., p.text_2))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(3.))
                .children((0..METER_SEGMENTS).map(|i| {
                    div().w(px(10.)).h(px(8.)).rounded(px(2.)).bg(if i < lit {
                        p.voice
                    } else {
                        off
                    })
                })),
        )
}

pub(crate) fn shadow(p: Palette) -> BoxShadow {
    BoxShadow {
        color: p.shadow,
        offset: point(px(0.), px(4.)),
        blur_radius: px(14.),
        spread_radius: px(0.),
        inset: false,
    }
}

fn bar(window: &mut Window, from: Pixels, to: Pixels, mid: Pixels, h: f32, color: gpui::Rgba) {
    if to <= from {
        return;
    }
    window.paint_quad(
        fill(
            Bounds::new(point(from, mid - px(h / 2.)), size(to - from, px(h))),
            color,
        )
        .corner_radii(px(h / 2.)),
    );
}

fn dot(window: &mut Window, x: Pixels, y: Pixels, r: f32, color: gpui::Rgba) {
    window.paint_quad(
        fill(
            Bounds::new(point(x - px(r), y - px(r)), size(px(r * 2.), px(r * 2.))),
            color,
        )
        .corner_radii(px(r)),
    );
}

pub(crate) fn menu_row(
    id: impl Into<gpui::ElementId>,
    title: impl Into<SharedString>,
    detail: Option<String>,
    selected: bool,
    p: Palette,
) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .gap(px(8.))
        .h(px(30.))
        .px(px(8.))
        .rounded(px(7.))
        .cursor_pointer()
        .when(selected, |d| d.bg(p.selected))
        .hover(move |s| s.bg(p.hover()))
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .truncate()
                .text_size(px(13.))
                .font_weight(if selected {
                    FontWeight::SEMIBOLD
                } else {
                    FontWeight::MEDIUM
                })
                .child(title.into()),
        )
        .when_some(detail, |d, text| {
            d.child(
                div()
                    .flex_none()
                    .max_w(px(96.))
                    .truncate()
                    .text_size(px(11.5))
                    .text_color(p.text_2)
                    .child(text),
            )
        })
        .when(selected, |d| d.child(icon(Icon::Check, 14., p.text)))
}

/// `[−] value [+]`, the ends disabled at the bounds.
pub(crate) fn stepper(
    id: &'static str,
    value: impl Into<SharedString>,
    can_down: bool,
    can_up: bool,
    on_down: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    on_up: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    p: Palette,
) -> Div {
    let end = |suffix: &'static str, glyph: Icon, enabled: bool| {
        let b =
            icon_button((id, if suffix == "down" { 0usize } else { 1 }), glyph, p).size(px(26.));
        if enabled { b } else { disabled(b) }
    };
    let down = end("down", Icon::Minus, can_down);
    let up = end("up", Icon::Plus, can_up);
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(2.))
        .p(px(2.))
        .rounded(px(9.))
        .bg(p.surface)
        .border_1()
        .border_color(p.line)
        .child(if can_down {
            down.on_click(on_down)
        } else {
            down
        })
        .child(
            div()
                .min_w(px(56.))
                .flex()
                .justify_center()
                .font_family(MONO_FONT)
                .text_size(px(12.5))
                .child(value.into()),
        )
        .child(if can_up { up.on_click(on_up) } else { up })
}

impl Render for Reader {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        perf::bump(&perf::READER_RENDERS);
        let p = ui_state(cx).palette;
        self.sync(cx);
        let orbit = p.voice_style == VoiceStyle::Orbit;
        let (t, demo, compact, show_cost, skip, shortcut, max_chars, fallback_voice, card_h) = {
            let snap = &self.hub.read(cx).snap;
            let c = &snap.config.tts;
            (
                snap.tts.clone(),
                snap.demo,
                c.compact,
                c.show_cost,
                c.skip_seconds,
                c.read_shortcut.clone(),
                c.max_chars,
                c.voice.clone(),
                reader_height(snap, orbit),
            )
        };
        // On Wayland the window keeps the tallest card (see
        // `fixed_surface`); only the card takes input.
        let bottom = fixed_surface() && self.placement.card_at_bottom();
        let s = window.viewport_size();
        let card = px(card_h).min(s.height);
        let top = if bottom { s.height - card } else { px(0.) };
        let region = Bounds::new(point(px(0.), top), size(s.width, card));
        // Only Wayland has input regions; elsewhere the window is the card.
        if fixed_surface() && self.input_region != Some(region) {
            window.set_input_region(Some(&[region]));
            self.input_region = Some(region);
        }
        let mode = reader_mode(&t);
        let header = self.header(&t, mode, demo, compact, p, cx);
        let body = match mode {
            ReaderMode::Waiting => self.waiting(&t, shortcut, p, cx),
            ReaderMode::Confirm => self.confirm(&t, demo, max_chars, p, cx),
            ReaderMode::Failed => self.failed(&t, fallback_voice, p, cx),
            ReaderMode::Player => self.player(&t, compact, (demo, show_cost, skip), p, cx),
        };
        let info_lines = match self.info(&t, demo, show_cost, lang(cx)) {
            Some((true, _)) => 2,
            Some(_) => 1,
            None if compact => 1,
            None => 0,
        };
        let popover = (mode == ReaderMode::Player)
            .then(|| self.popover(&t, f32::from(card), (compact, info_lines), p, cx))
            .flatten();
        div()
            .size_full()
            .flex()
            .flex_col()
            .when(bottom, |d| d.justify_end())
            .font_family(UI_FONT)
            .text_size(px(14.))
            .text_color(p.text)
            .child(
                div()
                    .id("reader-card")
                    .track_focus(&self.focus)
                    .key_context(CONTEXT)
                    .capture_any_mouse_down(cx.listener(|this, _, window, cx| {
                        this.focus.focus(window, cx);
                    }))
                    .on_action(cx.listener(|this, _: &TogglePlay, _, cx| {
                        this.navigate(Command::TtsToggle, cx)
                    }))
                    .on_action(cx.listener(|this, _: &SkipBack, _, cx| {
                        let s = i32::from(this.hub.read(cx).snap.config.tts.skip_seconds);
                        this.navigate(Command::TtsSkip(-s), cx)
                    }))
                    .on_action(cx.listener(|this, _: &SkipForward, _, cx| {
                        let s = i32::from(this.hub.read(cx).snap.config.tts.skip_seconds);
                        this.navigate(Command::TtsSkip(s), cx)
                    }))
                    .on_action(cx.listener(|this, _: &PrevSentence, _, cx| {
                        this.navigate(Command::TtsSentence(-1), cx)
                    }))
                    .on_action(cx.listener(|this, _: &NextSentence, _, cx| {
                        this.navigate(Command::TtsSentence(1), cx)
                    }))
                    .on_action(cx.listener(|this, _: &Slower, _, cx| {
                        let rate = step_rate(this.rate(cx), -KEY_RATE_STEP);
                        this.set_rate(rate, cx)
                    }))
                    .on_action(cx.listener(|this, _: &Faster, _, cx| {
                        let rate = step_rate(this.rate(cx), KEY_RATE_STEP);
                        this.set_rate(rate, cx)
                    }))
                    .on_action(cx.listener(|this, _: &CloseReader, _, cx| {
                        if this.popover.take().is_some() {
                            cx.notify();
                        } else {
                            this.send(cx, Command::TtsStop);
                        }
                    }))
                    .w_full()
                    .h(card)
                    .flex_none()
                    .relative()
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .rounded(px(p.radius))
                    .bg(p.surface)
                    .border_1()
                    .border_color(p.line)
                    .child(grain(p, f32::from(s.width), f32::from(card)))
                    .when(p.planet_corner, |d| {
                        d.child(
                            img(assets::art(assets::PLANET_CORNER.clone()))
                                .absolute()
                                .top(px(0.))
                                .right(px(0.))
                                .size(px(PLANET))
                                .rounded_tr(px(p.radius - 1.)),
                        )
                    })
                    .child(header)
                    .child(body)
                    .children(popover),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dicta_core::AppConfig;

    #[test]
    fn rates_read_with_a_decimal_comma_and_stay_on_the_grid() {
        let l = Lang::Es;
        assert_eq!(fmt_rate(1., l), "1×");
        assert_eq!(fmt_rate(1.25, l), "1,25×");
        assert_eq!(fmt_rate(0.75, l), "0,75×");
        assert_eq!(fmt_rate(1.5, l), "1,5×");
        assert_eq!(fmt_rate(2.5, l), "2,5×");
        assert_eq!(fmt_rate(1.25, Lang::En), "1.25×");
        assert_eq!(step_rate(1., 0.25), 1.25);
        assert_eq!(step_rate(1.23, 0.), 1.25, "snaps to 0,05");
        assert_eq!(step_rate(2.4, KEY_RATE_STEP), RATE_MAX);
        assert_eq!(step_rate(0.6, -KEY_RATE_STEP), RATE_MIN);
        for preset in RATE_PRESETS {
            assert!((RATE_MIN..=RATE_MAX).contains(&preset));
        }
        assert_eq!(step_volume(0.95, 0.1), 1.);
        assert_eq!(step_volume(0.05, -0.1), 0.);
    }

    #[test]
    fn times_and_estimates_are_labelled() {
        assert_eq!(fmt_time(0), "0:00");
        assert_eq!(fmt_time(278_400), "4:38");
        assert_eq!(fmt_time(3_723_000), "1:02:03");
        assert_eq!(total_label(278_400, false), "≈ 4:38");
        assert_eq!(total_label(278_400, true), "4:38");
        assert_eq!(cost_label(0.19, Lang::Es), "≈ US$ 0,19 estimado");
        assert_eq!(cost_label(0.0001, Lang::Es), "menos de US$ 0,01 estimado");
        assert!(cost_label(0., Lang::Es).contains("estimado"));
        assert_eq!(cost_label(0.19, Lang::En), "≈ US$0.19 estimated");
        assert_eq!(cost_label(0.0001, Lang::En), "less than US$0.01 estimated");
    }

    #[test]
    fn paragraphs_keep_their_offsets_and_skip_blank_lines() {
        let text = "Uno dos.\n\nTres cuatro.\nCinco";
        let paras = split_paragraphs(text);
        assert_eq!(
            paras,
            vec![(0, "Uno dos."), (10, "Tres cuatro."), (23, "Cinco")]
        );
        for (start, s) in paras {
            assert_eq!(&text[start..start + s.len()], s);
        }
        assert!(split_paragraphs("").is_empty());
    }

    #[test]
    fn spans_map_into_their_paragraph_only() {
        let span = TextSpan { start: 12, end: 18 };
        assert_eq!(local_range(span, 10, 12), Some(2..8));
        assert_eq!(local_range(span, 0, 8), None);
        // A sentence crossing a paragraph end is cut at it.
        assert_eq!(
            local_range(TextSpan { start: 5, end: 30 }, 10, 12),
            Some(0..12)
        );
    }

    #[test]
    fn a_range_is_split_on_every_visual_line() {
        // Three visual lines of 10 bytes, 10 px per byte.
        let lines = [(0, 0.), (10, 100.), (20, 200.)];
        let x_for = |i: usize| i as f32 * 10.;
        assert_eq!(
            line_pieces(&lines, 30, 300., x_for, 2..5),
            vec![(0, 20., 50.)]
        );
        // From the middle of the first line to the middle of the third.
        assert_eq!(
            line_pieces(&lines, 30, 300., x_for, 5..25),
            vec![(0, 50., 100.), (1, 0., 100.), (2, 0., 50.)]
        );
        // A range ending at a wrap closes at the line end, not at the start
        // of the next line.
        assert_eq!(
            line_pieces(&lines, 30, 300., x_for, 12..20),
            vec![(1, 20., 100.)]
        );
        assert!(line_pieces(&lines, 30, 300., x_for, 40..50).is_empty());
    }

    fn snap_with(phase: TtsPhase) -> AppSnapshot {
        let mut s = AppSnapshot::new(AppConfig::default());
        s.tts.phase = phase;
        s.tts.show = true;
        s
    }

    #[test]
    fn every_card_fits_the_fixed_surface() {
        for compact in [false, true] {
            for orbit in [false, true] {
                for phase in [
                    TtsPhase::Idle,
                    TtsPhase::Reading,
                    TtsPhase::Confirm,
                    TtsPhase::Connecting,
                    TtsPhase::Buffering,
                    TtsPhase::Playing,
                    TtsPhase::Paused,
                    TtsPhase::Finished,
                    TtsPhase::Error,
                ] {
                    let mut s = snap_with(phase);
                    s.config.tts.compact = compact;
                    s.tts.error = Some("x".into());
                    s.tts.generated = Arc::new(vec![(0, 1000)]);
                    let h = reader_height(&s, orbit);
                    assert!(h > HEADER_H && h <= READER_MAX_HEIGHT, "{phase:?} {h}");
                }
            }
        }
        // Compact shows the controls without the text and is much shorter.
        let mut s = snap_with(TtsPhase::Playing);
        s.config.tts.compact = true;
        assert!(reader_height(&s, false) < READER_MAX_HEIGHT * 0.65);
        assert!(
            reader_height(&s, true) > reader_height(&s, false),
            "Órbita is taller"
        );
    }

    #[test]
    fn the_still_meter_lights_any_voice_and_never_overflows() {
        assert_eq!(meter_lit(0.), 0);
        assert_eq!(meter_lit(f32::NAN), 0);
        assert_eq!(meter_lit(0.01), 1);
        assert_eq!(meter_lit(0.5), METER_SEGMENTS / 2);
        assert_eq!(meter_lit(3.), METER_SEGMENTS);
    }

    #[test]
    fn error_cards_grow_with_the_message_up_to_a_limit() {
        let short = failed_height("No hay texto seleccionado.");
        let long = failed_height(&"palabra ".repeat(60));
        let huge = failed_height(&"palabra ".repeat(1000));
        assert!(short < long && long <= huge);
        assert!(huge <= READER_MAX_HEIGHT);
        assert_eq!(failed_height("a\nb"), failed_height("a") + 19. + 4.);
    }

    #[test]
    fn errors_keep_the_player_only_when_audio_exists() {
        let mut t = TtsSnapshot {
            phase: TtsPhase::Error,
            ..Default::default()
        };
        assert_eq!(reader_mode(&t), ReaderMode::Failed);
        t.generated = Arc::new(vec![(0, 0)]);
        assert_eq!(
            reader_mode(&t),
            ReaderMode::Failed,
            "empty range is no audio"
        );
        t.generated = Arc::new(vec![(0, 2_000)]);
        assert_eq!(reader_mode(&t), ReaderMode::Player);
        t.phase = TtsPhase::Confirm;
        assert_eq!(reader_mode(&t), ReaderMode::Confirm);
        t.phase = TtsPhase::Reading;
        assert_eq!(reader_mode(&t), ReaderMode::Waiting);
    }

    #[test]
    fn taking_the_text_says_what_happens_to_the_clipboard() {
        let l = Lang::Es;
        assert!(taking_text(TtsSource::CopySelection, true, l).contains("devuelve"));
        assert!(taking_text(TtsSource::CopySelection, false, l).contains("queda"));
        assert!(taking_text(TtsSource::Clipboard, true, l).contains("portapapeles"));
        assert!(!taking_text(TtsSource::Selection, true, l).contains("copia"));
        assert!(taking_text(TtsSource::CopySelection, true, Lang::En).contains("restores"));
    }

    #[test]
    fn unknown_voices_and_long_previews_read_well() {
        let voices = dicta_core::tts::builtin_voices();
        assert_eq!(voice_name(&voices, "eve", Lang::Es), "Eve");
        assert_eq!(voice_name(&voices, "nova-custom", Lang::Es), "Nova-custom");
        assert_eq!(voice_name(&voices, "", Lang::En), "Voice");
        let long = "palabra ".repeat(100);
        let e = excerpt(&long, 30);
        assert!(e.ends_with('…') && e.chars().count() <= 31, "{e}");
        assert!(!e.contains("  "));
        assert_eq!(excerpt("Hola\n\nmundo", 30), "Hola mundo");
        assert_eq!(wav_path("/tmp/x".into()), PathBuf::from("/tmp/x.wav"));
        assert_eq!(wav_path("/tmp/x.flac".into()), PathBuf::from("/tmp/x.flac"));
    }
}
