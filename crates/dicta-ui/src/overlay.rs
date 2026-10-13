//! Dictation overlay: a small card that follows the controller phase.
//!
//! Live phases only display snapshot data (partials, level). Ready makes the
//! final text editable and offers learned-correction proposals; edits are sent
//! as `Command::EditText` when the field settles or before copy/paste.
//!
//! The card fills the window edge to edge (no shadow margin) and is dragged
//! natively from its header or, in Idle, from the pill itself. Every text that
//! can grow (transcript, status, proposals, errors) either scrolls inside a
//! flexible area or truncates, so controls keep their place.

use dicta_core::{Command, Phase, ShortcutMode};
use gpui::{
    AnyElement, App, Bounds, Context, Div, Entity, FontWeight, HighlightStyle, MouseButton,
    ScrollHandle, StyledText, Window, WindowControlArea, canvas, div, fill, img, point, prelude::*,
    px, size,
};

use crate::{
    Hub, OverlayPlacement,
    assets::{self, DISPLAY_FONT, Icon, MONO_FONT, UI_FONT},
    corrections::{self, Proposal},
    errors::ErrorOrigin,
    fixed_surface,
    i18n::{Lang, Msg, tf},
    lang, overlay_height, perf,
    settings::Section,
    text_field::{FieldEvent, TextField},
    theme::{Palette, VoiceStyle},
    ui_state,
    wave::voice,
    widgets::{
        Kind, button, demo_badge, format_elapsed, grain, icon_button, icon_label_button, kbd,
    },
};

/// Regolito's planet in the card corner, idle pill and taller cards. The
/// planet is centred on the corner with a radius of 104/120 of its box, so
/// at 62 px it clears the 40 px header plus the body padding: it stays
/// decoration behind the content and never under text or controls.
const PLANET_IDLE: f32 = 52.;
const PLANET_CARD: f32 = 62.;
/// Header right padding that keeps the timer and × clear of the planet.
const PLANET_HEADER_PAD: f32 = 60.;

/// Makes `area` move the window when pressed. Interactive children must sit
/// outside it: the press would start a move instead of a click.
///
/// Wayland, X11 and macOS move the window from the press
/// (`start_window_move`). On Windows that call does nothing; there the area
/// is a caption (`WindowControlArea::Drag`, answered as `HTCAPTION`), which
/// the system drags itself. The control area is ignored on the others.
pub(crate) fn draggable(area: Div) -> Div {
    area.cursor_grab()
        .window_control_area(WindowControlArea::Drag)
        .on_mouse_down(MouseButton::Left, |_, window, cx| {
            cx.stop_propagation();
            window.start_window_move();
        })
}

/// Name of a dictation phase; `Phase::label` is Spanish.
pub(crate) fn phase_label(phase: Phase, l: Lang) -> &'static str {
    l.t(match phase {
        Phase::Idle => Msg::PhaseIdle,
        Phase::Connecting => Msg::PhaseConnecting,
        Phase::Recording => Msg::PhaseRecording,
        Phase::Finalizing => Msg::PhaseFinalizing,
        Phase::Polishing => Msg::PhasePolishing,
        Phase::Ready => Msg::PhaseReady,
        Phase::Error => Msg::PhaseError,
    })
}

/// True while the live transcript shows its end, so new words keep it there.
/// Scrolling up to reread stops following until the user returns down.
fn follows_end(handle: &ScrollHandle) -> bool {
    handle.offset().y <= -handle.max_offset().y + px(4.)
}

pub struct Overlay {
    hub: Entity<Hub>,
    placement: OverlayPlacement,
    field: Entity<TextField>,
    live_scroll: ScrollHandle,
    /// Proposals the user dismissed for the current dictation.
    dismissed: Vec<Proposal>,
    input_region: Option<Bounds<gpui::Pixels>>,
    _subs: Vec<gpui::Subscription>,
}

