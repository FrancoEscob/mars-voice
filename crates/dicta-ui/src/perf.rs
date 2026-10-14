//! Work counters for the render hot paths. The perf harness (`bench`,
//! feature `screenshots`) and tests read them to assert on *how much work*
//! a scenario does, which is stable across machines and build profiles,
//! unlike wall-clock timings. In ordinary builds `bump`/`add` compile to
//! nothing, so the hot paths carry no atomics.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

macro_rules! counters {
    ($($(#[$doc:meta])* $name:ident),* $(,)?) => {
        $(
            $(#[$doc])*
            pub static $name: AtomicU64 = AtomicU64::new(0);
        )*

        /// Snapshot of every counter.
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
        #[allow(non_snake_case)]
        pub struct Counters {
            $(pub $name: u64,)*
        }

        impl Counters {
            pub fn now() -> Self {
                Self { $($name: $name.load(Relaxed),)* }
            }

            /// Work done since `earlier`.
            pub fn since(self, earlier: Self) -> Self {
                Self { $($name: self.$name.wrapping_sub(earlier.$name),)* }
            }

            pub fn fields(&self) -> Vec<(&'static str, u64)> {
                vec![$((stringify!($name), self.$name),)*]
            }
        }
    };
}

counters! {
    /// `Overlay::render` calls (one per overlay frame).
    OVERLAY_RENDERS,
    /// `Settings::render` calls (main window frames).
    SETTINGS_RENDERS,
    /// Paints of the voice canvas (wave, dots, meter or orbit back layer).
    VOICE_PAINTS,
    /// `request_animation_frame` calls from the voice drawing.
    FRAMES_REQUESTED,
    /// Snapshots applied by the hub.
    HUB_APPLIES,
    /// Hub notifications that reached observers (windows) after a snapshot.
    HUB_NOTIFIES,
    /// Settings digests computed on hub notifications.
    SETTINGS_DIGESTS,
    /// Notes hashed inside a settings digest.
    DIGEST_NOTES,
    /// Grain tile `img` elements built.
    GRAIN_TILES,
    /// Text field prepaints (each shapes its text).
    FIELD_PREPAINTS,
    /// Live transcript elements built (one `StyledText` each).
    LIVE_TEXTS,
    /// `SearchNotes` commands sent by the history search field.
    SEARCH_SENT,
    /// Live-only snapshots (level, clock, transcript) that invalidated the
    /// overlay without notifying the hub (so the main window stayed quiet).
    LIVE_TICKS,
    /// Live-only snapshots that changed nothing visible and drew nothing.
    SKIPPED_TICKS,
    /// `Reader::render` calls (read-aloud window frames).
    READER_RENDERS,
    /// Reader text areas built (one element per visible-or-not paragraph).
    READER_TEXTS,
    /// Live-only snapshots that redrew the reader alone (position, word,
    /// generation, output level).
    READER_TICKS,
    /// `Writer::render` calls (Escritura window frames).
    WRITER_RENDERS,
}

/// Counting is compiled in for tests and the harness only.
pub const ENABLED: bool = cfg!(any(test, feature = "screenshots"));

#[inline(always)]
pub fn bump(counter: &AtomicU64) {
    if ENABLED {
        counter.fetch_add(1, Relaxed);
    }
}

#[inline(always)]
pub fn add(counter: &AtomicU64, n: u64) {
    if ENABLED {
        counter.fetch_add(n, Relaxed);
    }
}
