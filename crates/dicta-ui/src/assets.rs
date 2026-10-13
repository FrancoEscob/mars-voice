//! Bundled fonts, brand marks, icons and grain. Everything is compiled into
//! the binary, so the UI never reads files at runtime and works offline.
//!
//! Two kinds of vector art:
//! - Colour art (planet, marks, grain) is drawn with `img()` from bytes.
//! - Icons are single-colour line drawings served by [`Assets`] and drawn
//!   with `svg()`, which tints them with the active palette.

use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    sync::{Arc, LazyLock, Mutex},
};

use gpui::{
    App, AppContext as _, AssetSource, Image, ImageFormat, ImageSource, RenderImage, SharedString,
};

/// Body text: Archivo at normal width, comfortable for long paragraphs.
pub const UI_FONT: &str = "Archivo";
/// Identity: page titles, the wordmark and the overlay title.
pub const DISPLAY_FONT: &str = "Archivo SemiExpanded";
pub const MONO_FONT: &str = "IBM Plex Mono";

const FONTS: [&[u8]; 9] = [
    include_bytes!("../assets/fonts/Archivo-Regular.ttf"),
    include_bytes!("../assets/fonts/Archivo-Medium.ttf"),
    include_bytes!("../assets/fonts/Archivo-SemiBold.ttf"),
    include_bytes!("../assets/fonts/ArchivoSemiExpanded-Regular.ttf"),
    include_bytes!("../assets/fonts/ArchivoSemiExpanded-Medium.ttf"),
    include_bytes!("../assets/fonts/ArchivoSemiExpanded-SemiBold.ttf"),
    include_bytes!("../assets/fonts/ArchivoSemiExpanded-Bold.ttf"),
    include_bytes!("../assets/fonts/IBMPlexMono-Regular.ttf"),
    include_bytes!("../assets/fonts/IBMPlexMono-Medium.ttf"),
];

pub fn load_fonts(cx: &App) -> anyhow::Result<()> {
    cx.text_system()
        .add_fonts(FONTS.iter().map(|f| Cow::Borrowed(*f)).collect())
}

fn svg(bytes: &'static [u8]) -> Arc<Image> {
    Arc::new(Image::from_bytes(ImageFormat::Svg, bytes.to_vec()))
}

macro_rules! art {
    ($name:ident, $file:literal) => {
        pub static $name: LazyLock<Arc<Image>> =
            LazyLock::new(|| svg(include_bytes!(concat!("../assets/", $file))));
    };
}

art!(MARK, "marks/mars-voice.svg");
art!(MARK_DARK, "marks/mars-voice-dark.svg");
art!(MARK_32, "marks/mars-voice-32.svg");
art!(MARK_32_DARK, "marks/mars-voice-32-dark.svg");
art!(MARK_16, "marks/mars-voice-16.svg");
art!(MARK_16_DARK, "marks/mars-voice-16-dark.svg");
art!(PLANET_CORNER, "marks/planet-corner.svg");
// Mars alone: the overlay microphone and the centre of Órbita.
art!(MARS, "marks/mars-planet.svg");
art!(MARS_SMALL, "marks/mars-planet-small.svg");
art!(GRAIN_FINE, "grain/grain-fine.svg");
art!(GRAIN_COARSE, "grain/grain-coarse.svg");
art!(GRAIN_NIGHT, "grain/grain-night.svg");
art!(GRAIN_COARSE_NIGHT, "grain/grain-coarse-night.svg");

/// Side of a grain tile in logical pixels; tiles are seamless.
pub const GRAIN_TILE: f32 = 256.;

/// Colour art already rasterised, by the identity of its [`Image`].
///
/// GPUI decodes an `img(Arc<Image>)` lazily on a background thread the first
/// time a frame paints it, and paints nothing there until it lands. The
/// grain tiles are `feTurbulence` filters and the planet carries the same
/// grain: in debug builds they take hundreds of milliseconds, so the first
/// overlay would open flat and fill in a moment later. [`prewarm`] renders
/// them ahead of time with the renderer and scale GPUI itself uses, and
/// [`art`] hands the finished bitmap to `img()` so no decode happens in the
/// frame. Anything not prewarmed keeps the lazy path.
static BITMAPS: LazyLock<Mutex<HashMap<usize, Arc<RenderImage>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static PENDING: LazyLock<Mutex<HashSet<usize>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

