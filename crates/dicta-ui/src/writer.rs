//! Escritura window: a floating card, sibling of the reader, that fixes or
//! translates the text the controller captured and explains what changed.
//! It never calls Grok or touches the clipboard; it renders
//! [`WriterSnapshot`]s and turns every control into a `Writer*` [`Command`].
//!
//! Layout: header · mode and target language · input · result (diff or
//! translation, then the explanations, scrolling together) · status and
//! actions. The card has one height in every phase, so the result never
//! jumps while Grok works; a re-run keeps the previous result dimmed.
//! Like the reader, it is dragged natively from its header.

use std::ops::Range;

use dicta_core::{
    AppSnapshot, Command,
    writer::{DiffSpan, TARGET_LANGUAGES, WriterConfig, WriterMode, WriterPhase, WriterSnapshot},
};
use gpui::{
    AnyElement, App, AppContext as _, Bounds, Context, Div, Entity, FocusHandle, FontWeight,
    HighlightStyle, KeyBinding, MouseButton, ScrollHandle, SharedString, Stateful,
    StrikethroughStyle, StyledText, Task, Window, actions, div, img, point, prelude::*, px, size,
};

use crate::{
    Hub, OverlayPlacement, WRITER_MAX_HEIGHT,
    assets::{self, DISPLAY_FONT, Icon, UI_FONT},
    fixed_surface, format,
    home::token_pair,
    i18n::{Lang, Msg, tf},
    lang,
    overlay::{Overlay, draggable},
    perf,
    reader::{menu_row, shadow},
    text_field::{self, FieldEvent, TextField},
    theme::Palette,
    ui_state,
    widgets::{Kind, demo_badge, disabled, grain, icon, icon_button, icon_label_button, segment},
};

actions!(
    dicta_writer,
    [CloseWriter, RunWriter, CopyResult, ReplaceResult]
);

const CONTEXT: &str = "DictaWriter";

/// Keys while the card has focus. In the input, Ctrl+Enter and Esc reach
/// the card through the field's `CommitField` and `Blur`.
pub fn bind_keys(cx: &mut App) {
    let c = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("escape", CloseWriter, c),
        KeyBinding::new("secondary-enter", RunWriter, c),
        KeyBinding::new("secondary-shift-c", CopyResult, c),
        KeyBinding::new("secondary-shift-r", ReplaceResult, c),
    ]);
}

// ------------------------------------------------------------------ layout

const HEADER_H: f32 = 44.;
const TOOLBAR_H: f32 = 44.;
/// Regolito's planet in the corner, as in the reader.
const PLANET: f32 = 62.;
const PLANET_PAD: f32 = 60.;
const PAD_X: f32 = 12.;
/// Four lines of input before it scrolls.
const INPUT_H: f32 = 92.;
const MENU_W: f32 = 232.;
/// Typing settles into `WriterSetInput` after this pause.
const INPUT_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(400);

// --------------------------------------------------------------- wording

/// A language in its own name, for the target selector.
pub(crate) fn native_name(code: &str) -> SharedString {
    match code {
        "en" => "English".into(),
        "es" => "Español".into(),
        "pt" => "Português".into(),
        "fr" => "Français".into(),
        "it" => "Italiano".into(),
        "de" => "Deutsch".into(),
        "ja" => "日本語".into(),
        "zh" => "中文".into(),
        "ko" => "한국어".into(),
        "ru" => "Русский".into(),
        other => other.to_uppercase().into(),
    }
}

/// A language named in the interface language, capitalised ("Inglés",
/// "English"); unknown codes read as themselves.
pub(crate) fn language_name(code: &str, l: Lang) -> String {
    let m = match code {
        "en" => Msg::LangEnglish,
        "es" => Msg::LangSpanish,
        "pt" => Msg::LangPortuguese,
        "fr" => Msg::LangFrench,
        "it" => Msg::LangItalian,
        "de" => Msg::LangGerman,
        "ja" => Msg::LangJapanese,
        "zh" => Msg::LangChinese,
        "ko" => Msg::LangKorean,
        "ru" => Msg::LangRussian,
        other => return other.to_string(),
    };
    l.t(m).into()
}

/// The name inside a sentence: Spanish writes languages in lowercase.
fn in_sentence(code: &str, l: Lang) -> String {
    let name = language_name(code, l);
    match l {
        Lang::Es => name.to_lowercase(),
        Lang::En => name,
    }
}

