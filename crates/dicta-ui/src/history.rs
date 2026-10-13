//! Historial: search, a virtualised list of notes and the detail of the
//! selected one (text, original, result, audio). The controller resolves
//! every id against its store; the UI never sees file paths. Deleting asks
//! for confirmation in place.

use dicta_core::{Command, HistoryKind, Note, NoteStatus, ProviderMode, Reading};
use gpui::{AnyElement, Context, Div, FontWeight, SharedString, div, prelude::*, px, uniform_list};

use crate::{
    assets::{DISPLAY_FONT, Icon},
    errors::ErrorOrigin,
    format,
    home::{note_meta, token_pair},
    i18n::{Lang, Msg, tf},
    lang,
    settings::{CONTENT_PAD_X, Confirm, Section, Settings, error_block, hint},
    theme::Palette,
    widgets::{Kind, button, confirm_bar, icon, icon_label_button, segment, segmented},
};

const LIST_W: f32 = 340.;
/// Narrowest detail pane that keeps the actions and the meta readable
/// beside the list.
const DETAIL_MIN_W: f32 = 420.;
/// Every row has the same height so the list can be virtualised.
const ROW_H: f32 = 86.;

/// How Historial lays out in a pane `width` px wide (sidebar excluded).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HistoryLayout {
    /// List and detail side by side.
    Split,
    /// Only the list; choosing a note opens the detail.
    List,
    /// Only the detail, with a way back to the list.
    Detail,
}

impl HistoryLayout {
    pub(crate) fn for_width(width: f32, detail_open: bool, has_selection: bool) -> Self {
        if width >= LIST_W + 1. + DETAIL_MIN_W {
            Self::Split
        } else if detail_open && has_selection {
            Self::Detail
        } else {
            Self::List
        }
    }
}

pub(crate) fn provider_label(mode: ProviderMode, l: Lang) -> &'static str {
    l.t(match mode {
        ProviderMode::Subscription => Msg::ProviderSubscription,
        ProviderMode::ApiKey => Msg::ProviderApi,
    })
}

/// What the empty list says, by cause.
pub(crate) fn empty_message(query: &str, history_on: bool, l: Lang) -> String {
    match (query.trim(), history_on) {
        ("", true) => l.t(Msg::HistoryEmpty).into(),
        ("", false) => l.t(Msg::HistoryOff).into(),
        (q, _) => tf!(l, NoNoteMatches, q = q),
    }
}

/// What the empty readings list says, by cause.
pub(crate) fn empty_readings_message(
    query: &str,
    history_on: bool,
    shortcut: &str,
    l: Lang,
) -> String {
    match (query.trim(), history_on) {
        ("", true) => tf!(l, ReadingsEmpty, key = shortcut),
        ("", false) => l.t(Msg::ReadingsOff).into(),
        (q, _) => tf!(l, NoReadingMatches, q = q),
    }
}

/// "Carina · 1.240 car. · 1:32" for a reading row.
pub(crate) fn reading_meta(r: &Reading, l: Lang) -> String {
    let mut meta = tf!(
        l,
        ReadingMetaChars,
        voice = crate::reader::voice_name(&[], &r.voice, l),
        n = format::count(r.chars, l)
    );
    if r.duration_ms > 0 {
        meta.push_str(&format!(" · {}", format::clock(r.duration_ms)));
    }
    if r.audio_filename.is_some() {
        meta.push_str(" · audio");
    }
    meta
}

impl Settings {
    /// `width` is the pane beside the sidebar. Wide windows show list and
    /// detail together; narrow ones show one at a time.
    pub(crate) fn history(&self, p: Palette, width: f32, cx: &mut Context<Self>) -> Div {
        let snap = &self.hub.read(cx).snap;
        let selected = match snap.history_kind {
            HistoryKind::Dictations => snap.selected_note.is_some(),
            HistoryKind::Readings => snap.selected_reading.is_some(),
        };
        let pane = div().flex_1().min_h(px(0.)).min_w(px(0.)).flex();
        match HistoryLayout::for_width(width, self.history_detail_open, selected) {
            HistoryLayout::Split => pane
                .child(self.history_list(p, true, cx))
                .child(self.history_detail(p, false, cx)),
            HistoryLayout::List => pane.child(self.history_list(p, false, cx)),
            HistoryLayout::Detail => pane.child(self.history_detail(p, true, cx)),
        }
    }