fn key(image: &Arc<Image>) -> usize {
    Arc::as_ptr(image) as usize
}

/// Image source for `img()`: the prewarmed bitmap when there is one.
pub fn art(image: Arc<Image>) -> ImageSource {
    match BITMAPS.lock().unwrap().get(&key(&image)) {
        Some(bitmap) => ImageSource::Render(bitmap.clone()),
        None => ImageSource::Image(image),
    }
}

/// Whether `image` is already rasterised (harness).
#[cfg_attr(not(feature = "screenshots"), allow(dead_code))]
pub fn bitmap_ready(image: &Arc<Image>) -> bool {
    BITMAPS.lock().unwrap().contains_key(&key(image))
}

/// Rasterises `images` in the background, once each.
pub fn prewarm(images: impl IntoIterator<Item = Arc<Image>>, cx: &mut App) {
    for image in images {
        let k = key(&image);
        if BITMAPS.lock().unwrap().contains_key(&k) || !PENDING.lock().unwrap().insert(k) {
            continue;
        }
        let renderer = cx.svg_renderer();
        let task = cx.background_spawn(async move {
            // The same call GPUI's image decoder makes for SVG sources.
            renderer.render_single_frame(image.bytes(), 1.0).ok()
        });
        cx.spawn(async move |_| {
            if let Some(bitmap) = task.await {
                BITMAPS.lock().unwrap().insert(k, bitmap);
            }
            PENDING.lock().unwrap().remove(&k);
        })
        .detach();
    }
}

/// Mars voice mark for a rendered size: speckled shadow and grain from
/// 40 px, fewer and larger specks at 20-40 px, a solid crescent below.
pub fn mark(size: f32, dark: bool) -> Arc<Image> {
    match (size, dark) {
        (s, false) if s >= 40. => MARK.clone(),
        (s, true) if s >= 40. => MARK_DARK.clone(),
        (s, false) if s >= 20. => MARK_32.clone(),
        (s, true) if s >= 20. => MARK_32_DARK.clone(),
        (_, false) => MARK_16.clone(),
        (_, true) => MARK_16_DARK.clone(),
    }
}

/// Mars alone for a rendered diameter.
pub fn mars(size: f32) -> Arc<Image> {
    if size >= 22. {
        MARS.clone()
    } else {
        MARS_SMALL.clone()
    }
}

/// Dicta's own icon family: 20 px grid, 1.6 stroke, round caps, and one
/// filled "planet" dot per pictogram.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    General,
    Provider,
    Writing,
    Vocabulary,
    Corrections,
    Microphone,
    Shortcuts,
    Overlay,
    Background,
    Close,
    Copy,
    Paste,
    Retry,
    Refresh,
    Check,
    Alert,
    Link,
    Stop,
    Home,
    History,
    Search,
    Trash,
    Play,
    Appearance,
    Audio,
    More,
    Back,
    Pause,
    SkipBack,
    SkipForward,
    PrevSentence,
    NextSentence,
    ChevronDown,
    Compact,
    Expand,
    Read,
    Minus,
    Plus,
}

