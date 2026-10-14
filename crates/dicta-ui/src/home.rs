//! Inicio: what the app is ready to do, usage over a period and the latest
//! notes. Everything comes from the snapshot; the totals are computed by the
//! controller from the history the user kept, never invented here.

use dicta_core::{Command, Note, NoteStatus, UsagePeriod};
use gpui::{AnyElement, Context, Div, FontWeight, SharedString, div, img, prelude::*, px};

use crate::{
    assets::{self, DISPLAY_FONT, Icon},
    format,
    i18n::{Lang, Msg, tf},
    lang,
    overlay::phase_label,
    settings::{DASHBOARD_W, GROUP_PAD, Section, Settings, group, hint},
    theme::Palette,
    widgets::{Kind, button, demo_badge, icon, icon_label_button, kbd, segment, segmented},
};

pub(crate) fn period_label(period: UsagePeriod, l: Lang) -> &'static str {
    l.t(match period {
        UsagePeriod::Today => Msg::PeriodToday,
        UsagePeriod::Week => Msg::PeriodWeek,
        UsagePeriod::All => Msg::PeriodAll,
    })
}

/// Label and note of the cost tile. The amount is the public speech-to-text
/// list price for the audio sent, not a bill nor a subscription balance.
pub(crate) const COST_LABEL: Msg = Msg::SttCostLabel;
pub(crate) const COST_NOTE: Msg = Msg::SttCostNote;
/// Read aloud: the public text-to-speech price for the characters sent.
pub(crate) const TTS_COST_LABEL: Msg = Msg::TtsCostLabel;
pub(crate) const TTS_COST_NOTE: Msg = Msg::TtsCostNote;

/// Polish tokens as "entrada · salida"; unknown counts say so.
pub(crate) fn token_pair(input: Option<u64>, output: Option<u64>, l: Lang) -> String {
    match (input, output) {
        (None, None) => l.t(Msg::NotAvailable).into(),
        (i, o) => tf!(
            l,
            TokenPair,
            input = format::tokens(i, l),
            output = format::tokens(o, l)
        ),
    }
}

/// Compact tile value: "412 · 96", or "No disponible" when unknown.
pub(crate) fn token_value(input: Option<u64>, output: Option<u64>, l: Lang) -> String {
    match (input, output) {
        (None, None) => l.t(Msg::NotAvailable).into(),
        (i, o) => format!("{} · {}", format::tokens(i, l), format::tokens(o, l)),
    }
}

const RECENT: usize = 5;
const TILE_GAP: f32 = 12.;
const TILE_MIN_W: f32 = 180.;

fn tile(label: &'static str, value: String, note: Option<SharedString>, p: Palette) -> Div {
    div()
        .flex_1()
        .min_w(px(TILE_MIN_W))
        .min_h(px(108.))
        .flex()
        .flex_col()
        .gap(px(4.))
        .p(px(14.))
        .rounded(px(14.))
        .bg(p.raised)
        .border_1()
        .border_color(p.line)
        .child(div().text_size(px(12.5)).text_color(p.text_2).child(label))
        .child(
            div()
                .font_family(DISPLAY_FONT)
                .font_weight(FontWeight::SEMIBOLD)
                .text_size(px(22.))
                .line_height(px(28.))
                .child(value),
        )
        .when_some(note, |d, n| {
            d.child(
                div()
                    .text_size(px(12.))
                    .line_height(px(16.))
                    .text_color(p.text_2)
                    .child(n),
            )
        })
}

/// A one-line note row for Inicio. Selecting it opens Historial on it.
pub(crate) fn note_meta(note: &Note, l: Lang) -> String {
    let mut meta = format!(
        "{} · {}",
        format::words(note.word_count, l),
        format::clock(note.captured_ms)
    );
    if note.audio_filename.is_some() {
        meta.push_str(" · audio");
    }
    meta
}

/// Reading tiles per row: the four in one row when they fit, else two.
pub(crate) fn reading_tiles_per_row(width: f32) -> usize {
    let inner = width.min(DASHBOARD_W) - GROUP_PAD * 2. - 2.;
    if inner >= TILE_MIN_W * 4. + TILE_GAP * 3. {
        4
    } else {
        2
    }
}

/// Usage tiles per row in a content column `width` px wide: three when they
/// fit inside the group card, two otherwise.
pub(crate) fn tiles_per_row(width: f32) -> usize {
    let inner = width.min(DASHBOARD_W) - GROUP_PAD * 2. - 2.;
    if inner >= TILE_MIN_W * 3. + TILE_GAP * 2. {
        3
    } else {
        2
    }
}