pub(crate) fn mode_label(mode: WriterMode, l: Lang) -> &'static str {
    l.t(match mode {
        WriterMode::Auto => Msg::WriterModeAuto,
        WriterMode::Fix => Msg::WriterModeFix,
        WriterMode::Translate => Msg::WriterModeTranslate,
    })
}

/// What was done: "Arreglado · inglés", "Traducido del español al inglés".
/// `mode` is the output's, never `Auto`.
pub(crate) fn done_label(mode: WriterMode, detected: &str, target: &str, l: Lang) -> String {
    let detected = detected.trim();
    match mode {
        WriterMode::Translate if detected.is_empty() || detected == target => {
            tf!(l, WriterDoneTranslateTo, to = in_sentence(target, l))
        }
        WriterMode::Translate => tf!(
            l,
            WriterDoneTranslate,
            from = in_sentence(detected, l),
            to = in_sentence(target, l)
        ),
        _ => {
            let code = if detected.is_empty() {
                target
            } else {
                detected
            };
            tf!(l, WriterDoneFix, lang = in_sentence(code, l))
        }
    }
}

/// What the result area says while Grok works.
fn working_label(phase: WriterPhase, mode: WriterMode, l: Lang) -> &'static str {
    l.t(match (phase, mode) {
        (WriterPhase::Capturing, _) => Msg::WriterCapturing,
        (_, WriterMode::Fix) => Msg::WriterWorkingFix,
        (_, WriterMode::Translate) => Msg::WriterWorkingTranslate,
        (_, WriterMode::Auto) => Msg::WriterWorkingAuto,
    })
}

/// The diff as one text with its removed and added stretches styled:
/// removed struck through in the danger tone, added on the word marker.
/// For display only: Copiar and Reemplazar use the controller's text.
pub(crate) fn diff_text(
    diff: &[DiffSpan],
    p: Palette,
) -> (String, Vec<(Range<usize>, HighlightStyle)>) {
    let mut text = String::new();
    let mut runs = Vec::new();
    let mut after_removed = false;
    for span in diff {
        // "has" struck through right before "have" reads as one word;
        // a plain space between them keeps both legible.
        if let DiffSpan::Added(t) = span
            && after_removed
            && !text.ends_with(char::is_whitespace)
            && !t.starts_with(char::is_whitespace)
        {
            text.push(' ');
        }
        after_removed = matches!(span, DiffSpan::Removed(_));
        let (piece, style) = match span {
            DiffSpan::Same(t) => (t, None),
            DiffSpan::Removed(t) => (
                t,
                Some(HighlightStyle {
                    color: Some(p.danger.into()),
                    strikethrough: Some(StrikethroughStyle {
                        thickness: px(1.5),
                        color: Some(p.danger.into()),
                    }),
                    ..Default::default()
                }),
            ),
            DiffSpan::Added(t) => (
                t,
                Some(HighlightStyle {
                    color: Some(p.text.into()),
                    background_color: Some(p.mark_word.into()),
                    ..Default::default()
                }),
            ),
        };
        let start = text.len();
        text.push_str(piece);
        if let Some(style) = style
            && !piece.is_empty()
        {
            runs.push((start..text.len(), style));
        }
    }
    (text, runs)
}

/// The diff changes something.
pub(crate) fn has_changes(diff: &[DiffSpan]) -> bool {
    diff.iter().any(|s| match s {
        DiffSpan::Same(_) => false,
        DiffSpan::Removed(t) | DiffSpan::Added(t) => !t.trim().is_empty(),
    })
}

/// The explanations show when at least one change says why.
pub(crate) fn shows_explanations(w: &WriterSnapshot) -> bool {
    w.output
        .as_ref()
        .is_some_and(|o| o.changes.iter().any(|c| !c.why.trim().is_empty()))
}

/// What the window draws. A hub notification that leaves it equal (a
/// dictation, a reading, another setting) does not redraw the window.
#[derive(Clone, Debug, PartialEq)]
struct Seen {
    writer: WriterSnapshot,
    config: WriterConfig,
    demo: bool,
}

impl Seen {
    fn of(snap: &AppSnapshot) -> Self {
        Self {
            writer: snap.writer.clone(),
            config: snap.config.writer.clone(),
            demo: snap.demo,
        }
    }

