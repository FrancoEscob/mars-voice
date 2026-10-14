//! Ajustes → Escritura: every field of `WriterConfig` (activación, cuenta,
//! resultado, explicaciones, modelo y preferencias, ventana), plus fixing or
//! translating the clipboard now. Edits go through `Hub::update_config`,
//! validated with `AppConfig::validate`. Credentials are the dictation's.

use dicta_core::{
    Command,
    writer::{TARGET_LANGUAGES, WriterExplain, WriterMode, WriterStrength, WriterTone},
};
use gpui::{Context, Div, ElementId, FontWeight, SharedString, Stateful, div, prelude::*, px};

use crate::{
    WRITER_APP_ID, WRITER_WIDTH,
    assets::Icon,
    i18n::{Lang, Msg, tf},
    lang,
    lectura::{setting_row, subheading},
    settings::{Consent, Section, Settings, group, hint, labelled, session_hint},
    theme::Palette,
    ui_state,
    widgets::{
        Kind, demo_badge, icon, icon_label_button, kbd, radio_row, segment, segmented, toggle_row,
    },
    writer::{mode_label, native_name},
};

const MODES: [WriterMode; 3] = [WriterMode::Auto, WriterMode::Fix, WriterMode::Translate];
const EXPLAIN: [WriterExplain; 3] = [
    WriterExplain::Off,
    WriterExplain::Brief,
    WriterExplain::Detailed,
];
const TONES: [WriterTone; 4] = [
    WriterTone::Keep,
    WriterTone::Casual,
    WriterTone::Neutral,
    WriterTone::Formal,
];

fn mode_note(mode: WriterMode, l: Lang) -> &'static str {
    l.t(match mode {
        WriterMode::Auto => Msg::WriterModeAutoNote,
        WriterMode::Fix => Msg::WriterModeFixNote,
        WriterMode::Translate => Msg::WriterModeTranslateNote,
    })
}

fn explain_label(e: WriterExplain, l: Lang) -> &'static str {
    l.t(match e {
        WriterExplain::Off => Msg::ExplainOff,
        WriterExplain::Brief => Msg::ExplainBrief,
        WriterExplain::Detailed => Msg::ExplainDetailed,
    })
}

fn tone_label(t: WriterTone, l: Lang) -> &'static str {
    l.t(match t {
        WriterTone::Keep => Msg::ToneKeep,
        WriterTone::Casual => Msg::ToneCasual,
        WriterTone::Neutral => Msg::ToneNeutral,
        WriterTone::Formal => Msg::ToneFormal,
    })
}

/// A language chip, in its own name, like the voice chips of Lectura.
fn chip(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    selected: bool,
    p: Palette,
) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap(px(6.))
        .h(px(30.))
        .px(px(11.))
        .rounded(px(8.))
        .border_1()
        .border_color(if selected { p.control } else { p.line })
        .bg(if selected { p.selected } else { p.surface })
        .cursor_pointer()
        .hover(move |s| s.border_color(p.control))
        .text_size(px(13.))
        .font_weight(if selected {
            FontWeight::SEMIBOLD
        } else {
            FontWeight::MEDIUM
        })
        .when(selected, |d| d.child(icon(Icon::Check, 13., p.text)))
        .child(label.into())
}

impl Settings {
    pub(crate) fn writer(&self, p: Palette, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_col()
            .child(self.writer_now(p, cx))
            .child(self.writer_activation(p, cx))
            .child(self.writer_account(p, cx))
            .child(self.writer_result(p, cx))
            .child(self.writer_explanations(p, cx))
            .child(self.writer_model(p, cx))
            .child(self.writer_window(p, cx))
    }

