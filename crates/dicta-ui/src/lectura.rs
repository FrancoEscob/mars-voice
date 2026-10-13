//! Ajustes → Lectura: every field of `TtsConfig`, grouped like PLAN-TTS §2
//! (activación, cuenta, voz, generación, reproducción, texto y
//! pronunciación, ventana, historial y coste), plus the read-now actions.
//! Edits go through `Hub::update_config`, validated with
//! `AppConfig::validate`. Credentials are the dictation's: the same login,
//! key and disconnect commands.

use dicta_core::{
    AudioRetention, Command, ProviderMode,
    tts::{
        GENERATION_SPEED_MAX, GENERATION_SPEED_MIN, Lookahead, MAX_PRONUNCIATIONS, NewTextAction,
        OnDictation, Pronunciation, RATE_MAX, RATE_MIN, SameTextAction, TtsSource, TtsStyle,
    },
};
use gpui::{Context, Div, FontWeight, SharedString, div, prelude::*, px};

use crate::{
    READER_APP_ID, READER_WIDTH,
    assets::Icon,
    format,
    i18n::{Lang, Msg, tf},
    lang,
    reader::{RATE_PRESETS, fmt_rate, step_rate, stepper},
    session::Session,
    settings::{
        Consent, SUBSCRIPTION_HINT, Section, Settings, connect_command, group, hint, labelled,
        retention_label, session_hint,
    },
    theme::Palette,
    ui_state,
    widgets::{
        Kind, badge, button, demo_badge, disabled, icon, icon_button, icon_label_button, kbd,
        radio_row, segment, segmented, toggle_row,
    },
};

/// "20.000", "20,000", "20000" or "20 000" → 20000, whatever the language:
/// a count has no decimals. Ranges are checked by `TtsConfig::validate`.
pub(crate) fn parse_chars(text: &str, l: Lang) -> Result<u32, String> {
    let digits: String = text
        .chars()
        .filter(|c| !matches!(c, '.' | ',' | ' ' | '\u{a0}' | '_'))
        .collect();
    digits
        .parse::<u32>()
        .map_err(|_| l.t(Msg::ErrChars).to_string())
}

/// "1,00" / "1.00" for the generation speed.
fn fmt_speed(v: f32, l: Lang) -> String {
    format::decimal(f64::from(v), 2, l)
}

/// Generation speed `v + delta` on the 0,05 grid within the API range.
fn step_speed(v: f32, delta: f32) -> f32 {
    (((v + delta) * 20.).round() / 20.).clamp(GENERATION_SPEED_MIN, GENERATION_SPEED_MAX)
}

fn fmt_window(seconds: u16) -> String {
    match seconds {
        s if s % 60 == 0 => format!("{} min", s / 60),
        s if s > 60 => format!("{} min {} s", s / 60, s % 60),
        s => format!("{s} s"),
    }
}

/// Short line inside a group that starts a sub-topic.
fn subheading(text: impl Into<SharedString>, p: Palette) -> Div {
    div()
        .pt(px(6.))
        .text_size(px(12.5))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(p.text_2)
        .child(text.into())
}

/// A label on the left and a control on the right that wraps below on
/// narrow columns.
fn setting_row(label: &'static str, control: impl IntoElement) -> Div {
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(10.))
        .child(
            div()
                .flex_1()
                .min_w(px(160.))
                .font_weight(FontWeight::MEDIUM)
                .child(label),
        )
        .child(control)
}

/// Name of a text source; `TtsSource::label` is Spanish.
pub(crate) fn source_label(source: TtsSource, l: Lang) -> &'static str {
    l.t(match source {
        TtsSource::CopySelection => Msg::SourceCopy,
        TtsSource::SelectionThenClipboard => Msg::SourceSelectionClipboard,
        TtsSource::Selection => Msg::SourceSelection,
        TtsSource::Clipboard => Msg::SourceClipboard,
    })
}

/// What a source needs and does, with the copy keys of the session.
pub(crate) fn source_detail(source: TtsSource, l: Lang, session: Session) -> String {
    match source {
        TtsSource::CopySelection => {
            let keys = if session.is_linux() {
                l.t(Msg::LinuxCopyKeys).to_string()
            } else {
                session.with_mod("C")
            };
            tf!(l, SourceCopyDetail, keys = keys)
        }
        TtsSource::SelectionThenClipboard => l.t(Msg::SourceSelectionClipboardDetail).into(),
        TtsSource::Selection => l.t(Msg::SourceSelectionDetail).into(),
        TtsSource::Clipboard => l.t(Msg::SourceClipboardDetail).into(),
    }
}

/// "En Hyprland: …" under the sources that need the primary selection.
pub(crate) fn source_session_hint(
    source: TtsSource,
    l: Lang,
    session: Session,
) -> Option<&'static str> {
    (session.is_hyprland()
        && matches!(
            source,
            TtsSource::SelectionThenClipboard | TtsSource::Selection
        ))
    .then(|| l.t(Msg::HyprPrimarySelection))
}

/// Window class typed for the terminal list: trimmed, non-empty, not
/// already listed (case-insensitive). Length is checked by `validate`.
pub(crate) fn new_terminal_class(
    typed: &str,
    listed: &[String],
    l: Lang,
) -> Result<String, String> {
    let class = typed.trim();
    if class.is_empty() {
        return Err(l.t(Msg::ErrTerminalEmpty).into());
    }
    if listed.iter().any(|c| c.trim().eq_ignore_ascii_case(class)) {
        return Err(tf!(l, ErrTerminalListed, class = class));
    }
    Ok(class.to_string())
}