    fn history_list(&self, p: Palette, split: bool, cx: &mut Context<Self>) -> Div {
        let l = lang(cx);
        let snap = &self.hub.read(cx).snap;
        let kind = snap.history_kind;
        let readings = kind == HistoryKind::Readings;
        let (count, more) = if readings {
            (snap.readings.len(), snap.readings_has_more)
        } else {
            (snap.notes.len(), snap.notes_has_more)
        };
        let empty = (count == 0).then(|| {
            if readings {
                empty_readings_message(
                    &snap.history_query,
                    snap.config.tts.history_enabled,
                    &snap.config.tts.read_shortcut,
                    l,
                )
            } else {
                empty_message(&snap.history_query, snap.config.history_enabled, l)
            }
        });
        let clear_confirm = if readings {
            Confirm::ClearReadings
        } else {
            Confirm::ClearHistory
        };
        let confirming = self.confirm == Some(clear_confirm);
        let items = count + usize::from(more);
        let switch = segmented(
            [
                (HistoryKind::Dictations, Msg::Dictations),
                (HistoryKind::Readings, Msg::Readings),
            ]
            .into_iter()
            .enumerate()
            .map(|(i, (k, label))| {
                segment(("history-kind", i), l.t(label), k == kind, p)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if this.hub.read(cx).snap.history_kind != k {
                            this.confirm = None;
                            this.history_detail_open = false;
                            this.send(cx, Command::SetHistoryKind(k));
                        }
                    }))
                    .into_any_element()
            }),
            p,
        );
        let list = if readings {
            uniform_list(
                "readings",
                items,
                cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                    let snap = &this.hub.read(cx).snap;
                    let active = snap
                        .selected_reading
                        .as_ref()
                        .map(|r| r.id)
                        .or(snap.active_reading_id);
                    let rows: Vec<(usize, Option<Reading>)> =
                        range.map(|i| (i, snap.readings.get(i).cloned())).collect();
                    rows.into_iter()
                        .map(|(i, reading)| match reading {
                            Some(r) => {
                                let selected = active == Some(r.id);
                                this.reading_row(i, r, selected, p, cx)
                            }
                            None => load_more_row(p, cx),
                        })
                        .collect()
                }),
            )
            .flex_1()
            .px(px(8.))
            .into_any_element()
        } else {
            self.notes_list(items, p, cx)
        };
        let noun = |n: usize, more: bool| {
            let count = format::count(n as u64, l);
            match (readings, n, more) {
                (false, 1, false) => l.t(Msg::NotesOne).to_string(),
                (false, _, false) => tf!(l, NotesMany, n = count),
                (false, _, true) => tf!(l, NotesMore, n = count),
                (true, 1, false) => l.t(Msg::ReadingsOne).to_string(),
                (true, _, false) => tf!(l, ReadingsMany, n = count),
                (true, _, true) => tf!(l, ReadingsMore, n = count),
            }
        };

        div()
            .when(split, |d| {
                d.flex_none()
                    .w(px(LIST_W))
                    .border_r_1()
                    .border_color(p.line)
            })
            .when(!split, |d| d.flex_1().min_w(px(0.)))
            .h_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .px(px(16.))
                    .pt(px(14.))
                    .pb(px(10.))
                    .child(div().flex().child(switch))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(icon(Icon::Search, 16., p.text_2))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .child(self.fields.search.clone()),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .h(px(28.))
                            .child(
                                div()
                                    .flex_1()
                                    .text_size(px(12.5))
                                    .text_color(p.text_2)
                                    .child(noun(count, more)),
                            )
                            .when(count > 0 && !confirming, |d| {
                                d.child(
                                    icon_label_button(
                                        "clear",
                                        Icon::Trash,
                                        l.t(Msg::DeleteAll),
                                        Kind::Ghost,
                                        p,
                                    )
                                    .h(px(28.))
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.confirm = Some(clear_confirm);
                                            cx.notify();
                                        },
                                    )),
                                )
                            }),
                    )
                    .when(confirming, |d| {
                        d.child(confirm_bar(
                            l.t(if readings {
                                Msg::DeleteAllReadingsQuestion
                            } else {
                                Msg::DeleteAllNotesQuestion
                            }),
                            l.t(Msg::DeleteAll),
                            cx.listener(move |this, _, _, cx| {
                                this.confirm = None;
                                this.send(
                                    cx,
                                    if readings {
                                        Command::ClearReadings
                                    } else {
                                        Command::ClearHistory
                                    },
                                );
                                cx.notify();
                            }),
                            cx.listener(|this, _, _, cx| {
                                this.confirm = None;
                                cx.notify();
                            }),
                            l,
                            p,
                        ))
                    }),
            )
            .child(match empty {
                Some(message) => div()
                    .flex_1()
                    .px(px(16.))
                    .pt(px(8.))
                    .child(hint(message, p))
                    .into_any_element(),
                None => list,
            })
    }

    fn notes_list(&self, items: usize, p: Palette, cx: &mut Context<Self>) -> AnyElement {
        uniform_list(
            "notes",
            items,
            cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                let snap = &this.hub.read(cx).snap;
                let active = snap
                    .selected_note
                    .as_ref()
                    .map(|n| n.id)
                    .or(snap.active_note_id);
                let rows: Vec<(usize, Option<Note>)> =
                    range.map(|i| (i, snap.notes.get(i).cloned())).collect();
                rows.into_iter()
                    .map(|(i, note)| match note {
                        Some(n) => {
                            let selected = active == Some(n.id);
                            this.note_row(i, n, selected, p, cx)
                        }
                        None => load_more_row(p, cx),
                    })
                    .collect()
            }),
        )
        .flex_1()
        .px(px(8.))
        .into_any_element()
    }

    fn note_row(
        &self,
        i: usize,
        n: Note,
        selected: bool,
        p: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let l = lang(cx);
        let id = n.id;
        let failed = n.status == NoteStatus::Failed;
        div()
            .id(("note", i))
            .h(px(ROW_H))
            .py(px(2.))
            .child(
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .px(px(10.))
                    .py(px(8.))
                    .rounded(px(10.))
                    .cursor_pointer()
                    .when(selected, |d| {
                        d.bg(p.selected).border_1().border_color(p.line)
                    })
                    .when(!selected, move |d| d.hover(move |s| s.bg(p.hover())))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .text_size(px(12.))
                            .text_color(p.text_2)
                            .when(failed, |d| {
                                d.child(div().flex_none().size(px(7.)).rounded_full().bg(p.danger))
                            })
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .truncate()
                                    .child(format::note_date_local(n.created_at_ms, l)),
                            )
                            .child(div().flex_none().child(note_meta(&n, l))),
                    )
                    .child(
                        div()
                            .min_w(px(0.))
                            .text_size(px(13.5))
                            .line_height(px(19.))
                            .line_clamp(2)
                            .text_ellipsis()
                            .when(selected, |d| d.font_weight(FontWeight::MEDIUM))
                            .child(format::preview(&n.text, l)),
                    ),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.confirm = None;
                this.history_detail_open = true;
                this.send(cx, Command::OpenNote(id));
                cx.notify();
            }))
            .into_any_element()
    }

    /// `alone`: the detail fills the pane and offers a way back to the list.
    fn history_detail(&self, p: Palette, alone: bool, cx: &mut Context<Self>) -> AnyElement {
        if self.hub.read(cx).snap.history_kind == HistoryKind::Readings {
            return self.reading_detail(p, alone, cx);
        }
        let l = lang(cx);
        let snap = &self.hub.read(cx).snap;
        let Some(n) = snap.selected_note.clone() else {
            return div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .p(px(CONTENT_PAD_X))
                .child(hint(
                    l.t(if snap.notes.is_empty() {
                        Msg::PickNoteWhenAny
                    } else {
                        Msg::PickNote
                    }),
                    p,
                ))
                .into_any_element();
        };
        let id = n.id;
        let playing = snap.audio_playing_note_id == Some(id);
        let failed = n.status == NoteStatus::Failed;
        let edited = n.original.trim() != n.text.trim() && !n.original.trim().is_empty();
        let confirm = self.confirm;
        let tokens = token_pair(n.input_tokens, n.output_tokens, l);

        let meta = |label: &'static str, value: String| {
            // Natural width, so a cell moves to the next line whole
            // instead of breaking "Suscripción de Grok" in two.
            div()
                .flex_none()
                .flex()
                .flex_col()
                .gap(px(1.))
                .min_w(px(110.))
                .child(div().text_size(px(11.5)).text_color(p.text_2).child(label))
                .child(div().text_size(px(13.)).child(value))
        };

        let actions = if confirm == Some(Confirm::DeleteNote(id)) {
            confirm_bar(
                l.t(Msg::DeleteNoteQuestion),
                l.t(Msg::DeleteNote),
                cx.listener(move |this, _, _, cx| {
                    this.confirm = None;
                    this.send(cx, Command::DeleteNote(id));
                    cx.notify();
                }),
                cx.listener(|this, _, _, cx| {
                    this.confirm = None;
                    cx.notify();
                }),
                l,
                p,
            )
        } else {
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(8.))
                .child(
                    icon_label_button(
                        "copy-note",
                        Icon::Copy,
                        l.t(Msg::CopyText),
                        Kind::Secondary,
                        p,
                    )
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.send(cx, Command::CopyNote(id))),
                    ),
                )
                .when(!n.text.trim().is_empty(), |d| {
                    let text = n.text.clone();
                    d.child(
                        icon_label_button(
                            "read-note",
                            Icon::Read,
                            l.t(Msg::ReadNoteAloud),
                            Kind::Secondary,
                            p,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.send(cx, Command::ReadText(text.clone()))
                        })),
                    )
                })
                .child(
                    icon_label_button(
                        "delete-note",
                        Icon::Trash,
                        l.t(Msg::DeleteNote),
                        Kind::Ghost,
                        p,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.confirm = Some(Confirm::DeleteNote(id));
                        cx.notify();
                    })),
                )
        };

        let audio = match &n.audio_filename {
            Some(_) if confirm == Some(Confirm::DeleteAudio(id)) => confirm_bar(
                l.t(Msg::DeleteAudioQuestion),
                l.t(Msg::DeleteAudio),
                cx.listener(move |this, _, _, cx| {
                    this.confirm = None;
                    this.send(cx, Command::DeleteNoteAudio(id));
                    cx.notify();
                }),
                cx.listener(|this, _, _, cx| {
                    this.confirm = None;
                    cx.notify();
                }),
                l,
                p,
            ),
            Some(_) => div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(10.))
                .child(if playing {
                    icon_label_button("stop-audio", Icon::Stop, l.t(Msg::Stop), Kind::Secondary, p)
                        .on_click(
                            cx.listener(|this, _, _, cx| this.send(cx, Command::StopAudioPlayback)),
                        )
                } else {
                    icon_label_button("play-audio", Icon::Play, l.t(Msg::Play), Kind::Secondary, p)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.send(cx, Command::PlayNoteAudio(id))
                        }))
                })
                .child(
                    div()
                        .flex_1()
                        .min_w(px(120.))
                        .text_size(px(12.5))
                        .text_color(p.text_2)
                        .child(format!(
                            "{} · {}{}",
                            format::clock(n.captured_ms),
                            format::bytes(n.audio_bytes, l),
                            if playing { l.t(Msg::PlayingSuffix) } else { "" }
                        )),
                )
                .child(
                    button("delete-audio", l.t(Msg::DeleteAudio), Kind::Ghost, p).on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.confirm = Some(Confirm::DeleteAudio(id));
                            cx.notify();
                        }),
                    ),
                ),
            None => div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(8.))
                .child(hint(l.t(Msg::NoNoteAudio), p))
                .when(!snap.config.saves_audio(), |d| {
                    d.child(
                        button(
                            "audio-settings",
                            l.t(Msg::KeepNextNotesAudio),
                            Kind::Ghost,
                            p,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.go(Section::Data, cx))),
                    )
                }),
        };

        let back = alone.then(|| {
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(8.))
                .px(px(CONTENT_PAD_X - 10.))
                .py(px(8.))
                .border_b_1()
                .border_color(p.line)
                .child(
                    icon_label_button("history-back", Icon::Back, l.t(Msg::Notes), Kind::Ghost, p)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.confirm = None;
                            this.history_detail_open = false;
                            cx.notify();
                        })),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .truncate()
                        .text_size(px(12.5))
                        .text_color(p.text_2)
                        .child(note_position(&snap.notes, id, l)),
                )
        });
        let body = div()
            .id("note-detail")
            .flex_1()
            .min_w(px(0.))
            .min_h(px(0.))
            .overflow_y_scroll()
            .px(px(CONTENT_PAD_X))
            .py(px(18.))
            .child(
                div()
                    .max_w(px(680.))
                    .flex()
                    .flex_col()
                    .gap(px(14.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .font_family(DISPLAY_FONT)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_size(px(17.))
                                    .child(format::note_date_local(n.created_at_ms, l)),
                            )
                            .child(status_chip(failed, l, p)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(px(16.))
                            .child(meta(l.t(Msg::MetaWords), format::count(n.word_count, l)))
                            .child(meta(
                                l.t(Msg::MetaDictated),
                                format::duration(n.captured_ms),
                            ))
                            .child(meta(l.t(Msg::MetaSent), format::duration(n.sent_ms)))
                            .child(meta(
                                l.t(Msg::MetaProvider),
                                provider_label(n.provider, l).into(),
                            ))
                            .child(meta(l.t(Msg::PolishTokens), tokens)),
                    )
                    .when_some(n.error.clone().filter(|_| failed), |d, e| {
                        d.child(error_block(
                            l.t(Msg::DictationEndedError),
                            &ErrorOrigin::Dictation.body(&e),
                            p,
                        ))
                    })
                    .child(actions)
                    .child(text_block(None, &n.text, l, p))
                    .when(edited, |d| {
                        d.child(text_block(
                            Some(l.t(Msg::OriginalTranscript)),
                            &n.original,
                            l,
                            p,
                        ))
                    })
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(8.))
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
                                    .text_size(px(13.5))
                                    .child(icon(Icon::Audio, 15., p.text_2))
                                    .child(l.t(Msg::Audio)),
                            )
                            .child(audio),
                    ),
            );
        div()
            .flex_1()
            .min_w(px(0.))
            .flex()
            .flex_col()
            .children(back)
            .child(body)
            .into_any_element()
    }

    fn reading_row(
        &self,
        i: usize,
        r: Reading,
        selected: bool,
        p: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let l = lang(cx);
        let id = r.id;
        let failed = r.status == NoteStatus::Failed;
        div()
            .id(("reading", i))
            .h(px(ROW_H))
            .py(px(2.))
            .child(
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .px(px(10.))
                    .py(px(8.))
                    .rounded(px(10.))
                    .cursor_pointer()
                    .when(selected, |d| {
                        d.bg(p.selected).border_1().border_color(p.line)
                    })
                    .when(!selected, move |d| d.hover(move |s| s.bg(p.hover())))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .text_size(px(12.))
                            .text_color(p.text_2)
                            .when(failed, |d| {
                                d.child(div().flex_none().size(px(7.)).rounded_full().bg(p.danger))
                            })
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .truncate()
                                    .child(format::note_date_local(r.created_at_ms, l)),
                            )
                            .child(div().flex_none().child(reading_meta(&r, l))),
                    )
                    .child(
                        div()
                            .min_w(px(0.))
                            .text_size(px(13.5))
                            .line_height(px(19.))
                            .line_clamp(2)
                            .text_ellipsis()
                            .when(selected, |d| d.font_weight(FontWeight::MEDIUM))
                            .child(format::preview(&r.text, l)),
                    ),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.confirm = None;
                this.history_detail_open = true;
                this.send(cx, Command::OpenReading(id));
                cx.notify();
            }))
            .into_any_element()
    }

    fn reading_detail(&self, p: Palette, alone: bool, cx: &mut Context<Self>) -> AnyElement {
        let l = lang(cx);
        let snap = &self.hub.read(cx).snap;
        let Some(r) = snap.selected_reading.clone() else {
            return div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .p(px(CONTENT_PAD_X))
                .child(hint(
                    l.t(if snap.readings.is_empty() {
                        Msg::PickReadingWhenAny
                    } else {
                        Msg::PickReading
                    }),
                    p,
                ))
                .into_any_element();
        };
        let id = r.id;
        let failed = r.status == NoteStatus::Failed;
        let saved_audio = r.audio_filename.is_some();
        let confirm = self.confirm;
        let demo = snap.demo;
        let saves_audio = snap.config.tts.saves_audio();
        let position = reading_position(&snap.readings, id, l);
        let meta = |label: &'static str, value: String| {
            div()
                .flex_none()
                .flex()
                .flex_col()
                .gap(px(1.))
                .min_w(px(110.))
                .child(div().text_size(px(11.5)).text_color(p.text_2).child(label))
                .child(div().text_size(px(13.)).child(value))
        };
        let actions =
            if confirm == Some(Confirm::DeleteReading(id)) {
                confirm_bar(
                    l.t(Msg::DeleteReadingQuestion),
                    l.t(Msg::DeleteReading),
                    cx.listener(move |this, _, _, cx| {
                        this.confirm = None;
                        this.send(cx, Command::DeleteReading(id));
                        cx.notify();
                    }),
                    cx.listener(|this, _, _, cx| {
                        this.confirm = None;
                        cx.notify();
                    }),
                    l,
                    p,
                )
            } else {
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        icon_label_button(
                            "replay-reading",
                            Icon::Play,
                            l.t(Msg::Play),
                            Kind::Secondary,
                            p,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.send(cx, Command::ReplayReading(id))
                        })),
                    )
                    .child(
                        icon_label_button(
                            "copy-reading",
                            Icon::Copy,
                            l.t(Msg::CopyText),
                            Kind::Secondary,
                            p,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.send(cx, Command::CopyReading(id))
                        })),
                    )
                    .child(
                        icon_label_button(
                            "delete-reading",
                            Icon::Trash,
                            l.t(Msg::Delete),
                            Kind::Ghost,
                            p,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.confirm = Some(Confirm::DeleteReading(id));
                            cx.notify();
                        })),
                    )
            };
        let replay_hint = l.t(match (saved_audio, demo) {
            (true, _) => Msg::ReplaySaved,
            (false, true) => Msg::ReplayDemo,
            (false, false) => Msg::ReplayRegenerates,
        });
        let audio = match &r.audio_filename {
            Some(_) if confirm == Some(Confirm::DeleteReadingAudio(id)) => confirm_bar(
                l.t(Msg::DeleteAudioQuestion),
                l.t(Msg::DeleteAudio),
                cx.listener(move |this, _, _, cx| {
                    this.confirm = None;
                    this.send(cx, Command::DeleteReadingAudio(id));
                    cx.notify();
                }),
                cx.listener(|this, _, _, cx| {
                    this.confirm = None;
                    cx.notify();
                }),
                l,
                p,
            ),
            Some(_) => div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(10.))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(120.))
                        .text_size(px(12.5))
                        .text_color(p.text_2)
                        .child(tf!(
                            l,
                            SavedAudioMeta,
                            clock = format::clock(r.duration_ms),
                            size = format::bytes(r.audio_bytes, l)
                        )),
                )
                .child(
                    button(
                        "delete-reading-audio",
                        l.t(Msg::DeleteAudio),
                        Kind::Ghost,
                        p,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.confirm = Some(Confirm::DeleteReadingAudio(id));
                        cx.notify();
                    })),
                ),
            None => div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(8.))
                .child(hint(l.t(Msg::NoReadingAudio), p))
                .when(!saves_audio, |d| {
                    d.child(
                        button(
                            "reading-audio-settings",
                            l.t(Msg::KeepNextReadingsAudio),
                            Kind::Ghost,
                            p,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.go(Section::Reading, cx))),
                    )
                }),
        };
        let back = alone.then(|| {
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(8.))
                .px(px(CONTENT_PAD_X - 10.))
                .py(px(8.))
                .border_b_1()
                .border_color(p.line)
                .child(
                    icon_label_button(
                        "history-back",
                        Icon::Back,
                        l.t(Msg::Readings),
                        Kind::Ghost,
                        p,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.confirm = None;
                        this.history_detail_open = false;
                        cx.notify();
                    })),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .truncate()
                        .text_size(px(12.5))
                        .text_color(p.text_2)
                        .child(position.clone()),
                )
        });
        let body = div()
            .id("reading-detail")
            .flex_1()
            .min_w(px(0.))
            .min_h(px(0.))
            .overflow_y_scroll()
            .px(px(CONTENT_PAD_X))
            .py(px(18.))
            .child(
                div()
                    .max_w(px(680.))
                    .flex()
                    .flex_col()
                    .gap(px(14.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .font_family(DISPLAY_FONT)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_size(px(17.))
                                    .child(format::note_date_local(r.created_at_ms, l)),
                            )
                            .child(status_chip(failed, l, p)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(px(16.))
                            .child(meta(l.t(Msg::MetaCharacters), format::count(r.chars, l)))
                            .child(meta(
                                l.t(Msg::MetaSentChars),
                                format::count(r.chars_sent, l),
                            ))
                            .child(meta(
                                l.t(Msg::AudioGenerated),
                                format::duration(r.duration_ms),
                            ))
                            .child(meta(
                                l.t(Msg::Voice),
                                crate::reader::voice_name(&[], &r.voice, l),
                            ))
                            .child(meta(
                                l.t(Msg::Language),
                                if r.language == "auto" {
                                    l.t(Msg::Automatic).into()
                                } else {
                                    r.language.clone()
                                },
                            ))
                            .child(meta(
                                l.t(Msg::MetaAccount),
                                provider_label(r.provider, l).into(),
                            ))
                            .child(meta(
                                l.t(Msg::MetaEstimatedCost),
                                crate::reader::cost_label(
                                    dicta_core::tts::estimate_usd(r.chars_sent),
                                    l,
                                ),
                            )),
                    )
                    .when_some(r.error.clone().filter(|_| failed), |d, e| {
                        d.child(error_block(l.t(Msg::ReadingEndedError), &e, p))
                    })
                    .child(actions)
                    .child(hint(replay_hint, p))
                    .child(text_block(None, &r.text, l, p))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(8.))
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
                                    .text_size(px(13.5))
                                    .child(icon(Icon::Audio, 15., p.text_2))
                                    .child(l.t(Msg::Audio)),
                            )
                            .child(audio),
                    ),
            );
        div()
            .flex_1()
            .min_w(px(0.))
            .flex()
            .flex_col()
            .children(back)
            .child(body)
            .into_any_element()
    }
}