impl Settings {
    pub(crate) fn home(&self, p: Palette, width: f32, cx: &mut Context<Self>) -> Div {
        let l = lang(cx);
        let snap = &self.hub.read(cx).snap;
        let ready = snap.authenticated || snap.demo;
        let demo = snap.demo;
        let busy = snap.phase.busy();
        let key = snap.config.toggle_shortcut.clone();
        let m = snap.metrics.clone();
        let period = snap.usage_period;
        let history_on = snap.config.history_enabled;
        let recent: Vec<Note> = snap.notes.iter().take(RECENT).cloned().collect();
        let rm = snap.reading_metrics.clone();
        let show_tts_cost = snap.config.tts.show_cost;
        let read_key = snap.config.tts.read_shortcut.clone();
        let write_key = snap.config.writer.shortcut.clone();

        let hero = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(16.))
            .mt(px(14.))
            .p(px(18.))
            .rounded(px(16.))
            .bg(p.raised)
            .border_1()
            .border_color(p.line)
            .child(
                img(assets::art(assets::mark(56., p.dark)))
                    .flex_none()
                    .size(px(56.)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(220.))
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .font_family(DISPLAY_FONT)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_size(px(18.))
                            .child(match (busy, ready) {
                                (true, _) => phase_label(snap.phase, l),
                                (false, true) => l.t(Msg::ReadyToDictate),
                                (false, false) => l.t(Msg::ConnectToDictate),
                            })
                            .when(demo, |d| d.child(demo_badge(p))),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap(px(6.))
                            .text_size(px(13.))
                            .text_color(p.text_2)
                            .child(kbd(key, p))
                            .child(l.t(match (ready, demo) {
                                (true, true) => Msg::HeroSimulated,
                                (true, false) => Msg::HeroConnected,
                                (false, _) => Msg::HeroNotConnected,
                            })),
                    ),
            )
            .child(if ready {
                icon_label_button(
                    "home-dictate",
                    Icon::Microphone,
                    l.t(Msg::Dictate),
                    Kind::Primary,
                    p,
                )
                .on_click(cx.listener(|this, _, _, cx| this.send(cx, Command::Start)))
                .into_any_element()
            } else {
                icon_label_button(
                    "home-connect",
                    Icon::Provider,
                    l.t(Msg::ConnectGrok),
                    Kind::Primary,
                    p,
                )
                .on_click(cx.listener(|this, _, _, cx| this.go(Section::Provider, cx)))
                .into_any_element()
            });

        let periods = segmented(
            [UsagePeriod::Today, UsagePeriod::Week, UsagePeriod::All]
                .into_iter()
                .enumerate()
                .map(|(i, it)| {
                    segment(("period", i), period_label(it, l), it == period, p)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.send(cx, Command::SetUsagePeriod(it))
                        }))
                        .into_any_element()
                }),
            p,
        );

        let per_row = tiles_per_row(width);
        let tiles = vec![
            tile(
                l.t(Msg::Dictations),
                format::count(m.dictations, l),
                Some(l.t(Msg::DictationsNote).into()),
                p,
            ),
            tile(l.t(Msg::Words), format::count(m.words, l), None, p),
            tile(
                l.t(Msg::TimeDictated),
                format::duration(m.captured_ms),
                Some(l.t(Msg::TimeDictatedNote).into()),
                p,
            ),
            tile(
                l.t(Msg::AudioSent),
                format::duration(m.sent_ms),
                Some(l.t(Msg::AudioSentNote).into()),
                p,
            ),
            tile(
                l.t(COST_LABEL),
                format::usd(m.estimated_stt_usd, l),
                Some(l.t(COST_NOTE).into()),
                p,
            ),
            tile(
                l.t(Msg::PolishTokens),
                token_value(m.input_tokens, m.output_tokens, l),
                Some(
                    l.t(if m.input_tokens.is_none() && m.output_tokens.is_none() {
                        Msg::NoTokensReported
                    } else {
                        Msg::TokensReported
                    })
                    .into(),
                ),
                p,
            ),
        ];
        let mut rows = div().flex().flex_col().gap(px(TILE_GAP));
        let mut it = tiles.into_iter().peekable();
        while it.peek().is_some() {
            rows = rows.child(
                div()
                    .flex()
                    .gap(px(TILE_GAP))
                    .children(it.by_ref().take(per_row)),
            );
        }

        let usage = group(l.t(Msg::Usage), p)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(10.))
                    .child(periods)
                    .child(div().flex_1())
                    .when(m.audio_bytes > 0, |d| {
                        d.child(hint(
                            tf!(l, AudioKept, size = format::bytes(m.audio_bytes, l)),
                            p,
                        ))
                    }),
            )
            .child(rows)
            .child(hint(l.t(Msg::UsageFromHistory), p));

        let mut reading_tiles = vec![
            tile(
                l.t(Msg::Readings),
                format::count(rm.readings, l),
                Some(l.t(Msg::ReadingsNote).into()),
                p,
            ),
            tile(
                l.t(Msg::CharsSent),
                format::count(rm.chars_sent, l),
                Some(l.t(Msg::CharsSentNote).into()),
                p,
            ),
            tile(
                l.t(Msg::AudioGenerated),
                format::duration(rm.duration_ms),
                None,
                p,
            ),
        ];
        if show_tts_cost {
            reading_tiles.push(tile(
                l.t(TTS_COST_LABEL),
                format::usd(rm.estimated_tts_usd, l),
                Some(l.t(TTS_COST_NOTE).into()),
                p,
            ));
        }
        let per_row = reading_tiles_per_row(width);
        let mut reading_rows = div().flex().flex_col().gap(px(TILE_GAP));
        let mut it = reading_tiles.into_iter().peekable();
        while it.peek().is_some() {
            reading_rows = reading_rows.child(
                div()
                    .flex()
                    .gap(px(TILE_GAP))
                    .children(it.by_ref().take(per_row)),
            );
        }
        let reading = group(l.t(Msg::ReadAloud), p)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(8.))
                    .when(!read_key.trim().is_empty(), |d| d.child(kbd(read_key, p)))
                    .child(
                        hint(tf!(l, ReadAloudPeriod, period = period_label(period, l)), p)
                            .flex_1()
                            .min_w(px(200.)),
                    )
                    .child(
                        icon_label_button(
                            "home-read-clipboard",
                            Icon::Paste,
                            l.t(Msg::ReadClipboard),
                            Kind::Ghost,
                            p,
                        )
                        .on_click(
                            cx.listener(|this, _, _, cx| this.send(cx, Command::ReadClipboard)),
                        ),
                    ),
            )
            .child(reading_rows)
            .child(hint(l.t(Msg::ReadingUsageFromHistory), p));

        // Escritura has no history yet: the entry point and its shortcut.
        let writer = group(l.t(Msg::SecWriter), p).child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(8.))
                .when(!write_key.trim().is_empty(), |d| d.child(kbd(write_key, p)))
                .child(hint(l.t(Msg::WriterHomeHint), p).flex_1().min_w(px(200.)))
                .child(
                    icon_label_button(
                        "home-writer-clipboard",
                        Icon::Paste,
                        l.t(Msg::WriterClipboard),
                        Kind::Ghost,
                        p,
                    )
                    .on_click(
                        cx.listener(|this, _, _, cx| this.send(cx, Command::WriterClipboard)),
                    ),
                ),
        );

        let mut list = group(l.t(Msg::RecentNotes), p);
        if !history_on {
            list = list.child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(10.))
                    .child(hint(l.t(Msg::HistoryOffHome), p).flex_1().min_w(px(220.)))
                    .child(
                        button("home-data", l.t(Msg::TurnHistoryOn), Kind::Secondary, p)
                            .on_click(cx.listener(|this, _, _, cx| this.go(Section::Data, cx))),
                    ),
            );
        }
        list = if recent.is_empty() {
            list.child(hint(l.t(Msg::NoNotesYet), p))
        } else {
            list.children(
                recent
                    .into_iter()
                    .enumerate()
                    .map(|(i, n)| self.recent_row(i, n, stacked_recent(width), p, cx)),
            )
            .child(
                div().flex().child(
                    icon_label_button(
                        "home-history",
                        Icon::History,
                        l.t(Msg::SeeHistory),
                        Kind::Ghost,
                        p,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.go(Section::History, cx))),
                ),
            )
        };

        div()
            .flex()
            .flex_col()
            .child(hero)
            .child(usage)
            .child(reading)
            .child(writer)
            .child(list)
    }

    /// `stacked`: narrow columns put the date and meta on a line above the
    /// preview, so the text keeps most of the width.
    fn recent_row(
        &self,
        i: usize,
        n: Note,
        stacked: bool,
        p: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let l = lang(cx);
        let id = n.id;
        let failed = n.status == NoteStatus::Failed;
        let date = div()
            .flex_none()
            .when(!stacked, |d| d.w(px(92.)))
            .text_size(px(12.5))
            .text_color(p.text_2)
            .child(format::note_date_local(n.created_at_ms, l));
        let error = failed.then(|| {
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(4.))
                .text_size(px(12.))
                .text_color(p.danger)
                .child(icon(Icon::Alert, 13., p.danger))
                .child(l.t(Msg::WithError))
        });
        let meta = div()
            .flex_none()
            .text_size(px(12.))
            .text_color(p.text_2)
            .child(note_meta(&n, l));
        let preview = div()
            .flex_1()
            .min_w(px(0.))
            .truncate()
            .child(format::preview(&n.text, l));
        let row = div()
            .id(("recent", i))
            .px(px(10.))
            .py(px(8.))
            .rounded(px(10.))
            .cursor_pointer()
            .hover(move |s| s.bg(p.hover()));
        let row = if stacked {
            row.flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .child(date)
                        .child(div().flex_1())
                        .children(error)
                        .child(meta),
                )
                .child(div().flex().child(preview))
        } else {
            row.flex()
                .items_center()
                .gap(px(12.))
                .child(date)
                .child(preview)
                .children(error)
                .child(meta)
        };
        row.on_click(cx.listener(move |this, _, _, cx| {
            this.send(cx, Command::OpenNote(id));
            this.go(Section::History, cx);
            this.history_detail_open = true;
        }))
        .into_any_element()
    }
}