impl Overlay {
    pub fn new(
        hub: Entity<Hub>,
        placement: OverlayPlacement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let initial = hub.read(cx).snap.final_text.clone();
        let field = cx.new(|cx| {
            TextField::new(initial, Msg::FinalTextPlaceholder, window, cx)
                .multiline()
                .fill_height()
        });
        let subs = vec![
            cx.subscribe(&field, |this, field, event, cx| {
                if *event == FieldEvent::Commit {
                    let text = field.read(cx).text().to_string();
                    this.hub.read(cx).send(Command::EditText(text));
                }
            }),
            cx.observe(&hub, |this, hub, cx| {
                let snap = &hub.read(cx).snap;
                if snap.phase != Phase::Ready {
                    this.dismissed.clear();
                }
                let text = snap.final_text.clone();
                this.field.update(cx, |f, cx| f.sync(&text, cx));
                cx.notify();
            }),
        ];
        Self {
            hub,
            placement,
            field,
            live_scroll: ScrollHandle::new(),
            dismissed: Vec::new(),
            input_region: None,
            _subs: subs,
        }
    }

    fn send(&self, cx: &App, command: Command) {
        self.hub.read(cx).send(command);
    }

    /// Final-text editor, for the perf harness.
    #[cfg_attr(not(feature = "screenshots"), allow(dead_code))]
    pub(crate) fn field(&self) -> &Entity<TextField> {
        &self.field
    }

    /// Pushes unsaved edits before an action that uses the text.
    fn flush_edit(&self, cx: &App) {
        let hub = self.hub.read(cx);
        let text = self.field.read(cx).text();
        if hub.snap.phase == Phase::Ready && text != hub.snap.final_text {
            hub.send(Command::EditText(text.to_string()));
        }
    }

