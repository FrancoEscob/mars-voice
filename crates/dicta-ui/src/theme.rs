//! Appearance = style × colour mode.
//!
//! The three styles (Señal, Regolito, Órbita) only decide structure: how the
//! voice is drawn, corner radius, the grain and whether the planet peeks out
//! of the overlay card. Colours come from the mode alone: every light style
//! shares Señal's greens, every dark style shares the old Noche charcoal.
//! Legacy `Theme::Night` renders as Señal; `AppConfig::normalize` turns it
//! into Señal + dark when the configuration loads.
//! The UI never hardcodes colours outside this module.

use dicta_core::{ColorMode, Theme};
use gpui::{BoxShadow, Hsla, Rgba, point, px, rgb, rgba};

use crate::i18n::{Lang, Msg};

/// How the live voice is drawn inside the overlay.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VoiceStyle {
    /// Continuous wave (Señal).
    Wave,
    /// Columns of dots, the loudest ones accented in orange shade (Regolito).
    Dots,
    /// Mars with rings of regolith dust that swell with the voice (Órbita).
    Orbit,
}

#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub theme: Theme,
    pub mode: ColorMode,
    /// Dominant background of the main window.
    pub ground: Rgba,
    /// Overlay card and panels.
    pub surface: Rgba,
    /// Raised controls on top of a surface (inputs, segmented buttons).
    pub raised: Rgba,
    /// Selected row / pressed segment.
    pub selected: Rgba,
    pub line: Rgba,
    /// Outline of interactive controls (inputs, radios, switch tracks):
    /// at least 3:1 against `raised`, unlike the decorative `line`.
    pub control: Rgba,
    pub text: Rgba,
    pub text_2: Rgba,
    /// Words that the recogniser has not confirmed yet.
    pub tentative: Rgba,
    pub orange: Rgba,
    /// Accent for the loudest voice dots/dust and small orange text.
    pub orange_shade: Rgba,
    /// Text on top of orange, always ink (5.3:1), in every state.
    pub on_orange: Rgba,
    /// Hovered orange: lighter, so ink on it gains contrast (6.2:1).
    pub orange_hover: Rgba,
    /// Pressed orange: a touch darker, ink still 4.6:1.
    pub orange_pressed: Rgba,
    pub voice: Rgba,
    pub voice_style: VoiceStyle,
    pub danger: Rgba,
    /// Positive state (connected, completed).
    pub ok: Rgba,
    pub demo_bg: Rgba,
    pub demo_text: Rgba,
    /// Read aloud: the sentence being read, a soft marker behind the text.
    pub mark_sentence: Rgba,
    /// Read aloud: the word being read, a pale planet tint behind the ink.
    pub mark_word: Rgba,
    pub shadow: Hsla,
    pub radius: f32,
    /// Regolito lets the big grainy planet peek out of the card corner.
    pub planet_corner: bool,
    /// Pale-ink marks and grain (dark mode).
    pub dark: bool,
    /// Coarser, two-layer grain (Regolito) instead of the fine one.
    pub coarse_grain: bool,
}

/// What a style decides, independent of colour.
fn structure(theme: Theme) -> (VoiceStyle, f32, bool, bool) {
    match theme {
        Theme::Regolith => (VoiceStyle::Dots, 16., true, true),
        Theme::Orbit => (VoiceStyle::Orbit, 18., false, false),
        // Night is legacy: it renders as Señal.
        Theme::Signal | Theme::Night => (VoiceStyle::Wave, 18., false, false),
    }
}

impl Palette {
    pub fn new(theme: Theme, mode: ColorMode) -> Self {
        // A legacy Night that was not normalised still means dark.
        let mode = if theme == Theme::Night {
            ColorMode::Dark
        } else {
            mode
        };
        let theme = if theme == Theme::Night {
            Theme::Signal
        } else {
            theme
        };
        let (voice_style, radius, planet_corner, coarse_grain) = structure(theme);
        let base = Self {
            theme,
            mode,
            ground: rgb(0xD6E5CB),
            surface: rgb(0xEEF4E6),
            raised: rgb(0xF7FAF3),
            selected: rgb(0xDCE8D2),
            line: rgb(0xB9CCAE),
            control: rgb(0x7F9274),
            text: rgb(0x24271E),
            text_2: rgb(0x515D47),
            tentative: rgb(0x5E685A),
            orange: rgb(0xFF4F00),
            orange_shade: rgb(0xB23A00),
            on_orange: rgb(0x21170F),
            orange_hover: rgb(0xFF6A26),
            orange_pressed: rgb(0xEB4900),
            voice: rgb(0x24271E),
            voice_style,
            danger: rgb(0x9E2A10),
            ok: rgb(0x3C6A2E),
            demo_bg: rgb(0x24271E),
            demo_text: rgb(0xEEF4E6),
            mark_sentence: rgb(0xDAE7CF),
            mark_word: rgb(0xFFC7A6),
            shadow: gpui::hsla(100. / 360., 0.2, 0.15, 0.22),
            radius,
            planet_corner,
            dark: false,
            coarse_grain,
        };
        match mode {
            ColorMode::Light => base,
            ColorMode::Dark => Self {
                ground: rgb(0x14110E),
                surface: rgb(0x211C18),
                raised: rgb(0x2A241F),
                selected: rgb(0x352E28),
                line: rgb(0x463D35),
                control: rgb(0x7A6E62),
                text: rgb(0xE8EEE2),
                text_2: rgb(0xA6B59E),
                tentative: rgb(0x909C8A),
                orange_shade: rgb(0xFF8A57),
                voice: rgb(0xCFE3C4),
                danger: rgb(0xFF8A66),
                ok: rgb(0xA8D49A),
                demo_bg: rgb(0xCFE3C4),
                demo_text: rgb(0x14110E),
                mark_sentence: rgb(0x342C26),
                mark_word: rgb(0x6E2F12),
                shadow: gpui::hsla(0., 0., 0., 0.5),
                dark: true,
                ..base
            },
        }
    }