    fn writer_now(&self, p: Palette, cx: &mut Context<Self>) -> Div {
        let l = lang(cx);
        let snap = &self.hub.read(cx).snap;
        let key = snap.config.writer.shortcut.clone();
        let demo = snap.demo;
        group(l.t(Msg::WriterNow), p)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(6.))
                    .when(!key.trim().is_empty(), |d| d.child(kbd(key, p)))
                    .child(hint(l.t(Msg::WriterNowHint), p))
                    .when(demo, |d| d.child(demo_badge(p))),
            )
            .child(
                div().flex().child(
                    icon_label_button(
                        "writer-clipboard",
                        Icon::Paste,
                        l.t(Msg::WriterClipboard),
                        Kind::Secondary,
                        p,
                    )
                    .on_click(
                        cx.listener(|this, _, _, cx| this.send(cx, Command::WriterClipboard)),
                    ),
                ),
            )
            .child(hint(
                l.t(if demo {
                    Msg::WriterNowDemo
                } else {
                    Msg::WriterNowPrivacy
                }),
                p,
            ))
    }

    fn writer_activation(&self, p: Palette, cx: &mut Context<Self>) -> Div {
        let l = lang(cx);
        let hyprland = ui_state(cx).session.is_hyprland();
        let restore = self.hub.read(cx).snap.config.writer.restore_clipboard;
        group(l.t(Msg::Activation), p)
            .child(div().max_w(px(320.)).child(labelled(
                l.t(Msg::WriterShortcut),
                &self.fields.writer_shortcut,
                p,
            )))
            .child(hint(l.t(Msg::WriterShortcutHint), p))
            .children(session_hint(hyprland, l.t(Msg::HyprWriterShortcut), p))
            .child(
                toggle_row(
                    "writer-restore-clipboard",
                    l.t(Msg::RestoreClipboard),
                    Some(l.t(Msg::WriterRestoreClipboardNote)),
                    restore,
                    p,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.edit(cx, |c| {
                        c.writer.restore_clipboard = !c.writer.restore_clipboard
                    })
                })),
            )
            .child(hint(l.t(Msg::ClipboardHistoryWarning), p))
    }

    /// The same two options as Proveedor and Lectura, with the same
    /// subscription consent the first time.
    fn writer_account(&self, p: Palette, cx: &mut Context<Self>) -> Div {
        let l = lang(cx);
        let mode = self.hub.read(cx).snap.config.writer.provider;
        group(l.t(Msg::Account), p)
            .children(self.account_options(Consent::Writer, mode, p, cx))
            .child(hint(l.t(Msg::WriterOwnMode), p))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        icon_label_button(
                            "writer-provider",
                            Icon::Provider,
                            l.t(Msg::GoToProvider),
                            Kind::Ghost,
                            p,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.go(Section::Provider, cx))),
                    )
                    .child(hint(l.t(Msg::WriterCredentials), p)),
            )
    }

    fn writer_result(&self, p: Palette, cx: &mut Context<Self>) -> Div {
        let l = lang(cx);
        let c = self.hub.read(cx).snap.config.writer.clone();
        group(l.t(Msg::WriterResult), p)
            .child(subheading(l.t(Msg::WriterDefaultMode), p))
            .children(MODES.map(|mode| {
                radio_row(
                    ("writer-default-mode", mode as usize),
                    mode_label(mode, l),
                    Some(mode_note(mode, l).into()),
                    c.default_mode == mode,
                    p,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.edit(cx, |c| c.writer.default_mode = mode)
                }))
            }))
            .child(subheading(l.t(Msg::WriterTarget), p))
            .child(div().flex().flex_wrap().gap(px(6.)).children(
                TARGET_LANGUAGES.iter().enumerate().map(|(i, &code)| {
                    chip(
                        ("writer-target", i),
                        native_name(code),
                        c.target_language == code,
                        p,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.edit(cx, |c| c.writer.target_language = code.into())
                    }))
                }),
            ))
            .child(hint(l.t(Msg::WriterTargetHint), p))
            .child(subheading(l.t(Msg::WriterStrength), p))
            .child(
                radio_row(
                    "writer-minimal",
                    l.t(Msg::StrengthMinimal),
                    Some(l.t(Msg::StrengthMinimalNote).into()),
                    c.strength == WriterStrength::Minimal,
                    p,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.edit(cx, |c| c.writer.strength = WriterStrength::Minimal)
                })),
            )
            .child(
                radio_row(
                    "writer-natural",
                    l.t(Msg::StrengthNatural),
                    Some(l.t(Msg::StrengthNaturalNote).into()),
                    c.strength == WriterStrength::Natural,
                    p,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.edit(cx, |c| c.writer.strength = WriterStrength::Natural)
                })),
            )
            .child(setting_row(
                l.t(Msg::WriterTone),
                segmented(
                    TONES.map(|tone| {
                        segment(
                            ("writer-tone", tone as usize),
                            tone_label(tone, l),
                            c.tone == tone,
                            p,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.edit(cx, |c| c.writer.tone = tone)
                        }))
                        .into_any_element()
                    }),
                    p,
                )
                .flex_wrap()
                .flex_shrink(1.)
                .min_w(px(0.)),
            ))
            .child(
                toggle_row(
                    "writer-unslop",
                    l.t(Msg::Unslop),
                    Some(l.t(Msg::UnslopNote)),
                    c.unslop,
                    p,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.edit(cx, |c| c.writer.unslop = !c.writer.unslop)
                })),
            )
            .child(
                toggle_row(
                    "writer-copy",
                    l.t(Msg::WriterCopyOnFinish),
                    Some(l.t(Msg::WriterCopyOnFinishNote)),
                    c.copy_on_finish,
                    p,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.edit(cx, |c| c.writer.copy_on_finish = !c.writer.copy_on_finish)
                })),
            )
    }

    fn writer_explanations(&self, p: Palette, cx: &mut Context<Self>) -> Div {
        let l = lang(cx);
        let c = self.hub.read(cx).snap.config.writer.clone();
        let auto = l.t(Msg::Automatic);
        group(l.t(Msg::Explanations), p)
            .child(setting_row(
                l.t(Msg::ExplainLevel),
                segmented(
                    EXPLAIN.map(|e| {
                        segment(
                            ("writer-explain", e as usize),
                            explain_label(e, l),
                            c.explain == e,
                            p,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.edit(cx, |c| c.writer.explain = e)
                        }))
                        .into_any_element()
                    }),
                    p,
                ),
            ))
            .child(hint(l.t(Msg::ExplainLevelHint), p))
            .when(c.explain != WriterExplain::Off, |d| {
                d.child(subheading(l.t(Msg::ExplainLanguage), p))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(px(6.))
                            .child(
                                chip("writer-explain-auto", auto, c.explain_language == "auto", p)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.edit(cx, |c| c.writer.explain_language = "auto".into())
                                    })),
                            )
                            .children(TARGET_LANGUAGES.iter().enumerate().map(|(i, &code)| {
                                chip(
                                    ("writer-explain-lang", i),
                                    native_name(code),
                                    c.explain_language == code,
                                    p,
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.edit(cx, |c| c.writer.explain_language = code.into())
                                    },
                                ))
                            })),
                    )
                    .child(hint(l.t(Msg::ExplainLanguageHint), p))
            })
    }

    fn writer_model(&self, p: Palette, cx: &mut Context<Self>) -> Div {
        let l = lang(cx);
        let session = ui_state(cx).session;
        group(l.t(Msg::ModelAndPreferences), p)
            .child(div().max_w(px(260.)).child(labelled(
                l.t(Msg::Model),
                &self.fields.writer_model,
                p,
            )))
            .child(labelled(
                l.t(Msg::WriterPreferences),
                &self.fields.writer_instructions,
                p,
            ))
            .child(hint(
                tf!(l, WriterInstructionsHint, key = session.with_mod("Enter")),
                p,
            ))
    }

    fn writer_window(&self, p: Palette, cx: &mut Context<Self>) -> Div {
        let l = lang(cx);
        let hyprland = ui_state(cx).session.is_hyprland();
        let c = self.hub.read(cx).snap.config.writer.clone();
        let center = ((self.display_w - WRITER_WIDTH) / 2.).round().max(0.);
        let presets: [(Msg, f32, f32); 4] = [
            (Msg::TopLeft, 24., 24.),
            (Msg::TopCentre, center, 24.),
            (Msg::TopRight, -24., 24.),
            (Msg::BottomRight, -24., -48.),
        ];
        let (x, y, centered) = (c.window_x, c.window_y, c.window_centered);
        group(l.t(Msg::Window), p)
            .child(hint(l.t(Msg::WriterDragHint), p))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(6.))
                    .child(
                        segment("writer-center", l.t(Msg::Centre), centered, p)
                            .border_1()
                            .border_color(if centered { p.control } else { p.line })
                            .when(centered, |d| d.bg(p.selected))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.edit(cx, |c| c.writer.window_centered = true)
                            })),
                    )
                    .children(
                        presets
                            .into_iter()
                            .enumerate()
                            .map(|(i, (label, px_, py_))| {
                                let selected =
                                    !centered && (x - px_).abs() < 0.5 && (y - py_).abs() < 0.5;
                                segment(("writer-pos", i), l.t(label), selected, p)
                                    .border_1()
                                    .border_color(if selected { p.control } else { p.line })
                                    .when(selected, |d| d.bg(p.selected))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.edit(cx, |c| {
                                            c.writer.window_centered = false;
                                            c.writer.window_x = px_;
                                            c.writer.window_y = py_;
                                        })
                                    }))
                            }),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap(px(12.))
                    .child(div().w(px(140.)).child(labelled(
                        l.t(Msg::Horizontal),
                        &self.fields.writer_x,
                        p,
                    )))
                    .child(div().w(px(140.)).child(labelled(
                        l.t(Msg::Vertical),
                        &self.fields.writer_y,
                        p,
                    ))),
            )
            .child(hint(l.t(Msg::ReaderOffsetsHint), p))
            .child(hint(l.t(Msg::DesktopMayPlace), p))
            .children(session_hint(
                hyprland,
                tf!(l, HyprWriterRules, class = WRITER_APP_ID),
                p,
            ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_option_is_named_and_valid() {
        for l in Lang::ALL {
            for mode in MODES {
                assert!(!mode_note(mode, l).is_empty());
            }
            for e in EXPLAIN {
                assert!(!explain_label(e, l).is_empty());
            }
            for t in TONES {
                assert!(!tone_label(t, l).is_empty());
            }
        }
        // Every language the window offers is accepted by validation, as a
        // target and as the explanations' language.
        let mut c = dicta_core::AppConfig::default();
        for code in TARGET_LANGUAGES {
            c.writer.target_language = code.into();
            c.writer.explain_language = code.into();
            c.validate().unwrap();
        }
        c.writer.explain_language = "auto".into();
        c.validate().unwrap();
    }
}