    fn matches(&self, snap: &AppSnapshot) -> bool {
        self.demo == snap.demo && self.writer == snap.writer && self.config == snap.config.writer
    }
}

/// A short note on hover, e.g. why Reemplazar is off.
struct Tip(SharedString);

impl Render for Tip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = ui_state(cx).palette;
        div()
            .max_w(px(260.))
            .px(px(9.))
            .py(px(6.))
            .rounded(px(8.))
            .bg(p.raised)
            .border_1()
            .border_color(p.control)
            .shadow(vec![shadow(p)])
            .font_family(UI_FONT)
            .text_size(px(12.))
            .line_height(px(17.))
            .text_color(p.text)
            .child(self.0.clone())
    }
}

pub struct Writer {
    hub: Entity<Hub>,
    focus: FocusHandle,
    field: Entity<TextField>,
    scroll: ScrollHandle,
    /// The target language menu is open.
    menu: bool,
    seen: Option<Seen>,
    /// The request the view state belongs to.
    request: u64,
    /// The input the controller has, as far as the window knows: the last
    /// one sent or received. Typing past it is sent after a pause.
    sent_input: String,
    input_debounce: Option<Task<()>>,
    input_region: Option<Bounds<gpui::Pixels>>,
    _subs: Vec<gpui::Subscription>,
}