    /// Grip that hints the card can be dragged; the whole area around it
    /// moves the window.
    pub(crate) fn grip(p: Palette) -> Div {
        div()
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(2.))
            .p(px(4.))
            .children((0..3).map(move |_| {
                div()
                    .flex()
                    .gap(px(2.))
                    .children((0..2).map(move |_| div().size(px(2.5)).rounded_full().bg(p.text_2)))
            }))
    }

    fn header(&self, p: Palette, cx: &mut Context<Self>) -> AnyElement {
        let l = lang(cx);
        let snap = &self.hub.read(cx).snap;
        let phase = snap.phase;
        let title = match phase {
            Phase::Polishing => tf!(l, PolishingWith, model = snap.config.polish_model),
            Phase::Error => ErrorOrigin::of(snap)
                .unwrap_or(ErrorOrigin::Dictation)
                .title(l)
                .to_string(),
            other => phase_label(other, l).to_string(),
        };
        let live_timer = matches!(
            phase,
            Phase::Recording | Phase::Finalizing | Phase::Polishing
        );
        // Near full scale RMS, measured before any visual scaling. It is not
        // a peak measurement, so the badge says "very high", not clipping.
        let hub = self.hub.read(cx);
        let very_loud = phase == Phase::Recording && hub.levels.very_loud(hub.levels.now());
        // Everything but the hide button drags: grip, title and the empty
        // stretch. The title truncates so a long model name cannot push the
        // timer or the button out.
        let drag = draggable(
            div()
                .flex_1()
                .min_w(px(0.))
                .h_full()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(Self::grip(p))
                .when(phase == Phase::Recording, |d| {
                    d.child(div().flex_none().size(px(8.)).rounded_full().bg(p.orange))
                })
                .when(phase == Phase::Error, |d| {
                    d.child(div().flex_none().size(px(8.)).rounded_full().bg(p.danger))
                })
                .child(
                    div()
                        .min_w(px(0.))
                        .truncate()
                        .font_family(DISPLAY_FONT)
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_size(px(13.))
                        .child(title),
                )
                .when(snap.demo, |d| d.child(demo_badge(p))),
        );
        div()
            .id("header")
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.))
            .h(px(40.))
            .pl(px(8.))
            .pr(px(if p.planet_corner {
                PLANET_HEADER_PAD
            } else {
                8.
            }))
            .border_b_1()
            .border_color(p.line)
            .child(drag)
            .when(very_loud, |d| {
                d.child(
                    div()
                        .flex_none()
                        .px(px(6.))
                        .rounded(px(5.))
                        .border_1()
                        .border_color(p.danger)
                        .text_size(px(11.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(p.danger)
                        .child(l.t(Msg::VeryLoud)),
                )
            })
            .when(live_timer, |d| {
                d.child(
                    div()
                        .flex_none()
                        .font_family(MONO_FONT)
                        .text_size(px(13.))
                        .text_color(p.text_2)
                        .child(format_elapsed(snap.elapsed_ms)),
                )
            })
            .when(!phase.busy(), |d| d.child(self.hide_button(p, cx)))
            .into_any_element()
    }

    fn hide_button(&self, p: Palette, cx: &mut Context<Self>) -> impl IntoElement {
        icon_button("hide", Icon::Close, p).on_click(cx.listener(|this, _, _, cx| {
            this.flush_edit(cx);
            this.send(cx, Command::HideOverlay);
        }))
    }

    /// The microphone of Mars voice: Mars with its speckled shadow and the
    /// dark orbiter of the Mars mark, the same in every style.
    fn mic(&self, p: Palette) -> AnyElement {
        div()
            .flex_none()
            .relative()
            .size(px(36.))
            .child(
                img(assets::art(assets::mars(30.)))
                    .absolute()
                    .left(px(1.))
                    .top(px(5.))
                    .size(px(30.)),
            )
            .child(
                div()
                    .absolute()
                    .right(px(0.))
                    .top(px(1.))
                    .size(px(6.))
                    .rounded_full()
                    .bg(p.text),
            )
            .into_any_element()
    }

    /// How the shortcut works in the current mode, for idle and recording.
    fn shortcut_hint(&self, recording: bool, p: Palette, cx: &App) -> Div {
        let l = lang(cx);
        let config = &self.hub.read(cx).snap.config;
        let key = kbd(config.toggle_shortcut.clone(), p);
        let text = |t: &'static str| div().flex_none().child(t);
        let row = div()
            .flex()
            .items_center()
            .gap(px(5.))
            .min_w(px(0.))
            .overflow_hidden()
            .text_size(px(12.))
            .text_color(p.text_2);
        // Short on purpose: the pill and the recording footer only have room
        // for the key. Ajustes › Atajos explains the gesture and the copy.
        match (config.shortcut_mode, recording) {
            (ShortcutMode::Toggle, false) => row.child(key).child(text(l.t(Msg::HintToDictate))),
            (ShortcutMode::Toggle, true) => row.child(key).child(text(l.t(Msg::HintFinishes))),
            (ShortcutMode::Hold, false) => row.child(text(l.t(Msg::HintHold))).child(key),
            (ShortcutMode::Hold, true) => row.child(text(l.t(Msg::HintRelease))).child(key),
        }
    }

    /// Confirmed words plus the tentative partial, wrapped to the card
    /// width. The caret is painted after layout at the real end of the text,
    /// so it follows wrapping instead of breaking the line.
    fn live_text(&self, p: Palette, cx: &App) -> AnyElement {
        let snap = &self.hub.read(cx).snap;
        let confirmed = snap.confirmed.trim_end();
        let partial = snap.partial.trim();
        if confirmed.is_empty() && partial.is_empty() {
            let hint = if snap.phase == Phase::Recording {
                lang(cx).t(Msg::Listening)
            } else {
                ""
            };
            return div().text_color(p.tentative).child(hint).into_any_element();
        }
        let sep = if confirmed.is_empty() || partial.is_empty() {
            ""
        } else {
            " "
        };
        perf::bump(&perf::LIVE_TEXTS);
        let text = format!("{confirmed}{sep}{partial}");
        let start = confirmed.len() + sep.len();
        let end = text.len();
        let highlight = HighlightStyle {
            color: Some(p.tentative.into()),
            ..Default::default()
        };
        let styled = StyledText::new(text).with_highlights([(start..end, highlight)]);
        let layout = styled.layout().clone();
        let caret = p.orange;
        div()
            .w_full()
            .child(styled)
            .when(snap.phase == Phase::Recording, |d| {
                d.child(
                    canvas(
                        move |_, _, _| (),
                        move |_, (), window, _| {
                            let line = layout.line_height();
                            if let Some(at) = layout.position_for_index(end) {
                                window.paint_quad(fill(
                                    Bounds::new(
                                        point(at.x + px(2.), at.y + line * 0.1),
                                        size(px(2.), line * 0.8),
                                    ),
                                    caret,
                                ));
                            }
                        },
                    )
                    .absolute()
                    .size_0(),
                )
            })
            .into_any_element()
    }

    /// Idle is a compact pill without header, like the Señal reference.
    fn idle(&self, p: Palette, cx: &mut Context<Self>) -> Div {
        let l = lang(cx);
        let snap = &self.hub.read(cx).snap;
        let needs_login = !snap.authenticated && !snap.demo;
        let status = snap.status.clone();
        // The detail lives in Ajustes; here a dot marks the failure and the
        // line stays in ink so it reads on every style.
        let failed = snap.error.is_some();
        let demo = snap.demo;
        let hint = self.shortcut_hint(false, p, cx);
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .pl(px(8.))
            .pr(px(if p.planet_corner { 40. } else { 10. }))
            .py(px(10.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(draggable(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(Self::grip(p))
                            .child(self.mic(p))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .flex()
                                    .flex_col()
                                    .gap(px(4.))
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap(px(6.))
                                            .font_family(DISPLAY_FONT)
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_size(px(13.5))
                                            .child(div().min_w(px(0.)).truncate().child(l.t(
                                                if needs_login {
                                                    Msg::ConnectToDictate
                                                } else {
                                                    Msg::ReadyToDictate
                                                },
                                            )))
                                            .when(demo, |d| d.child(demo_badge(p))),
                                    )
                                    .child(hint),
                            ),
                    ))
                    .child(
                        button("start", l.t(Msg::Dictate), Kind::Secondary, p)
                            .on_click(cx.listener(|this, _, _, cx| this.send(cx, Command::Start))),
                    )
                    .child(self.hide_button(p, cx)),
            )
            .when(needs_login, |d| {
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .pl(px(4.))
                        .text_size(px(12.5))
                        .text_color(if failed { p.text } else { p.text_2 })
                        .when(failed, |d| {
                            d.child(div().flex_none().size(px(7.)).rounded_full().bg(p.danger))
                        })
                        .child(div().flex_1().min_w(px(0.)).truncate().child(status))
                        .child(
                            icon_label_button(
                                "settings",
                                Icon::Provider,
                                l.t(Msg::Connect),
                                Kind::Ghost,
                                p,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.open_settings(Section::Provider, cx)
                            })),
                        ),
                )
            })
    }

    fn open_settings(&self, section: Section, cx: &mut Context<Self>) {
        self.hub
            .update(cx, |hub, cx| hub.show_settings_at(section, cx));
    }

    fn body(&mut self, p: Palette, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let l = lang(cx);
        let session = ui_state(cx).session;
        let hub = self.hub.read(cx);
        let snap = &hub.snap;
        let reduce = ui_state(cx).reduce_motion;
        match snap.phase {
            Phase::Idle => self.idle(p, cx).into_any_element(),
            Phase::Connecting => div()
                .flex()
                .flex_col()
                .gap(px(12.))
                .p(px(12.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .child(self.mic(p))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .flex()
                                .flex_col()
                                .gap(px(2.))
                                .child(
                                    div()
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(l.t(Msg::ConnectingGrok)),
                                )
                                .child(
                                    div()
                                        .min_w(px(0.))
                                        .line_clamp(2)
                                        .text_size(px(12.5))
                                        .text_color(p.text_2)
                                        .child(snap.status.clone()),
                                ),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .child(button("cancel", l.t(Msg::Cancel), Kind::Ghost, p).on_click(
                            cx.listener(|this, _, _, cx| this.send(cx, Command::Cancel)),
                        )),
                )
                .into_any_element(),
            Phase::Recording | Phase::Finalizing | Phase::Polishing => {
                let live = snap.phase == Phase::Recording;
                let status = snap.status.clone();
                let wave = voice(&hub.levels, p, live, reduce);
                if follows_end(&self.live_scroll) {
                    self.live_scroll.scroll_to_bottom();
                }
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(px(0.))
                    .gap(px(10.))
                    .p(px(12.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            // Órbita draws Mars inside the voice, at its centre.
                            .when(p.voice_style != VoiceStyle::Orbit, |d| d.child(self.mic(p)))
                            .child(div().flex_1().child(wave)),
                    )
                    .child(
                        div()
                            .id("live-text")
                            .flex_1()
                            .min_h(px(0.))
                            .overflow_y_scroll()
                            .track_scroll(&self.live_scroll)
                            .text_size(px(15.))
                            .line_height(px(21.))
                            .child(self.live_text(p, cx)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .gap(px(8.))
                            .when(live, |d| {
                                d.child(
                                    icon_label_button(
                                        "stop",
                                        Icon::Stop,
                                        l.t(Msg::Finish),
                                        Kind::Primary,
                                        p,
                                    )
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.send(cx, Command::Stop)),
                                    ),
                                )
                            })
                            .child(button("cancel", l.t(Msg::Cancel), Kind::Ghost, p).on_click(
                                cx.listener(|this, _, _, cx| this.send(cx, Command::Cancel)),
                            ))
                            .child(div().flex_1())
                            .child(if live {
                                self.shortcut_hint(true, p, cx)
                            } else {
                                div()
                                    .min_w(px(0.))
                                    .truncate()
                                    .text_size(px(12.))
                                    .text_color(p.text_2)
                                    .child(status)
                            }),
                    )
                    .into_any_element()
            }
            Phase::Ready => {
                let status = snap.status.clone();
                let text = self.field.read(cx).text();
                let edited = text != snap.original_text;
                let proposals: Vec<Proposal> =
                    corrections::propose(&snap.original_text, text, &snap.corrections)
                        .into_iter()
                        .filter(|p| !self.dismissed.contains(p))
                        .take(2)
                        .collect();
                let field_focused = self.field.read(cx).is_focused(window);
                let note = match (field_focused, edited) {
                    (true, _) => tf!(l, SaveEditKey, key = session.with_mod("Enter")),
                    (false, true) => tf!(l, EditedStatus, status = status),
                    (false, false) => status,
                };
                // The field takes whatever height is left and scrolls inside;
                // proposals, note and buttons below never move or shrink.
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(px(0.))
                    .gap(px(8.))
                    .p(px(12.))
                    .child(
                        div()
                            .flex_1()
                            .min_h(px(0.))
                            .flex()
                            .flex_col()
                            .text_size(px(15.))
                            .line_height(px(21.))
                            .child(self.field.clone()),
                    )
                    .children(proposals.into_iter().enumerate().map(|(i, prop)| {
                        let remember = prop.clone();
                        let dismiss = prop.clone();
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .gap(px(6.))
                            .text_size(px(12.5))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .truncate()
                                    .text_color(p.text_2)
                                    .child(tf!(
                                        l,
                                        RememberProposal,
                                        from = prop.from,
                                        to = prop.to
                                    )),
                            )
                            .child(
                                button(("remember", i), l.t(Msg::Remember), Kind::Secondary, p)
                                    .h(px(26.))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.send(
                                            cx,
                                            Command::RememberCorrection {
                                                from: remember.from.clone(),
                                                to: remember.to.clone(),
                                            },
                                        );
                                        this.dismissed.push(remember.clone());
                                        cx.notify();
                                    })),
                            )
                            .child(icon_button(("dismiss", i), Icon::Close, p).on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.dismissed.push(dismiss.clone());
                                    cx.notify();
                                }),
                            ))
                    }))
                    .child(
                        div()
                            .flex_none()
                            .min_w(px(0.))
                            .text_size(px(12.))
                            .text_color(p.text_2)
                            .truncate()
                            .child(note),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .gap(px(8.))
                            .child(
                                icon_label_button(
                                    "copy",
                                    Icon::Copy,
                                    l.t(Msg::Copy),
                                    Kind::Secondary,
                                    p,
                                )
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.flush_edit(cx);
                                        this.send(cx, Command::Copy);
                                    },
                                )),
                            )
                            .child(
                                icon_label_button(
                                    "paste",
                                    Icon::Paste,
                                    l.t(Msg::Paste),
                                    Kind::Secondary,
                                    p,
                                )
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.flush_edit(cx);
                                        this.send(cx, Command::Paste);
                                    },
                                )),
                            )
                            .child(div().flex_1())
                            .child(
                                button("again", l.t(Msg::NewDictation), Kind::Ghost, p).on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.flush_edit(cx);
                                        this.send(cx, Command::Start);
                                    }),
                                ),
                            ),
                    )
                    .into_any_element()
            }
            Phase::Error => {
                let origin = ErrorOrigin::of(snap).unwrap_or(ErrorOrigin::Dictation);
                let raw = snap.error.clone().unwrap_or_else(|| snap.status.clone());
                let message = origin.body(&raw);
                // Ink text: the title and the dot already say it failed, and
                // a long red paragraph is harder to read on every style.
                let actions = div().flex().flex_none().items_center().gap(px(8.));
                let actions = match origin {
                    ErrorOrigin::Microphone => actions
                        .child(
                            icon_label_button(
                                "pick-mic",
                                Icon::Microphone,
                                l.t(Msg::ChooseMic),
                                Kind::Secondary,
                                p,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.open_settings(Section::Microphone, cx)
                            })),
                        )
                        .child(
                            icon_label_button(
                                "retry",
                                Icon::Retry,
                                l.t(Msg::Retry),
                                Kind::Ghost,
                                p,
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.send(cx, Command::Start))),
                        ),
                    ErrorOrigin::Credential => actions
                        .child(
                            icon_label_button(
                                "provider",
                                Icon::Provider,
                                l.t(Msg::CheckConnection),
                                Kind::Secondary,
                                p,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.open_settings(Section::Provider, cx)
                            })),
                        )
                        .child(
                            icon_label_button(
                                "retry",
                                Icon::Retry,
                                l.t(Msg::Retry),
                                Kind::Ghost,
                                p,
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.send(cx, Command::Start))),
                        ),
                    ErrorOrigin::Dictation | ErrorOrigin::App => actions
                        .child(
                            icon_label_button(
                                "retry",
                                Icon::Retry,
                                l.t(Msg::Retry),
                                Kind::Secondary,
                                p,
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.send(cx, Command::Start))),
                        )
                        .child(
                            button("settings", l.t(Msg::Settings), Kind::Ghost, p).on_click(
                                cx.listener(|this, _, _, cx| this.send(cx, Command::ShowSettings)),
                            ),
                        ),
                };
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(px(0.))
                    .gap(px(10.))
                    .p(px(12.))
                    .child(
                        div()
                            .id("error")
                            .flex_1()
                            .min_h(px(0.))
                            .overflow_y_scroll()
                            .text_color(p.text)
                            .text_size(px(13.5))
                            .line_height(px(19.))
                            .child(message),
                    )
                    .child(actions)
                    .into_any_element()
            }
        }
    }
}

