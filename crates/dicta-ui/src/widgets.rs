//! Small building blocks shared by the overlay and settings, styled from the
//! active [`Palette`] so the three styles stay consistent.

use std::sync::Arc;

use gpui::{
    AnyElement, App, BoxShadow, ClickEvent, Div, ElementId, FontWeight, Image, Rgba, SharedString,
    Stateful, Svg, Window, div, img, point, prelude::*, px, svg,
};

use crate::{
    assets::{self, GRAIN_TILE, Icon, MONO_FONT},
    i18n::{Lang, Msg},
    perf,
    theme::Palette,
};

/// One of Dicta's icons, tinted with `color`.
pub fn icon(icon: Icon, size: f32, color: Rgba) -> Svg {
    svg()
        .path(icon.path())
        .flex_none()
        .size(px(size))
        .text_color(color)
}

/// The grain tile of a palette.
fn grain_tile(p: Palette) -> Arc<Image> {
    match (p.dark, p.coarse_grain) {
        (true, true) => assets::GRAIN_COARSE_NIGHT.clone(),
        (true, false) => assets::GRAIN_NIGHT.clone(),
        (false, true) => assets::GRAIN_COARSE.clone(),
        (false, false) => assets::GRAIN_FINE.clone(),
    }
}

/// Rasterises the art of the overlay card for `p` in the background: its
/// grain, Mars (the microphone and the centre of Órbita) and, for Regolito,
/// the planet in the corner. See [`assets::prewarm`].
pub fn prewarm_grain(p: Palette, cx: &mut App) {
    let mut art = vec![
        grain_tile(p),
        assets::MARS.clone(),
        assets::MARS_SMALL.clone(),
    ];
    if p.planet_corner {
        art.push(assets::PLANET_CORNER.clone());
    }
    assets::prewarm(art, cx);
}

/// Whether the grain of `p` is already rasterised (harness).
#[cfg_attr(not(feature = "screenshots"), allow(dead_code))]
pub fn grain_ready(p: Palette) -> bool {
    assets::bitmap_ready(&grain_tile(p))
}

/// Grain covering a `w`×`h` area, meant as the first child of a `relative`
/// container so everything after it paints on top. Tiles are seamless, and
/// the container's own `overflow_hidden` (if any) trims the last row.
pub fn grain(p: Palette, w: f32, h: f32) -> Div {
    let tile = assets::art(grain_tile(p));
    let cols = (w / GRAIN_TILE).ceil().max(1.) as usize;
    let rows = (h / GRAIN_TILE).ceil().max(1.) as usize;
    perf::add(&perf::GRAIN_TILES, (cols * rows) as u64);
    div()
        .absolute()
        .top_0()
        .left_0()
        .w(px(w))
        .h(px(h))
        .overflow_hidden()
        .flex()
        .flex_col()
        .children((0..rows).map(move |_| {
            let tile = tile.clone();
            div().flex().flex_none().children(
                (0..cols).map(move |_| img(tile.clone()).flex_none().size(px(GRAIN_TILE))),
            )
        }))
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Orange. Reserved for Terminar and the main action of a panel.
    Primary,
    Secondary,
    Ghost,
    /// Confirms a destructive action; only shown after a first click.
    Danger,
}

pub fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    kind: Kind,
    p: Palette,
) -> Stateful<Div> {
    button_base(id, kind, p).child(label.into())
}

fn button_base(id: impl Into<ElementId>, kind: Kind, p: Palette) -> Stateful<Div> {
    let base = div()
        .id(id)
        .font_family(assets::UI_FONT)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .gap(px(6.))
        .h(px(32.))
        .px(px(13.))
        .rounded(px(9.))
        .text_size(px(13.5))
        .font_weight(FontWeight::SEMIBOLD)
        .cursor_pointer();
    match kind {
        // Ink on orange in every state: hover lightens, press darkens a
        // touch, so the label never drops under 4.5:1.
        Kind::Primary => base
            .bg(p.orange)
            .text_color(p.on_orange)
            .hover(move |s| s.bg(p.orange_hover))
            .active(move |s| s.bg(p.orange_pressed)),
        Kind::Secondary => base
            .bg(p.raised)
            .border_1()
            .border_color(p.control)
            .text_color(p.text)
            .hover(move |s| s.bg(p.selected).border_color(p.text_2))
            .active(move |s| s.bg(p.line)),
        Kind::Ghost => base
            .text_color(p.text_2)
            .hover(move |s| s.bg(p.hover()).text_color(p.text))
            .active(move |s| s.bg(p.selected).text_color(p.text)),
        Kind::Danger => base
            .bg(p.danger)
            .text_color(p.surface)
            .hover(move |s| s.opacity(0.9))
            .active(move |s| s.opacity(0.8)),
    }
}