impl Writer {
    /// The card fills its window in every phase, so unlike the reader the
    /// placement never moves it inside a taller Wayland surface.
    pub fn new(
        hub: Entity<Hub>,
        _placement: OverlayPlacement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (input, request) = {
            let w = &hub.read(cx).snap.writer;
            (w.input.clone(), w.request_id)
        };
        let field = cx.new(|cx| {
            TextField::new(input.clone(), Msg::PhWriterInput, window, cx)
                .multiline()
                .fill_height()
        });
        let subs = vec![
            cx.subscribe(&field, |this, _, event, cx| match event {
                FieldEvent::Changed => {
                    this.input_debounce = Some(cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(INPUT_DEBOUNCE).await;
                        this.update(cx, |this, cx| this.flush_input(cx)).ok();
                    }));
                }
                FieldEvent::Commit => this.flush_input(cx),
            }),
            cx.observe(&hub, |this, hub, cx| {
                let snap = &hub.read(cx).snap;
                if this.seen.as_ref().is_some_and(|s| s.matches(snap)) {
                    return;
                }
                let seen = Seen::of(snap);
                let was_capturing = this
                    .seen
                    .as_ref()
                    .is_some_and(|s| s.writer.phase == WriterPhase::Capturing);
                this.sync_input(&seen.writer, was_capturing, cx);
                this.seen = Some(seen);
                cx.notify();
            }),
        ];
        let focus = cx.focus_handle();
        // Esc, Ctrl+Enter and the copy keys work as soon as it opens.
        focus.focus(window, cx);
        Self {
            seen: Some(Seen::of(&hub.read(cx).snap)),
            hub,
            focus,
            field,
            scroll: ScrollHandle::new(),
            menu: false,
            request,
            sent_input: input,
            input_debounce: None,
            input_region: None,
            _subs: subs,
        }
    }

    /// Gallery: open with the language menu shown.
    #[cfg_attr(not(feature = "screenshots"), allow(dead_code))]
    pub(crate) fn with_menu(mut self) -> Self {
        self.menu = true;
        self
    }

    fn send(&self, cx: &App, command: Command) {
        self.hub.read(cx).send(command);
    }

    /// A new capture replaces the input; the user's own typing is never
    /// overwritten by a snapshot of the request it started.
    fn sync_input(&mut self, w: &WriterSnapshot, was_capturing: bool, cx: &mut Context<Self>) {
        let fresh = w.request_id != self.request || was_capturing;
        self.request = w.request_id;
        if !fresh || w.input == self.sent_input {
            return;
        }
        self.sent_input = w.input.clone();
        self.input_debounce = None;
        self.menu = false;
        self.scroll.set_offset(point(px(0.), px(0.)));
        let text = w.input.clone();
        self.field.update(cx, |f, cx| {
            f.clear(cx);
            f.sync(&text, cx);
        });
    }

    /// Sends the typed input if the controller does not have it yet.
    fn flush_input(&mut self, cx: &mut Context<Self>) {
        self.input_debounce = None;
        let text = self.field.read(cx).text().to_string();
        if text != self.sent_input {
            self.send(cx, Command::WriterSetInput(text.clone()));
            self.sent_input = text;
        }
    }

    fn run(&mut self, mode: WriterMode, cx: &mut Context<Self>) {
        self.flush_input(cx);
        self.menu = false;
        self.scroll.set_offset(point(px(0.), px(0.)));
        self.send(cx, Command::WriterRun(mode));
        cx.notify();
    }

    fn run_current(&mut self, cx: &mut Context<Self>) {
        let mode = self.hub.read(cx).snap.writer.mode;
        self.run(mode, cx);
    }

    fn set_target(&mut self, code: &'static str, cx: &mut Context<Self>) {
        self.flush_input(cx);
        self.menu = false;
        self.send(cx, Command::WriterSetTarget(code.into()));
        cx.notify();
    }

    fn has_output(&self, cx: &App) -> bool {
        self.hub.read(cx).snap.writer.output.is_some()
    }

    fn copy(&mut self, cx: &mut Context<Self>) {
        if self.has_output(cx) {
            self.send(cx, Command::WriterCopy);
        }
    }

    fn replace(&mut self, cx: &mut Context<Self>) {
        let w = &self.hub.read(cx).snap.writer;
        if w.output.is_some() && w.can_replace {
            self.send(cx, Command::WriterReplace);
        }
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        if self.menu {
            self.menu = false;
            cx.notify();
        } else {
            self.send(cx, Command::WriterClose);
        }
    }

    fn header(&self, demo: bool, p: Palette, cx: &mut Context<Self>) -> Div {
        let l = lang(cx);
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
                .child(
                    div()
                        .min_w(px(0.))
                        .truncate()
                        .font_family(DISPLAY_FONT)
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_size(px(13.))
                        .child(l.t(Msg::SecWriter)),
                )
                .when(demo, |d| d.child(demo_badge(p))),
        );
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(4.))
            .h(px(HEADER_H))
            .pl(px(8.))
            .pr(px(if p.planet_corner { PLANET_PAD } else { 8. }))
            .border_b_1()
            .border_color(p.line)
            .child(drag)
            .child(
                icon_button("writer-close", Icon::Close, p)
                    .on_click(cx.listener(|this, _, _, cx| this.send(cx, Command::WriterClose))),
            )
    }

    /// Auto / Arreglar / Traducir, each a new run, and the target language.
    fn toolbar(&self, w: &WriterSnapshot, p: Palette, cx: &mut Context<Self>) -> Div {
        let l = lang(cx);
        let target = if w.target_language.is_empty() {
            self.hub.read(cx).snap.config.writer.target_language.clone()
        } else {
            w.target_language.clone()
        };
        let modes = [WriterMode::Auto, WriterMode::Fix, WriterMode::Translate];
        let open = self.menu;
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.))
            .h(px(TOOLBAR_H))
            .px(px(PAD_X))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .p(px(3.))
                    .gap(px(2.))
                    .rounded(px(10.))
                    .bg(p.selected)
                    .border_1()
                    .border_color(p.line)
                    .children(modes.map(|mode| {
                        segment(
                            ("writer-mode", mode as usize),
                            mode_label(mode, l),
                            w.mode == mode,
                            p,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| this.run(mode, cx)))
                    })),
            )
            .child(div().flex_1().min_w(px(4.)))
            .child(
                div()
                    .id("writer-target")
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(5.))
                    .h(px(30.))
                    .max_w(px(170.))
                    .pl(px(9.))
                    .pr(px(5.))
                    .rounded(px(8.))
                    .bg(p.raised)
                    .border_1()
                    .border_color(if open { p.control } else { p.line })
                    .cursor_pointer()
                    .hover(move |s| s.border_color(p.control))
                    .text_size(px(12.5))
                    .font_weight(FontWeight::MEDIUM)
                    .child(div().text_color(p.text_2).child("→"))
                    .child(div().min_w(px(0.)).truncate().child(native_name(&target)))
                    .child(icon(Icon::ChevronDown, 14., p.text_2))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.menu = !this.menu;
                        cx.notify();
                    })),
            )
    }

    fn input(&self, w: &WriterSnapshot, p: Palette, cx: &App) -> Div {
        let l = lang(cx);
        div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(4.))
            .child(
                div()
                    .h(px(INPUT_H))
                    .flex()
                    .flex_col()
                    .text_size(px(14.))
                    .line_height(px(20.))
                    .child(self.field.clone()),
            )
            .when(w.truncated, |d| {
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .text_size(px(12.))
                        .text_color(p.text_2)
                        .child(icon(Icon::Alert, 13., p.text_2))
                        .child(div().min_w(px(0.)).truncate().child(tf!(
                            l,
                            WriterTruncated,
                            n = format::count(dicta_core::writer::MAX_INPUT_CHARS as u64, l)
                        ))),
                )
            })
    }

    /// The line above the result: what was done, or what is happening.
    fn result_label(
        &self,
        w: &WriterSnapshot,
        working: bool,
        target: &str,
        p: Palette,
        l: Lang,
    ) -> Option<Div> {
        let row = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.))
            .h(px(18.))
            .text_size(px(12.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(p.text_2);
        if working {
            return Some(
                row.child(div().flex_none().size(px(7.)).rounded_full().bg(p.orange))
                    .child(working_label(w.phase, w.mode, l)),
            );
        }
        let o = w.output.as_ref()?;
        Some(row.child(icon(Icon::Check, 13., p.text_2)).child(
            div().min_w(px(0.)).truncate().child(done_label(
                o.mode,
                &o.detected_language,
                target,
                l,
            )),
        ))
    }

    /// The fixed text with its diff, or the translation.
    fn result_text(&self, w: &WriterSnapshot, p: Palette, l: Lang) -> Option<Div> {
        let o = w.output.as_ref()?;
        let body = div()
            .text_size(px(15.))
            .line_height(px(23.))
            .text_color(p.text);
        let fix = o.mode != WriterMode::Translate && !w.diff.is_empty();
        Some(if fix {
            let (text, runs) = diff_text(&w.diff, p);
            let unchanged = !has_changes(&w.diff);
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(body.child(StyledText::new(text).with_highlights(runs)))
                .when(unchanged, |d| {
                    d.child(
                        div()
                            .text_size(px(12.5))
                            .text_color(p.text_2)
                            .child(l.t(Msg::WriterNoChanges)),
                    )
                })
        } else {
            body.child(SharedString::from(o.text.clone()))
        })
    }

    fn explanations(&self, w: &WriterSnapshot, p: Palette, l: Lang) -> Option<Div> {
        if !shows_explanations(w) {
            return None;
        }
        let o = w.output.as_ref()?;
        Some(
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .pt(px(10.))
                .border_t_1()
                .border_color(p.line)
                .child(
                    div()
                        .text_size(px(11.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(p.text_2)
                        .child(l.t(Msg::WriterWhy).to_uppercase()),
                )
                .children(
                    o.changes
                        .iter()
                        .filter(|c| !c.why.trim().is_empty())
                        .map(|c| {
                            let pair = div()
                                .flex()
                                .flex_wrap()
                                .items_baseline()
                                .gap_x(px(6.))
                                .text_size(px(13.5))
                                .line_height(px(19.))
                                .when(!c.original.trim().is_empty(), |d| {
                                    d.child(
                                        div()
                                            .text_color(p.danger)
                                            .line_through()
                                            .child(SharedString::from(c.original.clone())),
                                    )
                                    .child(div().text_color(p.text_2).child("→"))
                                })
                                .child(
                                    div()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(SharedString::from(c.corrected.clone())),
                                );
                            div().flex().flex_col().gap(px(2.)).child(pair).child(
                                div()
                                    .text_size(px(13.))
                                    .line_height(px(19.))
                                    .text_color(p.text_2)
                                    .child(SharedString::from(c.why.trim().to_string())),
                            )
                        }),
                ),
        )
    }

    /// Lines standing in for a first result while Grok works.
    fn placeholder(p: Palette) -> Div {
        div()
            .flex()
            .flex_col()
            .gap(px(11.))
            .pt(px(6.))
            .children([0.92, 0.78, 0.55].map(|f| {
                div()
                    .h(px(11.))
                    .w(gpui::relative(f))
                    .rounded(px(5.))
                    .bg(p.selected)
            }))
    }

    fn failed(&self, w: &WriterSnapshot, p: Palette, cx: &mut Context<Self>) -> Div {
        let l = lang(cx);
        let message = w
            .error
            .clone()
            .filter(|e| !e.trim().is_empty())
            .unwrap_or_else(|| w.status.clone());
        let mode = w.mode;
        div()
            .flex()
            .gap(px(10.))
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
                            .child(l.t(Msg::WriterFailed)),
                    )
                    .children(
                        message
                            .lines()
                            .map(str::trim)
                            .filter(|line| !line.is_empty())
                            .map(|line| {
                                div()
                                    .text_size(px(13.))
                                    .line_height(px(19.))
                                    .child(SharedString::from(line.to_string()))
                            })
                            .collect::<Vec<_>>(),
                    )
                    .child(
                        div().flex().pt(px(6.)).child(
                            icon_label_button(
                                "writer-retry",
                                Icon::Retry,
                                l.t(Msg::Retry),
                                Kind::Secondary,
                                p,
                            )
                            .on_click(cx.listener(move |this, _, _, cx| this.run(mode, cx))),
                        ),
                    ),
            )
    }

    fn result(
        &self,
        w: &WriterSnapshot,
        (target, shortcut): (&str, &str),
        p: Palette,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let l = lang(cx);
        let working = matches!(w.phase, WriterPhase::Capturing | WriterPhase::Working);
        let content: Div = if w.phase == WriterPhase::Error {
            self.failed(w, p, cx)
        } else if w.output.is_some() {
            div()
                .flex()
                .flex_col()
                .gap(px(10.))
                .children(self.result_label(w, working, target, p, l))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(12.))
                        // The previous result stays, dimmed, while a re-run works.
                        .when(working, |d| d.opacity(0.45))
                        .children(self.result_text(w, p, l))
                        .children(self.explanations(w, p, l)),
                )
        } else if working {
            div()
                .flex()
                .flex_col()
                .gap(px(10.))
                .children(self.result_label(w, true, target, p, l))
                .child(Self::placeholder(p))
        } else {
            let session = ui_state(cx).session;
            let key = if shortcut.trim().is_empty() {
                session.with_mod("Enter")
            } else {
                shortcut.to_string()
            };
            div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .text_size(px(13.))
                .line_height(px(19.))
                .text_color(p.text_2)
                .child(tf!(l, WriterEmpty, key = key))
                .child(tf!(l, WriterRunHint, key = session.with_mod("Enter")))
        };
        div()
            .id("writer-result")
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .p(px(12.))
            .rounded(px(10.))
            .bg(p.raised)
            .border_1()
            .border_color(if w.phase == WriterPhase::Error {
                p.danger
            } else {
                p.line
            })
            .child(content)
    }

    fn footer(&self, w: &WriterSnapshot, p: Palette, cx: &mut Context<Self>) -> Div {
        let l = lang(cx);
        let ready = w.output.is_some();
        let tokens = (w.input_tokens.is_some() || w.output_tokens.is_some()).then(|| {
            tf!(
                l,
                WriterTokens,
                pair = token_pair(w.input_tokens, w.output_tokens, l)
            )
        });
        let status = w.status.trim().to_string();
        let copy = icon_label_button(
            "writer-copy",
            Icon::Copy,
            l.t(Msg::Copy),
            Kind::Secondary,
            p,
        );
        let copy = if ready {
            copy.on_click(cx.listener(|this, _, _, cx| this.copy(cx)))
        } else {
            disabled(copy)
        };
        let replace = icon_label_button(
            "writer-replace",
            Icon::Paste,
            l.t(Msg::WriterReplace),
            Kind::Primary,
            p,
        );
        let replace = if ready && w.can_replace {
            replace.on_click(cx.listener(|this, _, _, cx| this.replace(cx)))
        } else if ready {
            let tip: SharedString = l.t(Msg::WriterReplaceUnavailable).into();
            disabled(replace).tooltip(move |_, cx| cx.new(|_| Tip(tip.clone())).into())
        } else {
            disabled(replace)
        };
        let speak = icon_label_button("writer-speak", Icon::Read, l.t(Msg::Play), Kind::Ghost, p);
        let speak = if ready {
            speak.on_click(cx.listener(|this, _, _, cx| this.send(cx, Command::WriterSpeak)))
        } else {
            disabled(speak)
        };
        div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(6.))
            .when(!status.is_empty() || tokens.is_some(), |d| {
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .h(px(16.))
                        .text_size(px(12.))
                        .text_color(p.text_2)
                        .child(div().flex_1().min_w(px(0.)).truncate().child(status))
                        .children(tokens.map(|t| div().flex_none().child(t))),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(replace)
                    .child(copy)
                    .child(div().flex_1())
                    .child(speak),
            )
    }

    fn menu(&self, target: &str, p: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.menu {
            return None;
        }
        let l = lang(cx);
        let rows: Vec<AnyElement> = TARGET_LANGUAGES
            .iter()
            .enumerate()
            .map(|(i, &code)| {
                // The interface name helps with scripts the user cannot read.
                let local = language_name(code, l);
                let detail = (native_name(code) != local).then_some(local);
                menu_row(
                    ("writer-lang", i),
                    native_name(code),
                    detail,
                    code == target,
                    p,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.set_target(code, cx)))
                .into_any_element()
            })
            .collect();
        let scrim = div()
            .id("writer-scrim")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .occlude()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.menu = false;
                    cx.notify();
                }),
            );
        let panel = div()
            .id("writer-menu")
            .absolute()
            .top(px(HEADER_H + TOOLBAR_H - 4.))
            .right(px(PAD_X))
            .w(px(MENU_W))
            .max_h(px(WRITER_MAX_HEIGHT - HEADER_H - TOOLBAR_H - 16.))
            .overflow_y_scroll()
            .occlude()
            .flex()
            .flex_col()
            .gap(px(1.))
            .p(px(6.))
            .rounded(px(12.))
            .bg(p.raised)
            .border_1()
            .border_color(p.control)
            .shadow(vec![shadow(p)])
            .child(
                div()
                    .px(px(8.))
                    .pt(px(4.))
                    .pb(px(2.))
                    .text_size(px(11.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(p.text_2)
                    .child(l.t(Msg::WriterTargetLanguages).to_uppercase()),
            )
            .children(rows);
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
}

impl Render for Writer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        perf::bump(&perf::WRITER_RENDERS);
        let p = ui_state(cx).palette;
        let (w, demo, target, shortcut) = {
            let snap = &self.hub.read(cx).snap;
            let w = snap.writer.clone();
            let target = if w.target_language.is_empty() {
                snap.config.writer.target_language.clone()
            } else {
                w.target_language.clone()
            };
            (w, snap.demo, target, snap.config.writer.shortcut.clone())
        };
        let s = window.viewport_size();
        let card = px(WRITER_MAX_HEIGHT).min(s.height);
        let region = Bounds::new(point(px(0.), px(0.)), size(s.width, card));
        // Only Wayland has input regions; elsewhere the window is the card.
        if fixed_surface() && self.input_region != Some(region) {
            window.set_input_region(Some(&[region]));
            self.input_region = Some(region);
        }
        let header = self.header(demo, p, cx);
        let toolbar = self.toolbar(&w, p, cx);
        let input = self.input(&w, p, cx);
        let result = self.result(&w, (&target, &shortcut), p, cx);
        let footer = self.footer(&w, p, cx);
        let menu = self.menu(&target, p, cx);
        div()
            .size_full()
            .flex()
            .flex_col()
            .font_family(UI_FONT)
            .text_size(px(14.))
            .text_color(p.text)
            .child(
                div()
                    .id("writer-card")
                    .track_focus(&self.focus)
                    .key_context(CONTEXT)
                    .capture_any_mouse_down(cx.listener(|this, _, window, cx| {
                        // The input keeps its own focus when clicked.
                        if !this.field.read(cx).is_focused(window) {
                            this.focus.focus(window, cx);
                        }
                    }))
                    .on_action(cx.listener(|this, _: &CloseWriter, _, cx| this.close(cx)))
                    .on_action(cx.listener(|this, _: &RunWriter, _, cx| this.run_current(cx)))
                    .on_action(cx.listener(|this, _: &CopyResult, _, cx| this.copy(cx)))
                    .on_action(cx.listener(|this, _: &ReplaceResult, _, cx| this.replace(cx)))
                    // Bubbled from the input (see `text_field`).
                    .on_action(
                        cx.listener(|this, _: &text_field::CommitField, _, cx| {
                            this.run_current(cx)
                        }),
                    )
                    .on_action(cx.listener(|this, _: &text_field::Blur, _, cx| this.close(cx)))
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
                    .child(toolbar.when(p.planet_corner, |d| d.pr(px(PLANET_PAD))))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_h(px(0.))
                            .gap(px(10.))
                            .px(px(PAD_X))
                            .pb(px(12.))
                            .child(input)
                            .child(result)
                            .child(footer),
                    )
                    .children(menu),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dicta_core::{
        AppConfig, ColorMode, Theme,
        writer::{WriterChange, WriterOutput},
    };

    #[test]
    fn labels_say_what_was_done_in_both_languages() {
        let es = Lang::Es;
        assert_eq!(
            done_label(WriterMode::Fix, "en", "en", es),
            "Arreglado · inglés"
        );
        assert_eq!(
            done_label(WriterMode::Translate, "es", "en", es),
            "Traducido del español al inglés"
        );
        assert_eq!(
            done_label(WriterMode::Translate, "", "en", es),
            "Traducido al inglés"
        );
        assert_eq!(
            done_label(WriterMode::Translate, "es", "en", Lang::En),
            "Translated from Spanish to English"
        );
        assert_eq!(
            done_label(WriterMode::Fix, "", "pt", Lang::En),
            "Fixed · Portuguese"
        );
        // Unknown codes read as themselves, never empty.
        assert_eq!(
            done_label(WriterMode::Fix, "nl", "en", es),
            "Arreglado · nl"
        );
        for code in TARGET_LANGUAGES {
            for l in Lang::ALL {
                assert_ne!(language_name(code, l), code, "{code}");
                assert_ne!(native_name(code).to_string(), code.to_uppercase(), "{code}");
            }
        }
        assert_eq!(native_name("es").to_string(), "Español");
        assert_eq!(native_name("de").to_string(), "Deutsch");
    }

    #[test]
    fn the_diff_reproduces_the_result_and_styles_only_changes() {
        let p = Palette::new(Theme::Signal, ColorMode::Light);
        let diff = vec![
            DiffSpan::Same("I ".into()),
            DiffSpan::Removed("has".into()),
            DiffSpan::Added("have".into()),
            DiffSpan::Same(" a cat.".into()),
        ];
        let (text, runs) = diff_text(&diff, p);
        assert_eq!(
            text, "I has have a cat.",
            "a space between removed and added"
        );
        assert_eq!(runs.len(), 2);
        assert_eq!(&text[runs[0].0.clone()], "has");
        assert!(runs[0].1.strikethrough.is_some());
        assert_eq!(&text[runs[1].0.clone()], "have");
        assert!(runs[1].1.background_color.is_some());
        assert!(has_changes(&diff));
        assert!(!has_changes(&[DiffSpan::Same("ok".into())]));
    }

    fn ready(changes: Vec<WriterChange>) -> WriterSnapshot {
        WriterSnapshot {
            phase: WriterPhase::Ready,
            output: Some(WriterOutput {
                mode: WriterMode::Fix,
                detected_language: "en".into(),
                text: "I have a cat.".into(),
                changes,
            }),
            ..Default::default()
        }
    }

    #[test]
    fn explanations_hide_when_nothing_says_why() {
        assert!(!shows_explanations(&WriterSnapshot::default()));
        assert!(!shows_explanations(&ready(vec![])));
        let silent = WriterChange {
            original: "has".into(),
            corrected: "have".into(),
            why: " ".into(),
        };
        assert!(!shows_explanations(&ready(vec![silent.clone()])));
        let why = WriterChange {
            why: "With I, the verb is have.".into(),
            ..silent.clone()
        };
        assert!(shows_explanations(&ready(vec![silent, why])));
    }

    #[test]
    fn only_writer_state_and_its_settings_redraw_the_window() {
        let mut snap = AppSnapshot::new(AppConfig::default());
        snap.writer = ready(vec![]);
        let seen = Seen::of(&snap);
        let mut other = snap.clone();
        other.phase = dicta_core::Phase::Recording;
        other.config.tts.compact = true;
        other.tts.position_ms = 4_000;
        assert!(seen.matches(&other), "dictation and reading leave it alone");
        let mut changed = snap.clone();
        changed.writer.status = "Copiado".into();
        assert!(!seen.matches(&changed));
        let mut config = snap.clone();
        config.config.writer.target_language = "pt".into();
        assert!(!seen.matches(&config));
    }
}