/// Recent notes stack below this content width: date, preview and meta in
/// one line would leave the preview a few words.
const RECENT_INLINE_MIN_W: f32 = 600.;

pub(crate) fn stacked_recent(width: f32) -> bool {
    width.min(DASHBOARD_W) < RECENT_INLINE_MIN_W
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_tokens_read_as_unknown_never_as_zero() {
        let l = Lang::Es;
        assert_eq!(token_pair(None, None, l), "No disponible");
        assert_eq!(
            token_pair(Some(1200), None, l),
            "1.200 entrada · No disponible salida"
        );
        assert_eq!(token_pair(Some(0), Some(0), l), "0 entrada · 0 salida");
        assert_eq!(token_value(None, None, l), "No disponible");
        assert_eq!(token_value(Some(412), None, l), "412 · No disponible");
        assert_eq!(
            token_pair(Some(1200), None, Lang::En),
            "1,200 in · Not available out"
        );
    }

    /// Content column beside the sidebar for a window `w` px wide.
    fn column(w: f32) -> f32 {
        w - crate::settings::NAV_W - 1. - crate::settings::CONTENT_PAD_X * 2.
    }

    #[test]
    fn usage_tiles_and_recent_notes_fit_from_the_minimum_window() {
        let min = column(crate::MAIN_MIN_W);
        assert_eq!(tiles_per_row(min), 2);
        assert!(stacked_recent(min) && stacked_recent(column(725.)));
        // Two tiles at their minimum still fit inside the card.
        assert!(TILE_MIN_W * 2. + TILE_GAP <= min - GROUP_PAD * 2. - 2.);
        assert_eq!(tiles_per_row(column(1040.)), 3);
        assert_eq!(tiles_per_row(column(1364.)), 3);
        assert!(!stacked_recent(column(1040.)));
    }

    #[test]
    fn reading_tiles_fit_from_the_minimum_window() {
        assert_eq!(reading_tiles_per_row(column(crate::MAIN_MIN_W)), 2);
        assert_eq!(reading_tiles_per_row(column(1364.)), 4);
        let min = column(crate::MAIN_MIN_W);
        assert!(TILE_MIN_W * 2. + TILE_GAP <= min - GROUP_PAD * 2. - 2.);
    }

    #[test]
    fn the_cost_is_labelled_as_an_estimate() {
        let es = Lang::Es;
        assert!(es.t(TTS_COST_LABEL).contains("estimado"));
        assert!(es.t(TTS_COST_NOTE).contains("No es un") && es.t(TTS_COST_NOTE).contains("US$ 15"));
        assert!(es.t(COST_LABEL).contains("estimado"));
        assert!(es.t(COST_NOTE).contains("No es un cobro"));
        assert!(es.t(COST_NOTE).contains("0,20"));
        let en = Lang::En;
        for m in [COST_LABEL, TTS_COST_LABEL] {
            assert!(en.t(m).contains("Estimated"));
        }
        assert!(en.t(COST_NOTE).contains("Not a charge") && en.t(COST_NOTE).contains("0.20"));
        assert!(en.t(TTS_COST_NOTE).contains("Not a charge"));
    }
}