/// Disabled look without handlers; callers simply skip `on_click`.
pub fn disabled(button: Stateful<Div>) -> Stateful<Div> {
    button.opacity(0.45).cursor_default()
}

/// A button with a leading icon tinted like its label.
pub fn icon_label_button(
    id: impl Into<ElementId>,
    glyph: Icon,
    label: impl Into<SharedString>,
    kind: Kind,
    p: Palette,
) -> Stateful<Div> {
    let color = match kind {
        Kind::Primary => p.on_orange,
        Kind::Secondary => p.text,
        Kind::Ghost => p.text_2,
        Kind::Danger => p.surface,
    };
    button_base(id, kind, p)
        .pl(px(10.))
        .child(icon(glyph, 15., color))
        .child(label.into())
}

/// Square icon-only button (close, dismiss).
pub fn icon_button(id: impl Into<ElementId>, glyph: Icon, p: Palette) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(28.))
        .rounded(px(8.))
        .text_color(p.text_2)
        .cursor_pointer()
        .hover(move |s| s.bg(p.hover()).text_color(p.text))
        .active(move |s| s.bg(p.selected).text_color(p.text))
        .child(icon(glyph, 15., p.text_2))
}

/// Small rounded count shown next to a nav entry or heading.
pub fn count_chip(text: impl Into<SharedString>, p: Palette) -> Div {
    div()
        .flex_none()
        .min_w(px(20.))
        .h(px(18.))
        .px(px(6.))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(p.selected)
        .text_size(px(11.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(p.text_2)
        .child(text.into())
}

pub fn demo_badge(p: Palette) -> Div {
    div()
        .flex_none()
        .px(px(6.))
        .py(px(1.))
        .rounded(px(5.))
        .bg(p.demo_bg)
        .text_color(p.demo_text)
        .text_size(px(10.5))
        .font_weight(FontWeight::BOLD)
        .child("DEMO")
}

pub fn kbd(text: impl Into<SharedString>, p: Palette) -> Div {
    div()
        .flex_none()
        .px(px(6.))
        .py(px(1.))
        .rounded(px(5.))
        .border_1()
        .border_color(p.line)
        .bg(p.raised)
        .font_family(MONO_FONT)
        .text_size(px(11.5))
        .text_color(p.text_2)
        .child(text.into())
}

pub fn switch(id: impl Into<ElementId>, on: bool, p: Palette) -> Stateful<Div> {
    let knob = div()
        .size(px(16.))
        .rounded_full()
        // Off: a raised knob on light styles, a pale one on Noche, where the
        // raised colour would vanish into the dark track.
        .bg(if on {
            p.on_orange
        } else if p.dark {
            p.text_2
        } else {
            p.raised
        })
        .shadow(vec![BoxShadow {
            color: gpui::hsla(0., 0., 0., 0.25),
            offset: point(px(0.), px(1.)),
            blur_radius: px(2.),
            spread_radius: px(0.),
            inset: false,
        }]);
    div()
        .id(id)
        .flex_none()
        .flex()
        .items_center()
        .w(px(38.))
        .h(px(22.))
        .px(px(3.))
        .rounded_full()
        .cursor_pointer()
        .bg(if on { p.orange } else { p.line })
        .when(!on, |d| d.border_1().border_color(p.control))
        .when(on, |d| d.justify_end())
        .child(knob)
}

/// A labelled toggle row: title, optional description and a switch.
pub fn toggle_row(
    id: impl Into<ElementId>,
    title: impl Into<SharedString>,
    description: Option<&'static str>,
    on: bool,
    p: Palette,
) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap(px(14.))
        .py(px(8.))
        .cursor_pointer()
        .child(
            div()
                .flex_1()
                // Lets long descriptions wrap instead of widening the row.
                .min_w(px(0.))
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(div().font_weight(FontWeight::MEDIUM).child(title.into()))
                .when_some(description, |d, text| {
                    d.child(div().text_size(px(12.5)).text_color(p.text_2).child(text))
                }),
        )
        .child(switch("knob", on, p))
}

pub fn radio_row(
    id: impl Into<ElementId>,
    title: impl Into<SharedString>,
    description: Option<SharedString>,
    selected: bool,
    p: Palette,
) -> Stateful<Div> {
    radio_row_with(id, title, None, description, selected, p)
}

/// [`radio_row`] with a small badge after the title ("Recomendada").
pub fn radio_row_with(
    id: impl Into<ElementId>,
    title: impl Into<SharedString>,
    badge: Option<Div>,
    description: Option<SharedString>,
    selected: bool,
    p: Palette,
) -> Stateful<Div> {
    let dot = div()
        .flex_none()
        .mt(px(2.))
        .size(px(18.))
        .rounded_full()
        .border_2()
        .border_color(if selected { p.orange } else { p.control })
        .bg(p.raised)
        .flex()
        .items_center()
        .justify_center()
        .when(selected, |d| {
            d.child(div().size(px(8.)).rounded_full().bg(p.orange))
        });
    div()
        .id(id)
        .flex()
        .gap(px(10.))
        .py(px(7.))
        .px(px(8.))
        .rounded(px(9.))
        .cursor_pointer()
        .hover(move |s| s.bg(p.hover()))
        .active(move |s| s.bg(p.selected))
        .child(dot)
        .child(
            div()
                .flex_1()
                // Lets long descriptions wrap instead of widening the row.
                .min_w(px(0.))
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(8.))
                        .child(div().font_weight(FontWeight::MEDIUM).child(title.into()))
                        .children(badge),
                )
                .when_some(description, |d, text| {
                    d.child(div().text_size(px(12.5)).text_color(p.text_2).child(text))
                }),
        )
}