fn lookahead_label(a: Lookahead, l: Lang) -> &'static str {
    l.t(match a {
        Lookahead::Window => Msg::LookaheadWindow,
        Lookahead::All => Msg::LookaheadAll,
    })
}

fn lookahead_detail(a: Lookahead, l: Lang) -> &'static str {
    l.t(match a {
        Lookahead::Window => Msg::LookaheadWindowDetail,
        Lookahead::All => Msg::LookaheadAllDetail,
    })
}

fn same_text_label(a: SameTextAction, l: Lang) -> &'static str {
    l.t(match a {
        SameTextAction::TogglePause => Msg::PauseResume,
        SameTextAction::Restart => Msg::SameTextRestart,
    })
}

fn new_text_label(a: NewTextAction, l: Lang) -> &'static str {
    l.t(match a {
        NewTextAction::Replace => Msg::NewTextReplace,
        NewTextAction::Queue => Msg::NewTextQueue,
    })
}

fn style_label(s: TtsStyle, l: Lang) -> &'static str {
    l.t(match s {
        TtsStyle::Normal => Msg::StyleNormal,
        TtsStyle::Soft => Msg::StyleSoft,
        TtsStyle::Whisper => Msg::StyleWhisper,
        TtsStyle::Slow => Msg::StyleSlow,
        TtsStyle::Fast => Msg::StyleFast,
        TtsStyle::SlowSoft => Msg::StyleSlowSoft,
    })
}

fn on_dictation_label(o: OnDictation, l: Lang) -> &'static str {
    l.t(match o {
        OnDictation::Pause => Msg::OnDictationPause,
        OnDictation::Stop => Msg::OnDictationStop,
    })
}

impl Settings {
    pub(crate) fn reading(&self, p: Palette, width: f32, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_col()
            .child(self.reading_now(p, cx))
            .child(self.reading_activation(p, cx))
            .child(self.reading_account(p, cx))
            .child(self.reading_voice(p, cx))
            .child(self.reading_generation(p, cx))
            .child(self.reading_playback(p, cx))
            .child(self.reading_text(p, cx))
            .child(self.reading_window(p, width, cx))
            .child(self.reading_history(p, cx))
    }

    fn read_pasted(&mut self, cx: &mut Context<Self>) {
        let l = lang(cx);
        let text = self.fields.tts_paste.read(cx).text().trim().to_string();
        if text.is_empty() {
            return self.fail(l.t(Msg::ErrNothingToRead).into(), cx);
        }
        self.send(cx, Command::ReadText(text));
        self.error = None;
        self.notice = Some(l.t(Msg::TextSentToReader).into());
        cx.notify();
    }

    pub(crate) fn add_terminal_class(&mut self, cx: &mut Context<Self>) {
        let typed = self.fields.tts_terminal.read(cx).text().to_string();
        let listed = self.hub.read(cx).snap.config.tts.terminal_classes.clone();
        match new_terminal_class(&typed, &listed, lang(cx)) {
            Ok(class) => {
                self.edit(cx, |c| c.tts.terminal_classes.push(class));
                if self.error.is_none() {
                    self.fields.tts_terminal.update(cx, |f, cx| f.clear(cx));
                }
            }
            Err(e) => self.fail(e, cx),
        }
    }

    pub(crate) fn add_pronunciation(&mut self, cx: &mut Context<Self>) {
        let from = self.fields.tts_pron_from.read(cx).text().trim().to_string();
        let to = self.fields.tts_pron_to.read(cx).text().trim().to_string();
        if from.is_empty() || to.is_empty() {
            return self.fail(lang(cx).t(Msg::ErrPronunciationEmpty).into(), cx);
        }
        self.edit(cx, |c| {
            c.tts.pronunciations.push(Pronunciation { from, to })
        });
        if self.error.is_none() {
            self.fields.tts_pron_from.update(cx, |f, cx| f.clear(cx));
            self.fields.tts_pron_to.update(cx, |f, cx| f.clear(cx));
        }
    }