impl Render for Overlay {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        perf::bump(&perf::OVERLAY_RENDERS);
        let p = ui_state(cx).palette;
        let snap = &self.hub.read(cx).snap;
        let card_h = overlay_height(snap);
        // On Wayland the window keeps the tallest card height (see
        // `fixed_surface`); only the card takes pointer input, the
        // transparent rest lets clicks through to the window below.
        let bottom = fixed_surface() && self.placement.card_at_bottom();
        let s = window.viewport_size();
        let card_h = px(card_h).min(s.height);
        let top = if bottom { s.height - card_h } else { px(0.) };
        let region = Bounds::new(point(px(0.), top), size(s.width, card_h));
        // Only Wayland has input regions; elsewhere the window is the card.
        if fixed_surface() && self.input_region != Some(region) {
            window.set_input_region(Some(&[region]));
            self.input_region = Some(region);
        }
        let idle = snap.phase == Phase::Idle;
        let header = (!idle).then(|| self.header(p, cx));
        let body = self.body(p, window, cx);
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
                    .id("card")
                    .w_full()
                    .h(card_h)
                    .flex_none()
                    .relative()
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .rounded(px(p.radius))
                    .bg(p.surface)
                    .border_1()
                    .border_color(p.line)
                    // Decoration first, so every control and text paints
                    // over it. `overflow_hidden` clips to a rectangle only;
                    // the corner rounds itself (see rounded-corners review).
                    .child(grain(p, f32::from(s.width), f32::from(card_h)))
                    .when(p.planet_corner, |d| {
                        let planet = if idle { PLANET_IDLE } else { PLANET_CARD };
                        d.child(
                            img(assets::art(assets::PLANET_CORNER.clone()))
                                .absolute()
                                .top(px(0.))
                                .right(px(0.))
                                .size(px(planet))
                                .rounded_tr(px(p.radius - 1.)),
                        )
                    })
                    .children(header)
                    .child(body),
            )
    }
}