/// "Cargar más" at the end of a list that has more on the controller.
fn load_more_row(p: Palette, cx: &mut Context<Settings>) -> AnyElement {
    let l = lang(cx);
    div()
        .h(px(ROW_H))
        .flex()
        .items_center()
        .justify_center()
        .child(
            icon_label_button(
                "load-more",
                Icon::More,
                l.t(Msg::LoadMore),
                Kind::Secondary,
                p,
            )
            .on_click(cx.listener(|this, _, _, cx| this.send(cx, Command::LoadMoreNotes))),
        )
        .into_any_element()
}

/// "Lectura 2 de 6" in the loaded list.
pub(crate) fn reading_position(readings: &[Reading], id: i64, l: Lang) -> String {
    match readings.iter().position(|r| r.id == id) {
        Some(i) => tf!(
            l,
            ReadingPosition,
            i = format::count(i as u64 + 1, l),
            n = format::count(readings.len() as u64, l)
        ),
        None => l.t(Msg::ReadingOutsideSearch).into(),
    }
}

/// "Nota 2 de 6" in the loaded list; a note outside the current filter says
/// only that it is open.
pub(crate) fn note_position(notes: &[Note], id: i64, l: Lang) -> String {
    match notes.iter().position(|n| n.id == id) {
        Some(i) => tf!(
            l,
            NotePosition,
            i = format::count(i as u64 + 1, l),
            n = format::count(notes.len() as u64, l)
        ),
        None => l.t(Msg::NoteOutsideSearch).into(),
    }
}