    /// Faint fill used for hovers, derived so all styles stay coherent.
    pub fn hover(&self) -> Rgba {
        let mut c = self.text;
        c.a = 0.07;
        c
    }

    /// Soft 3 px ring around a focused control, on top of its ink border.
    pub fn focus_halo(&self) -> BoxShadow {
        let color = if self.dark {
            rgba(0xCFE3C440)
        } else {
            rgba(0x24271E2E)
        };
        BoxShadow {
            color: color.into(),
            offset: point(px(0.), px(0.)),
            blur_radius: px(0.),
            spread_radius: px(3.),
            inset: false,
        }
    }
}

/// Name of a style in the interface language; `Theme::label` is Spanish.
pub fn theme_name(theme: Theme, l: Lang) -> &'static str {
    l.t(match theme {
        Theme::Signal | Theme::Night => Msg::ThemeSignal,
        Theme::Regolith => Msg::ThemeRegolith,
        Theme::Orbit => Msg::ThemeOrbit,
    })
}

/// Short description shown under each style in the picker.
pub fn theme_blurb(theme: Theme, l: Lang) -> &'static str {
    l.t(match theme {
        Theme::Signal | Theme::Night => Msg::BlurbSignal,
        Theme::Regolith => Msg::BlurbRegolith,
        Theme::Orbit => Msg::BlurbOrbit,
    })
}

pub fn mode_label(mode: ColorMode, l: Lang) -> &'static str {
    l.t(match mode {
        ColorMode::Light => Msg::ModeLight,
        ColorMode::Dark => Msg::ModeDark,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn luminance(c: Rgba) -> f32 {
        let lin = |v: f32| {
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b)
    }

    fn contrast(a: Rgba, b: Rgba) -> f32 {
        let (x, y) = (luminance(a), luminance(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    const MODES: [ColorMode; 2] = [ColorMode::Light, ColorMode::Dark];

    #[test]
    fn all_six_combinations_are_readable() {
        for theme in Theme::ALL {
            for mode in MODES {
                let p = Palette::new(theme, mode);
                let tag = format!("{theme:?}/{mode:?}");
                for bg in [p.ground, p.surface, p.raised, p.selected] {
                    assert!(contrast(p.text, bg) >= 7., "{tag} text");
                    assert!(contrast(p.text_2, bg) >= 4.5, "{tag} text_2");
                    assert!(contrast(p.danger, bg) >= 4.5, "{tag} danger");
                }
                assert!(contrast(p.tentative, p.surface) >= 4.5, "{tag} tentative");
                assert!(contrast(p.control, p.raised) >= 3., "{tag} control");
                assert!(contrast(p.voice, p.surface) >= 4.5, "{tag} voice");
                assert!(contrast(p.orange_shade, p.surface) >= 3., "{tag} accent");
                assert!(contrast(p.ok, p.surface) >= 3., "{tag} ok");
                assert!(contrast(p.demo_text, p.demo_bg) >= 7., "{tag} demo");
                // The read-aloud markers sit behind body text.
                for mark in [p.mark_sentence, p.mark_word] {
                    assert!(contrast(p.text, mark) >= 7., "{tag} text on reading mark");
                }
                assert!(
                    contrast(p.mark_word, p.surface) > contrast(p.mark_sentence, p.surface),
                    "{tag} the word stands out from its sentence"
                );
                for o in [p.orange, p.orange_hover, p.orange_pressed] {
                    assert!(contrast(p.on_orange, o) >= 4.5, "{tag} on orange");
                }
            }
        }
    }

    #[test]
    fn colours_follow_the_mode_and_structure_follows_the_style() {
        for mode in MODES {
            let s = Palette::new(Theme::Signal, mode);
            for theme in Theme::ALL {
                let p = Palette::new(theme, mode);
                assert_eq!(p.surface, s.surface, "{theme:?} shares {mode:?} colours");
                assert_eq!(p.text, s.text);
                assert_eq!(p.dark, mode == ColorMode::Dark);
            }
        }
        let styles: Vec<VoiceStyle> = Theme::ALL
            .into_iter()
            .map(|t| Palette::new(t, ColorMode::Light).voice_style)
            .collect();
        assert_eq!(
            styles,
            [VoiceStyle::Wave, VoiceStyle::Dots, VoiceStyle::Orbit]
        );
        assert!(Palette::new(Theme::Regolith, ColorMode::Dark).coarse_grain);
    }

    #[test]
    fn legacy_night_renders_as_signal_dark() {
        let night = Palette::new(Theme::Night, ColorMode::Light);
        let signal_dark = Palette::new(Theme::Signal, ColorMode::Dark);
        assert_eq!(night.theme, Theme::Signal);
        assert_eq!(night.mode, ColorMode::Dark);
        assert_eq!(night.surface, signal_dark.surface);
        assert_eq!(night.voice_style, VoiceStyle::Wave);
        assert!(!Theme::ALL.contains(&Theme::Night), "no Noche card");
    }
}