impl Icon {
    pub fn path(self) -> &'static str {
        match self {
            Self::General => "icons/general.svg",
            Self::Provider => "icons/provider.svg",
            Self::Writing => "icons/writing.svg",
            Self::Vocabulary => "icons/vocabulary.svg",
            Self::Corrections => "icons/corrections.svg",
            Self::Microphone => "icons/microphone.svg",
            Self::Shortcuts => "icons/shortcuts.svg",
            Self::Overlay => "icons/overlay.svg",
            Self::Background => "icons/background.svg",
            Self::Close => "icons/close.svg",
            Self::Copy => "icons/copy.svg",
            Self::Paste => "icons/paste.svg",
            Self::Retry => "icons/retry.svg",
            Self::Refresh => "icons/refresh.svg",
            Self::Check => "icons/check.svg",
            Self::Alert => "icons/alert.svg",
            Self::Link => "icons/link.svg",
            Self::Stop => "icons/stop.svg",
            Self::Home => "icons/home.svg",
            Self::History => "icons/history.svg",
            Self::Search => "icons/search.svg",
            Self::Trash => "icons/trash.svg",
            Self::Play => "icons/play.svg",
            Self::Appearance => "icons/appearance.svg",
            Self::Audio => "icons/audio.svg",
            Self::More => "icons/more.svg",
            Self::Back => "icons/back.svg",
            Self::Pause => "icons/pause.svg",
            Self::SkipBack => "icons/skip-back.svg",
            Self::SkipForward => "icons/skip-forward.svg",
            Self::PrevSentence => "icons/prev-sentence.svg",
            Self::NextSentence => "icons/next-sentence.svg",
            Self::ChevronDown => "icons/chevron-down.svg",
            Self::Compact => "icons/compact.svg",
            Self::Expand => "icons/expand.svg",
            Self::Read => "icons/read.svg",
            Self::Minus => "icons/minus.svg",
            Self::Plus => "icons/plus.svg",
        }
    }
}

macro_rules! icon_bytes {
    ($($file:literal),* $(,)?) => {
        &[$(($file, include_bytes!(concat!("../assets/", $file)) as &[u8])),*]
    };
}

const ICONS: &[(&str, &[u8])] = icon_bytes!(
    "icons/general.svg",
    "icons/provider.svg",
    "icons/writing.svg",
    "icons/vocabulary.svg",
    "icons/corrections.svg",
    "icons/microphone.svg",
    "icons/shortcuts.svg",
    "icons/overlay.svg",
    "icons/background.svg",
    "icons/close.svg",
    "icons/copy.svg",
    "icons/paste.svg",
    "icons/retry.svg",
    "icons/refresh.svg",
    "icons/check.svg",
    "icons/alert.svg",
    "icons/link.svg",
    "icons/stop.svg",
    "icons/home.svg",
    "icons/history.svg",
    "icons/search.svg",
    "icons/trash.svg",
    "icons/play.svg",
    "icons/appearance.svg",
    "icons/audio.svg",
    "icons/more.svg",
    "icons/back.svg",
    "icons/pause.svg",
    "icons/skip-back.svg",
    "icons/skip-forward.svg",
    "icons/prev-sentence.svg",
    "icons/next-sentence.svg",
    "icons/chevron-down.svg",
    "icons/compact.svg",
    "icons/expand.svg",
    "icons/read.svg",
    "icons/minus.svg",
    "icons/plus.svg",
);

/// Serves the embedded icons to `svg()`.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        Ok(ICONS
            .iter()
            .find(|(p, _)| *p == path)
            .map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> anyhow::Result<Vec<SharedString>> {
        Ok(ICONS
            .iter()
            .filter(|(p, _)| p.starts_with(path))
            .map(|(p, _)| SharedString::from(*p))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_is_embedded() {
        for icon in [
            Icon::General,
            Icon::Provider,
            Icon::Writing,
            Icon::Vocabulary,
            Icon::Corrections,
            Icon::Microphone,
            Icon::Shortcuts,
            Icon::Overlay,
            Icon::Background,
            Icon::Close,
            Icon::Copy,
            Icon::Paste,
            Icon::Retry,
            Icon::Refresh,
            Icon::Check,
            Icon::Alert,
            Icon::Link,
            Icon::Stop,
            Icon::Home,
            Icon::History,
            Icon::Search,
            Icon::Trash,
            Icon::Play,
            Icon::Appearance,
            Icon::Audio,
            Icon::More,
            Icon::Back,
            Icon::Pause,
            Icon::SkipBack,
            Icon::SkipForward,
            Icon::PrevSentence,
            Icon::NextSentence,
            Icon::ChevronDown,
            Icon::Compact,
            Icon::Expand,
            Icon::Read,
            Icon::Minus,
            Icon::Plus,
        ] {
            assert!(Assets.load(icon.path()).unwrap().is_some(), "{icon:?}");
        }
    }
}