    fn reading_now(&self, p: Palette, cx: &mut Context<Self>) -> Div {
        let l = lang(cx);
        let snap = &self.hub.read(cx).snap;
        let key = snap.config.tts.read_shortcut.clone();
        let demo = snap.demo;
        group(l.t(Msg::ReadNow), p)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(6.))
                    .when(!key.trim().is_empty(), |d| d.child(kbd(key, p)))
                    .child(hint(l.t(Msg::ReadNowHint), p))
                    .when(demo, |d| d.child(demo_badge(p))),
            )
            .child(labelled(
                l.t(Msg::PasteTextToRead),
                &self.fields.tts_paste,
                p,
            ))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        icon_label_button(
                            "tts-read-text",
                            Icon::Read,
                            l.t(Msg::Read),
                            Kind::Primary,
                            p,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.read_pasted(cx))),
                    )
                    .child(
                        icon_label_button(
                            "tts-read-clipboard",
                            Icon::Paste,
                            l.t(Msg::ReadClipboard),
                            Kind::Secondary,
                            p,
                        )
                        .on_click(
                            cx.listener(|this, _, _, cx| this.send(cx, Command::ReadClipboard)),
                        ),
                    ),
            )
            .child(hint(
                l.t(if demo {
                    Msg::ReadNowDemo
                } else {
                    Msg::ReadNowPrivacy
                }),
                p,
            ))
    }

    fn reading_activation(&self, p: Palette, cx: &mut Context<Self>) -> Div {
        let l = lang(cx);
        let session = ui_state(cx).session;
        let c = self.hub.read(cx).snap.config.tts.clone();
        let field =
            |label: &'static str, f| div().flex_1().min_w(px(170.)).child(labelled(label, f, p));
        group(l.t(Msg::Activation), p)
            .child(subheading(l.t(Msg::Shortcuts), p))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(12.))
                    .child(field(
                        l.t(Msg::ReadSelection),
                        &self.fields.tts_read_shortcut,
                    ))
                    .child(field(
                        l.t(Msg::PauseResume),
                        &self.fields.tts_pause_shortcut,
                    ))
                    .child(field(l.t(Msg::Stop), &self.fields.tts_stop_shortcut)),
            )
            .child(hint(l.t(Msg::ReadShortcutsHint), p))
            .children(session_hint(
                session.is_hyprland(),
                l.t(Msg::HyprReadShortcuts),
                p,
            ))
            .child(subheading(l.t(Msg::WhatShortcutReads), p))
            .children({
                // The options of the copy source sit right under its radio.
                let mut rows = Vec::new();
                for source in TtsSource::ALL {
                    let detail = match source_session_hint(source, l, session) {
                        Some(extra) => format!("{} {extra}", source_detail(source, l, session)),
                        None => source_detail(source, l, session),
                    };
                    rows.push(
                        radio_row(
                            ("tts-source", source as usize),
                            source_label(source, l),
                            Some(detail.into()),
                            c.source == source,
                            p,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.edit(cx, |c| c.tts.source = source)
                        }))
                        .into_any_element(),
                    );
                    if source == TtsSource::CopySelection && c.source == source {
                        rows.push(self.copy_selection_options(&c, p, cx).into_any_element());
                    }
                }
                rows
            })
            .child(subheading(l.t(Msg::IfReading), p))
            .child(setting_row(
                l.t(Msg::SameText),
                segmented(
                    SameTextAction::ALL.into_iter().map(|a| {
                        segment(
                            ("tts-same", a as usize),
                            same_text_label(a, l),
                            c.same_text == a,
                            p,
                        )
                        .on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.edit(cx, |c| c.tts.same_text = a)
                            }),
                        )
                        .into_any_element()
                    }),
                    p,
                ),
            ))
            .child(setting_row(
                l.t(Msg::NewText),
                segmented(
                    NewTextAction::ALL.into_iter().map(|a| {
                        segment(
                            ("tts-new", a as usize),
                            new_text_label(a, l),
                            c.new_text == a,
                            p,
                        )
                        .on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.edit(cx, |c| c.tts.new_text = a)
                            }),
                        )
                        .into_any_element()
                    }),
                    p,
                ),
            ))
            .child(subheading(l.t(Msg::Limits), p))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(12.))
                    .child(div().w(px(200.)).child(labelled(
                        l.t(Msg::CharsPerReading),
                        &self.fields.tts_max_chars,
                        p,
                    )))
                    .child(div().w(px(200.)).child(labelled(
                        l.t(Msg::ConfirmFrom),
                        &self.fields.tts_confirm_chars,
                        p,
                    ))),
            )
            .child(hint(l.t(Msg::LimitsHint), p))
    }

    /// Options of `CopySelection`: restoring the clipboard and the extra
    /// terminal classes, indented under their radio.
    fn copy_selection_options(
        &self,
        c: &dicta_core::TtsConfig,
        p: Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let l = lang(cx);
        let hyprland = ui_state(cx).session.is_hyprland();
        let terminals = c.terminal_classes.clone();
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .ml(px(36.))
            .pl(px(14.))
            .border_l_2()
            .border_color(p.line)
            .child(
                toggle_row(
                    "tts-restore-clipboard",
                    l.t(Msg::RestoreClipboard),
                    Some(l.t(Msg::RestoreClipboardNote)),
                    c.restore_clipboard,
                    p,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.edit(cx, |c| c.tts.restore_clipboard = !c.tts.restore_clipboard)
                })),
            )
            .child(subheading(tf!(l, ExtraTerminals, n = terminals.len()), p))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .child(self.fields.tts_terminal.clone()),
                    )
                    .child(
                        button("tts-add-terminal", l.t(Msg::Add), Kind::Secondary, p)
                            .on_click(cx.listener(|this, _, _, cx| this.add_terminal_class(cx))),
                    ),
            )
            .child(hint(l.t(Msg::TerminalsHint), p))
            .children(session_hint(hyprland, l.t(Msg::HyprWindowClass), p))
            .children(terminals.into_iter().enumerate().map(|(i, class)| {
                let remove = class.clone();
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .py(px(4.))
                    .border_b_1()
                    .border_color(p.line)
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .font_family(crate::assets::MONO_FONT)
                            .text_size(px(13.))
                            .child(class),
                    )
                    .child(
                        icon_button(("tts-rm-terminal", i), Icon::Close, p).on_click(cx.listener(
                            move |this, _, _, cx| {
                                let remove = remove.clone();
                                this.edit(cx, |c| c.tts.terminal_classes.retain(|x| *x != remove))
                            },
                        )),
                    )
            }))
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(8.))
                    .p(px(10.))
                    .rounded(px(10.))
                    .bg(p.surface)
                    .border_1()
                    .border_color(p.line)
                    .child(div().pt(px(1.)).child(icon(Icon::Alert, 15., p.text_2)))
                    .child(
                        hint(l.t(Msg::ClipboardHistoryWarning), p)
                            .flex_1()
                            .min_w(px(0.)),
                    ),
            )
    }

    fn reading_account(&self, p: Palette, cx: &mut Context<Self>) -> Div {
        let l = lang(cx);
        let snap = &self.hub.read(cx).snap;
        let mode = snap.config.tts.provider;
        let shared = mode == snap.config.provider;
        let ready = snap.tts.account_ready;
        let demo = snap.demo;
        let login = snap.login.clone();
        let state = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(8.))
            .p(px(10.))
            .rounded(px(10.))
            .bg(p.surface)
            .border_1()
            .border_color(p.line)
            .child(
                div()
                    .size(px(9.))
                    .rounded_full()
                    .when(ready, |d| d.bg(p.voice))
                    .when(!ready, |d| d.border_1().border_color(p.control)),
            )
            .child(div().font_weight(FontWeight::MEDIUM).child(l.t(if ready {
                Msg::CredentialForReading
            } else {
                Msg::NoCredentialForMode
            })))
            .when(mode == ProviderMode::Subscription, |d| {
                d.child(badge(l.t(Msg::ExperimentalBadge), true, p))
            })
            .when(demo, |d| {
                d.child(demo_badge(p)).child(hint(l.t(Msg::Simulated), p))
            });
        let mut g = group(l.t(Msg::Account), p)
            .children(self.account_options(Consent::Reading, mode, p, cx))
            .child(hint(l.t(Msg::ReadingOwnMode), p))
            .child(state)
            .child(hint(l.t(Msg::ReadingCoverage), p));
        g = match mode {
            ProviderMode::Subscription if !ready => match login {
                Some(prompt) => g.child(self.login_panel(prompt, demo, p, cx)),
                None => {
                    g.child(hint(l.t(SUBSCRIPTION_HINT), p))
                        .child(div().flex().child(
                            button("tts-login", l.t(Msg::ConnectGrok), Kind::Primary, p).on_click(
                                cx.listener(|this, _, _, cx| this.send(cx, connect_command())),
                            ),
                        ))
                }
            },
            ProviderMode::ApiKey => g.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(div().flex_1().child(self.fields.api_key.clone()))
                    .child(
                        button("tts-save-key", l.t(Msg::SaveKey), Kind::Secondary, p)
                            .on_click(cx.listener(|this, _, _, cx| this.save_api_key(cx))),
                    ),
            ),
            ProviderMode::Subscription => g,
        };
        if ready {
            g = if shared {
                g.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(8.))
                        .child(
                            button("tts-disconnect", l.t(Msg::Disconnect), Kind::Ghost, p)
                                .on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.send(cx, Command::Disconnect)
                                    }),
                                ),
                        )
                        .child(hint(l.t(Msg::AlsoDisconnectsDictation), p)),
                )
            } else {
                g.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(8.))
                        .child(
                            icon_label_button(
                                "tts-provider",
                                Icon::Provider,
                                l.t(Msg::GoToProvider),
                                Kind::Ghost,
                                p,
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.go(Section::Provider, cx))),
                        )
                        .child(hint(l.t(Msg::ToDisconnectCredential), p)),
                )
            };
        }
        g
    }

    fn reading_voice(&self, p: Palette, cx: &mut Context<Self>) -> Div {
        let l = lang(cx);
        let snap = &self.hub.read(cx).snap;
        let c = snap.config.tts.clone();
        let voices = snap.tts.voices.clone();
        let previewing = snap.tts.previewing_voice.clone();
        let demo = snap.demo;
        let mut chips: Vec<(String, String, bool)> = voices
            .iter()
            .map(|v| {
                let name = if v.custom {
                    tf!(l, VoiceCustom, name = v.name)
                } else {
                    v.name.clone()
                };
                (v.id.clone(), name, v.id == c.voice)
            })
            .collect();
        if !voices.iter().any(|v| v.id == c.voice) {
            chips.insert(
                0,
                (
                    c.voice.clone(),
                    tf!(l, VoiceUnavailable, name = c.voice),
                    true,
                ),
            );
        }
        let languages = [
            ("auto", l.t(Msg::Automatic)),
            ("es-ES", "es-ES"),
            ("es-MX", "es-MX"),
            ("en", l.t(Msg::LangEnglish)),
        ];
        let current_voice = c.voice.clone();
        let is_previewing = previewing.as_deref() == Some(c.voice.as_str());
        group(l.t(Msg::Voice), p)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(6.))
                    .children(
                        chips
                            .into_iter()
                            .enumerate()
                            .map(|(i, (id, name, selected))| {
                                div()
                                    .id(("tts-voice", i))
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
                                    .child(name)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        let id = id.clone();
                                        this.edit(cx, |c| c.tts.voice = id)
                                    }))
                            }),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        if is_previewing {
                            icon_label_button(
                                "tts-preview",
                                Icon::Stop,
                                l.t(Msg::StopSample),
                                Kind::Secondary,
                                p,
                            )
                        } else {
                            icon_label_button(
                                "tts-preview",
                                Icon::Play,
                                l.t(Msg::TryVoice),
                                Kind::Secondary,
                                p,
                            )
                        }
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.send(cx, Command::PreviewVoice(current_voice.clone()))
                        })),
                    )
                    .child(
                        icon_label_button(
                            "tts-refresh-voices",
                            Icon::Refresh,
                            l.t(Msg::RefreshList),
                            Kind::Ghost,
                            p,
                        )
                        .on_click(
                            cx.listener(|this, _, _, cx| this.send(cx, Command::RefreshVoices)),
                        ),
                    ),
            )
            .child(hint(
                l.t(if demo {
                    Msg::TryVoiceDemo
                } else {
                    Msg::TryVoiceHint
                }),
                p,
            ))
            .child(subheading(l.t(Msg::Language), p))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(12.))
                    .child(segmented(
                        languages.into_iter().map(|(code, label)| {
                            segment(code, label, c.language == code, p)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.edit(cx, |c| c.tts.language = code.into())
                                }))
                                .into_any_element()
                        }),
                        p,
                    ))
                    .child(div().w(px(110.)).child(self.fields.tts_language.clone())),
            )
            .child(hint(l.t(Msg::TtsLanguageHint), p))
            .child(subheading(l.t(Msg::Style), p))
            .child(
                div().flex().child(
                    segmented(
                        TtsStyle::ALL.into_iter().map(|style| {
                            segment(
                                ("tts-style", style as usize),
                                style_label(style, l),
                                c.style == style,
                                p,
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.edit(cx, |c| c.tts.style = style)
                            }))
                            .into_any_element()
                        }),
                        p,
                    )
                    .flex_wrap()
                    .flex_shrink(1.)
                    .min_w(px(0.)),
                ),
            )
            .child(hint(l.t(Msg::StyleTagsHint), p))
    }

    fn reading_generation(&self, p: Palette, cx: &mut Context<Self>) -> Div {
        let l = lang(cx);
        let c = self.hub.read(cx).snap.config.tts.clone();
        let speed = c.generation_speed;
        group(l.t(Msg::Generation), p)
            .child(setting_row(
                l.t(Msg::GenerationSpeed),
                stepper(
                    "tts-speed",
                    fmt_speed(speed, l),
                    speed > GENERATION_SPEED_MIN + 0.001,
                    speed < GENERATION_SPEED_MAX - 0.001,
                    cx.listener(move |this, _, _, cx| {
                        this.edit(cx, |c| c.tts.generation_speed = step_speed(speed, -0.05))
                    }),
                    cx.listener(move |this, _, _, cx| {
                        this.edit(cx, |c| c.tts.generation_speed = step_speed(speed, 0.05))
                    }),
                    p,
                ),
            ))
            .child(hint(l.t(Msg::GenerationSpeedHint), p))
            .child(setting_row(
                l.t(Msg::AudioQuality),
                segmented(
                    [24_000u32, 48_000].into_iter().map(|rate| {
                        let label = if rate == 24_000 { "24 kHz" } else { "48 kHz" };
                        segment(label, label, c.sample_rate == rate, p)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.edit(cx, |c| c.tts.sample_rate = rate)
                            }))
                            .into_any_element()
                    }),
                    p,
                ),
            ))
            .child(
                radio_row(
                    "tts-latency",
                    l.t(Msg::LowLatency),
                    Some(l.t(Msg::LowLatencyNote).into()),
                    c.low_latency,
                    p,
                )
                .on_click(
                    cx.listener(|this, _, _, cx| this.edit(cx, |c| c.tts.low_latency = true)),
                ),
            )
            .child(
                radio_row(
                    "tts-quality",
                    l.t(Msg::BetterQuality),
                    Some(l.t(Msg::BetterQualityNote).into()),
                    !c.low_latency,
                    p,
                )
                .on_click(
                    cx.listener(|this, _, _, cx| this.edit(cx, |c| c.tts.low_latency = false)),
                ),
            )
            .child(
                toggle_row(
                    "tts-normalization",
                    l.t(Msg::Normalize),
                    Some(l.t(Msg::NormalizeNote)),
                    c.text_normalization,
                    p,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.edit(cx, |c| c.tts.text_normalization = !c.tts.text_normalization)
                })),
            )
    }

    fn reading_playback(&self, p: Palette, cx: &mut Context<Self>) -> Div {
        let l = lang(cx);
        let snap = &self.hub.read(cx).snap;
        let c = snap.config.tts.clone();
        let outputs = snap.tts.outputs.clone();
        let rate = c.playback_rate;
        let volume = c.volume;
        let missing = c
            .output_device
            .clone()
            .filter(|id| !outputs.iter().any(|d| &d.id == id));
        let lookahead = c.lookahead_seconds;
        group(l.t(Msg::Playback), p)
            .child(setting_row(
                l.t(Msg::InitialSpeed),
                stepper(
                    "tts-rate",
                    fmt_rate(rate, l),
                    rate > RATE_MIN,
                    rate < RATE_MAX,
                    cx.listener(move |this, _, _, cx| {
                        this.edit(cx, |c| c.tts.playback_rate = step_rate(rate, -0.05))
                    }),
                    cx.listener(move |this, _, _, cx| {
                        this.edit(cx, |c| c.tts.playback_rate = step_rate(rate, 0.05))
                    }),
                    p,
                ),
            ))
            .child(
                div().flex().child(
                    segmented(
                        RATE_PRESETS.map(|r| {
                            let label = fmt_rate(r, l);
                            segment(
                                ("tts-rate-preset", (r * 100.) as usize),
                                label,
                                (rate - r).abs() < 0.001,
                                p,
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.edit(cx, |c| c.tts.playback_rate = r)
                            }))
                            .into_any_element()
                        }),
                        p,
                    )
                    .flex_wrap()
                    .flex_shrink(1.)
                    .min_w(px(0.)),
                ),
            )
            .child(
                toggle_row(
                    "tts-remember-rate",
                    l.t(Msg::RememberSpeed),
                    Some(l.t(Msg::RememberSpeedNote)),
                    c.remember_rate,
                    p,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.edit(cx, |c| c.tts.remember_rate = !c.tts.remember_rate)
                })),
            )
            .child(hint(l.t(Msg::PlaybackSpeedHint), p))
            .child(setting_row(
                l.t(Msg::Volume),
                stepper(
                    "tts-volume",
                    tf!(l, PercentValue, v = (volume * 100.).round() as i32),
                    volume > 0.,
                    volume < 1.,
                    cx.listener(move |this, _, _, cx| {
                        this.edit(cx, |c| {
                            c.tts.volume = (((volume - 0.05) * 20.).round() / 20.).clamp(0., 1.)
                        })
                    }),
                    cx.listener(move |this, _, _, cx| {
                        this.edit(cx, |c| {
                            c.tts.volume = (((volume + 0.05) * 20.).round() / 20.).clamp(0., 1.)
                        })
                    }),
                    p,
                ),
            ))
            .child(subheading(l.t(Msg::AudioOutput), p))
            .child(
                radio_row(
                    "tts-output-default",
                    l.t(Msg::SystemDefaultOutput),
                    Some(l.t(Msg::SystemDefaultNote).into()),
                    c.output_device.is_none(),
                    p,
                )
                .on_click(
                    cx.listener(|this, _, _, cx| this.edit(cx, |c| c.tts.output_device = None)),
                ),
            )
            .children(outputs.into_iter().enumerate().map(|(i, d)| {
                let id = d.id.clone();
                let selected = c.output_device.as_deref() == Some(d.id.as_str());
                radio_row(("tts-output", i), d.name, Some(d.id.into()), selected, p).on_click(
                    cx.listener(move |this, _, _, cx| {
                        let id = id.clone();
                        this.edit(cx, |c| c.tts.output_device = Some(id))
                    }),
                )
            }))
            .when_some(missing, |d, id| {
                d.child(radio_row(
                    "tts-output-missing",
                    l.t(Msg::OutputMissing),
                    Some(tf!(l, OutputMissingNote, id = id).into()),
                    true,
                    p,
                ))
            })
            .child(subheading(l.t(Msg::WhileGenerating), p))
            .child(
                toggle_row(
                    "tts-autoplay",
                    l.t(Msg::Autoplay),
                    Some(l.t(Msg::AutoplayNote)),
                    c.autoplay,
                    p,
                )
                .on_click(
                    cx.listener(|this, _, _, cx| {
                        this.edit(cx, |c| c.tts.autoplay = !c.tts.autoplay)
                    }),
                ),
            )
            .children(Lookahead::ALL.into_iter().map(|a| {
                radio_row(
                    ("tts-lookahead-mode", a as usize),
                    lookahead_label(a, l),
                    Some(lookahead_detail(a, l).into()),
                    c.lookahead == a,
                    p,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.edit(cx, |c| c.tts.lookahead = a)))
            }))
            .when(c.lookahead == Lookahead::Window, |d| {
                d.child(setting_row(
                    l.t(Msg::Ahead),
                    stepper(
                        "tts-lookahead",
                        fmt_window(lookahead),
                        lookahead > 30,
                        lookahead < 900,
                        cx.listener(move |this, _, _, cx| {
                            this.edit(cx, |c| {
                                c.tts.lookahead_seconds = lookahead.saturating_sub(30).max(30)
                            })
                        }),
                        cx.listener(move |this, _, _, cx| {
                            this.edit(cx, |c| c.tts.lookahead_seconds = (lookahead + 30).min(900))
                        }),
                        p,
                    ),
                ))
            })
            .child(subheading(l.t(Msg::Controls), p))
            .child(setting_row(
                l.t(Msg::SkipButtons),
                segmented(
                    [5u8, 10, 15].into_iter().map(|s| {
                        let label = format!("{s} s");
                        segment(("tts-skip", s as usize), label, c.skip_seconds == s, p)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.edit(cx, |c| c.tts.skip_seconds = s)
                            }))
                            .into_any_element()
                    }),
                    p,
                ),
            ))
            .child(setting_row(
                l.t(Msg::OnDictationStart),
                segmented(
                    OnDictation::ALL.into_iter().map(|o| {
                        segment(
                            ("tts-on-dictation", o as usize),
                            on_dictation_label(o, l),
                            c.on_dictation == o,
                            p,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.edit(cx, |c| c.tts.on_dictation = o)
                        }))
                        .into_any_element()
                    }),
                    p,
                ),
            ))
            .child(hint(l.t(Msg::NeverOpensMic), p))
    }

    fn reading_text(&self, p: Palette, cx: &mut Context<Self>) -> Div {
        let l = lang(cx);
        let c = self.hub.read(cx).snap.config.tts.clone();
        let count = c.pronunciations.len();
        type Flag = fn(&mut dicta_core::TtsConfig) -> &mut bool;
        let toggles: [(&'static str, &'static str, Option<&'static str>, bool, Flag); 6] = [
            (
                "tts-hyphen",
                l.t(Msg::JoinHyphenated),
                None,
                c.join_hyphenated,
                |t| &mut t.join_hyphenated,
            ),
            (
                "tts-lines",
                l.t(Msg::JoinLines),
                Some(l.t(Msg::JoinLinesNote)),
                c.join_lines,
                |t| &mut t.join_lines,
            ),
            (
                "tts-urls",
                l.t(Msg::SkipLinks),
                Some(l.t(Msg::SkipLinksNote)),
                c.skip_urls,
                |t| &mut t.skip_urls,
            ),
            (
                "tts-code",
                l.t(Msg::SkipCode),
                Some(l.t(Msg::SkipCodeNote)),
                c.skip_code,
                |t| &mut t.skip_code,
            ),
            ("tts-emoji", l.t(Msg::ReadEmoji), None, c.read_emoji, |t| {
                &mut t.read_emoji
            }),
            (
                "tts-tags",
                l.t(Msg::InterpretTags),
                Some(l.t(Msg::InterpretTagsNote)),
                c.interpret_tags,
                |t| &mut t.interpret_tags,
            ),
        ];
        group(l.t(Msg::TextAndPronunciation), p)
            .children(toggles.into_iter().map(|(id, title, detail, on, flag)| {
                toggle_row(id, title, detail, on, p).on_click(cx.listener(move |this, _, _, cx| {
                    this.edit(cx, |c| {
                        let v = flag(&mut c.tts);
                        *v = !*v;
                    })
                }))
            }))
            .child(subheading(
                tf!(l, PronunciationCount, n = count, max = MAX_PRONUNCIATIONS),
                p,
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .child(self.fields.tts_pron_from.clone()),
                    )
                    .child(div().text_color(p.text_2).child("→"))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .child(self.fields.tts_pron_to.clone()),
                    )
                    .child(
                        button("tts-add-pron", l.t(Msg::Add), Kind::Secondary, p)
                            .on_click(cx.listener(|this, _, _, cx| this.add_pronunciation(cx))),
                    ),
            )
            .child(hint(l.t(Msg::PronunciationHint), p))
            .children(c.pronunciations.into_iter().enumerate().map(|(i, pr)| {
                let from = pr.from.clone();
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .py(px(4.))
                    .border_b_1()
                    .border_color(p.line)
                    .child(div().flex_1().min_w(px(0.)).truncate().child(pr.from))
                    .child(div().text_color(p.text_2).child("→"))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .font_weight(FontWeight::MEDIUM)
                            .child(pr.to),
                    )
                    .child(
                        icon_button(("tts-rm-pron", i), Icon::Close, p).on_click(cx.listener(
                            move |this, _, _, cx| {
                                let from = from.clone();
                                this.edit(cx, |c| c.tts.pronunciations.retain(|x| x.from != from))
                            },
                        )),
                    )
            }))
    }

    fn reading_window(&self, p: Palette, _width: f32, cx: &mut Context<Self>) -> Div {
        let l = lang(cx);
        let hyprland = ui_state(cx).session.is_hyprland();
        let c = self.hub.read(cx).snap.config.tts.clone();
        let center = ((self.display_w - READER_WIDTH) / 2.).round().max(0.);
        let presets: [(Msg, f32, f32); 4] = [
            (Msg::TopLeft, 24., 24.),
            (Msg::TopRight, -24., 24.),
            (Msg::BottomCentre, center, -48.),
            (Msg::BottomRight, -24., -48.),
        ];
        let (x, y, centered) = (c.window_x, c.window_y, c.window_centered);
        let closes = [0u16, 5, 15, 60];
        group(l.t(Msg::Window), p)
            .child(hint(l.t(Msg::ReaderDragHint), p))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(6.))
                    .child(
                        segment("tts-center", l.t(Msg::Centre), centered, p)
                            .border_1()
                            .border_color(if centered { p.control } else { p.line })
                            .when(centered, |d| d.bg(p.selected))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.edit(cx, |c| c.tts.window_centered = true)
                            })),
                    )
                    .children(
                        presets
                            .into_iter()
                            .enumerate()
                            .map(|(i, (label, px_, py_))| {
                                let selected =
                                    !centered && (x - px_).abs() < 0.5 && (y - py_).abs() < 0.5;
                                segment(("tts-pos", i), l.t(label), selected, p)
                                    .border_1()
                                    .border_color(if selected { p.control } else { p.line })
                                    .when(selected, |d| d.bg(p.selected))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.edit(cx, |c| {
                                            c.tts.window_centered = false;
                                            c.tts.window_x = px_;
                                            c.tts.window_y = py_;
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
                        &self.fields.tts_x,
                        p,
                    )))
                    .child(div().w(px(140.)).child(labelled(
                        l.t(Msg::Vertical),
                        &self.fields.tts_y,
                        p,
                    ))),
            )
            .child(hint(l.t(Msg::ReaderOffsetsHint), p))
            .child(
                toggle_row(
                    "tts-compact",
                    l.t(Msg::CompactMode),
                    Some(l.t(Msg::CompactModeNote)),
                    c.compact,
                    p,
                )
                .on_click(
                    cx.listener(|this, _, _, cx| this.edit(cx, |c| c.tts.compact = !c.tts.compact)),
                ),
            )
            .child(setting_row(
                l.t(Msg::CloseWhenDone),
                segmented(
                    closes.into_iter().map(|s| {
                        let label = if s == 0 {
                            l.t(Msg::Never).to_string()
                        } else {
                            tf!(l, AfterSeconds, s = s)
                        };
                        segment(
                            ("tts-close", s as usize),
                            label,
                            c.auto_close_seconds == s,
                            p,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.edit(cx, |c| c.tts.auto_close_seconds = s)
                        }))
                        .into_any_element()
                    }),
                    p,
                ),
            ))
            .child(hint(l.t(Msg::DesktopMayPlace), p))
            .children(session_hint(
                hyprland,
                tf!(l, HyprReaderRules, class = READER_APP_ID),
                p,
            ))
    }

    fn reading_history(&self, p: Palette, cx: &mut Context<Self>) -> Div {
        let l = lang(cx);
        let c = self.hub.read(cx).snap.config.tts.clone();
        let enabled = c.history_enabled;
        let options = [
            (AudioRetention::Off, Msg::ReadingRetentionOffNote),
            (AudioRetention::Days7, Msg::Retention7Note),
            (AudioRetention::Days30, Msg::Retention30Note),
            (AudioRetention::Forever, Msg::ReadingRetentionForeverNote),
        ];
        group(l.t(Msg::HistoryAndCost), p)
            .child(
                toggle_row(
                    "tts-history",
                    l.t(Msg::KeepReadings),
                    Some(l.t(Msg::KeepReadingsNote)),
                    enabled,
                    p,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.edit(cx, |c| c.tts.history_enabled = !c.tts.history_enabled)
                })),
            )
            .child(subheading(l.t(Msg::ReadingAudio), p))
            .children(options.into_iter().enumerate().map(|(i, (r, desc))| {
                let row = radio_row(
                    ("tts-retention", i),
                    retention_label(r, l),
                    Some(l.t(desc).into()),
                    r == c.audio_retention,
                    p,
                );
                if enabled {
                    row.on_click(cx.listener(move |this, _, _, cx| {
                        this.edit(cx, |c| c.tts.audio_retention = r)
                    }))
                } else {
                    disabled(row)
                }
            }))
            .when(!enabled, |d| {
                d.child(hint(l.t(Msg::ReadingAudioNeedsHistory), p))
            })
            .child(
                toggle_row(
                    "tts-cost",
                    l.t(Msg::ShowCost),
                    Some(l.t(Msg::ShowCostNote)),
                    c.show_cost,
                    p,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.edit(cx, |c| c.tts.show_cost = !c.tts.show_cost)
                })),
            )
            .child(hint(
                tf!(
                    l,
                    TtsPriceHint,
                    chars = format::count(100_000, l),
                    cost = format::usd(dicta_core::tts::estimate_usd(100_000), l)
                ),
                p,
            ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn character_counts_accept_thousands_separators() {
        let l = Lang::Es;
        assert_eq!(parse_chars("20.000", l), Ok(20_000));
        assert_eq!(parse_chars("20,000", Lang::En), Ok(20_000));
        assert_eq!(parse_chars(" 5 000 ", l), Ok(5_000));
        assert_eq!(parse_chars("0", l), Ok(0));
        assert!(parse_chars("mucho", l).is_err());
        assert!(parse_chars("-3", l).is_err());
        // What the field shows parses back in both languages.
        for l in Lang::ALL {
            assert_eq!(parse_chars(&format::count(20_000, l), l), Ok(20_000));
        }
    }

    #[test]
    fn terminal_classes_are_trimmed_unique_and_validated() {
        let listed = vec!["org.wezfurlong.wezterm".to_string()];
        let l = Lang::Es;
        assert_eq!(
            new_terminal_class("  com.raggesilver.BlackBox ", &listed, l),
            Ok("com.raggesilver.BlackBox".into())
        );
        assert!(new_terminal_class("ORG.wezfurlong.WEZTERM", &listed, l).is_err());
        assert!(new_terminal_class("   ", &listed, Lang::En).is_err());
        let mut c = dicta_core::AppConfig::default();
        c.tts.terminal_classes = vec!["x".repeat(101)];
        assert!(c.validate().is_err(), "length is refused by validate");
        c.tts.terminal_classes = (0..51).map(|i| format!("t{i}")).collect();
        assert!(c.validate().is_err(), "at most 50 classes");
    }

    #[test]
    fn every_source_explains_what_it_needs() {
        assert_eq!(TtsSource::ALL.len(), 4);
        assert_eq!(
            dicta_core::TtsConfig::default().source,
            TtsSource::CopySelection
        );
        for l in Lang::ALL {
            let linux = source_detail(TtsSource::CopySelection, l, Session::Wayland);
            assert!(linux.contains("Ctrl+Shift+C"), "{linux}");
            let mac = source_detail(TtsSource::CopySelection, l, Session::MacOs);
            assert!(mac.contains("Cmd+C") && !mac.contains("Shift"), "{mac}");
            for source in [TtsSource::SelectionThenClipboard, TtsSource::Selection] {
                // Neutral copy everywhere; the Hyprland setting only there.
                assert!(!source_detail(source, l, Session::Hyprland).contains("Hyprland"));
                let hint = source_session_hint(source, l, Session::Hyprland).unwrap();
                assert!(hint.contains("misc:middle_click_paste"));
                assert!(source_session_hint(source, l, Session::Wayland).is_none());
            }
            assert!(source_session_hint(TtsSource::Clipboard, l, Session::Hyprland).is_none());
        }
    }

    #[test]
    fn steppers_stay_inside_the_validated_ranges() {
        assert_eq!(step_speed(1.0, 0.05), 1.05);
        assert_eq!(step_speed(GENERATION_SPEED_MAX, 0.05), GENERATION_SPEED_MAX);
        assert_eq!(
            step_speed(GENERATION_SPEED_MIN, -0.05),
            GENERATION_SPEED_MIN
        );
        assert_eq!(fmt_speed(1.0, Lang::Es), "1,00");
        assert_eq!(fmt_speed(1.0, Lang::En), "1.00");
        assert_eq!(fmt_window(120), "2 min");
        assert_eq!(fmt_window(90), "1 min 30 s");
        assert_eq!(fmt_window(30), "30 s");
        let mut c = dicta_core::AppConfig::default();
        c.tts.generation_speed = step_speed(GENERATION_SPEED_MAX, 0.05);
        c.tts.playback_rate = step_rate(RATE_MAX, 0.05);
        c.validate().unwrap();
    }

    #[test]
    fn invalid_pronunciations_are_refused_before_sending() {
        let mut c = dicta_core::AppConfig::default();
        c.tts.pronunciations.push(Pronunciation {
            from: "Alumni".into(),
            to: "Álumni".into(),
        });
        c.validate().unwrap();
        c.tts.pronunciations.push(Pronunciation {
            from: "alumni".into(),
            to: "x".into(),
        });
        assert!(c.validate().is_err(), "duplicates are caught by validate");
    }
}