/// Small outlined tag: "Recomendada", "Experimental". `accent` uses the
/// planet shade for attention without the alarm of danger red.
pub fn badge(text: impl Into<SharedString>, accent: bool, p: Palette) -> Div {
    let color = if accent { p.orange_shade } else { p.text_2 };
    div()
        .flex_none()
        .px(px(6.))
        .h(px(18.))
        .flex()
        .items_center()
        .rounded(px(5.))
        .border_1()
        .border_color(color)
        .text_size(px(11.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(color)
        .child(text.into())
}

pub fn segmented(children: impl IntoIterator<Item = AnyElement>, p: Palette) -> Div {
    div()
        .flex()
        .flex_none()
        .p(px(3.))
        .gap(px(2.))
        .rounded(px(10.))
        .bg(p.selected)
        .border_1()
        .border_color(p.line)
        .children(children)
}

pub fn segment(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    selected: bool,
    p: Palette,
) -> Stateful<Div> {
    div()
        .id(id)
        .px(px(11.))
        .py(px(4.))
        .rounded(px(7.))
        .text_size(px(13.))
        .font_weight(FontWeight::MEDIUM)
        .cursor_pointer()
        .text_color(if selected { p.text } else { p.text_2 })
        .when(selected, |d| d.bg(p.raised).border_1().border_color(p.line))
        .when(!selected, move |d| {
            d.hover(move |s| s.bg(p.hover()).text_color(p.text))
        })
        .child(label.into())
}

/// In-place confirmation for a destructive action: the question, the
/// destructive button and Cancelar, inside a danger-edged strip.
pub fn confirm_bar(
    question: impl Into<SharedString>,
    action: impl Into<SharedString>,
    on_confirm: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    on_cancel: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    l: Lang,
    p: Palette,
) -> Div {
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(8.))
        .p(px(8.))
        .pl(px(12.))
        .rounded(px(10.))
        .bg(p.raised)
        .border_1()
        .border_color(p.danger)
        .child(
            div()
                .flex_1()
                .min_w(px(160.))
                .text_size(px(13.))
                .font_weight(FontWeight::MEDIUM)
                .child(question.into()),
        )
        .child(button("confirm-yes", action, Kind::Danger, p).on_click(on_confirm))
        .child(button("confirm-no", l.t(Msg::Cancel), Kind::Ghost, p).on_click(on_cancel))
}

pub fn format_elapsed(ms: u64) -> String {
    let s = ms / 1000;
    format!("{}:{:02}", s / 60, s % 60)
}

#[cfg(test)]
mod tests {
    #[test]
    fn elapsed_is_minutes_and_seconds() {
        assert_eq!(super::format_elapsed(0), "0:00");
        assert_eq!(super::format_elapsed(65_400), "1:05");
    }
}