fn status_chip(failed: bool, l: Lang, p: Palette) -> Div {
    let color = if failed { p.danger } else { p.ok };
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(6.))
        .px(px(9.))
        .h(px(24.))
        .rounded_full()
        .border_1()
        .border_color(color)
        .text_size(px(12.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(color)
        .child(div().size(px(7.)).rounded_full().bg(color))
        .child(l.t(if failed {
            Msg::WithError
        } else {
            Msg::Completed
        }))
}

/// Note text, wrapped and never truncated; the page scrolls.
fn text_block(heading: Option<&'static str>, text: &str, l: Lang, p: Palette) -> Div {
    let paragraphs: Vec<SharedString> = text
        .split('\n')
        .map(|l| SharedString::from(l.trim_end().to_string()))
        .collect();
    div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .when_some(heading, |d, h| {
            d.child(div().text_size(px(12.5)).text_color(p.text_2).child(h))
        })
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .p(px(14.))
                .rounded(px(12.))
                .bg(if heading.is_some() {
                    p.surface
                } else {
                    p.raised
                })
                .border_1()
                .border_color(p.line)
                .text_size(px(15.))
                .line_height(px(23.))
                .when(heading.is_some(), |d| d.text_color(p.text_2))
                .when(text.trim().is_empty(), |d| {
                    d.text_color(p.text_2).child(l.t(Msg::NoText))
                })
                .children(
                    paragraphs
                        .into_iter()
                        .filter(|l| !l.is_empty())
                        .map(|l| div().min_w(px(0.)).child(l)),
                ),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_readings_say_how_to_start_and_why() {
        let l = Lang::Es;
        assert!(empty_readings_message("", true, "Ctrl+Alt+Space", l).contains("Ctrl+Alt+Space"));
        assert!(empty_readings_message("", false, "x", l).contains("apagado"));
        assert_eq!(
            empty_readings_message(" marte ", true, "x", l),
            "Ninguna lectura contiene «marte»."
        );
        assert_eq!(
            empty_readings_message(" mars ", true, "x", Lang::En),
            "No reading contains “mars”."
        );
    }

    #[test]
    fn reading_rows_show_voice_size_and_audio() {
        let mut r = Reading {
            id: 3,
            created_at_ms: 0,
            text: "Hola".into(),
            chars: 1240,
            chars_sent: 1240,
            duration_ms: 92_000,
            voice: "carina".into(),
            language: "auto".into(),
            provider: ProviderMode::Subscription,
            status: NoteStatus::Completed,
            error: None,
            audio_filename: None,
            audio_bytes: 0,
        };
        let l = Lang::Es;
        assert_eq!(reading_meta(&r, l), "Carina · 1.240 car. · 1:32");
        assert_eq!(reading_meta(&r, Lang::En), "Carina · 1,240 chars · 1:32");
        r.audio_filename = Some("tts-1.wav".into());
        r.duration_ms = 0;
        assert_eq!(reading_meta(&r, l), "Carina · 1.240 car. · audio");
        assert_eq!(reading_position(&[r.clone()], 3, l), "Lectura 1 de 1");
        assert_eq!(
            reading_position(&[r.clone()], 3, Lang::En),
            "Reading 1 of 1"
        );
        assert!(reading_position(&[r], 9, l).contains("fuera"));
    }

    #[test]
    fn empty_history_explains_why() {
        let l = Lang::Es;
        assert!(empty_message("", true, l).contains("vacío"));
        assert!(empty_message("", false, l).contains("apagado"));
        assert_eq!(
            empty_message(" kernel ", true, l),
            "Ninguna nota contiene «kernel»."
        );
        assert!(empty_message("", false, Lang::En).contains("off"));
    }

    /// Pane width beside the sidebar for a window `w` px wide.
    fn pane(w: f32) -> f32 {
        w - crate::settings::NAV_W - 1.
    }

    #[test]
    fn narrow_windows_show_list_or_detail_and_wide_ones_both() {
        use HistoryLayout::*;
        for w in [720., 725., 880.] {
            assert_eq!(HistoryLayout::for_width(pane(w), false, true), List, "{w}");
            assert_eq!(HistoryLayout::for_width(pane(w), true, true), Detail, "{w}");
            // Nothing to show: back to the list even if the detail was open.
            assert_eq!(HistoryLayout::for_width(pane(w), true, false), List, "{w}");
        }
        for w in [1040., 1364.] {
            assert_eq!(HistoryLayout::for_width(pane(w), false, true), Split, "{w}");
            assert_eq!(HistoryLayout::for_width(pane(w), true, false), Split, "{w}");
        }
        // Split only when the detail keeps its readable width.
        assert!(pane(1040.) - LIST_W - 1. >= DETAIL_MIN_W);
    }
}
