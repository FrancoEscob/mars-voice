//! Explicit clipboard/paste actions and placement of this process's floating
//! windows. No clipboard polling, key logging, or background focus tracking.
//! An explicit paste may restore only the external target remembered for this
//! dictation, after validating its identity.
//!
//! These functions are thin wrappers over [`crate::backend::current`], so the
//! controller does not depend on a desktop. On Linux they run today's
//! wl-clipboard/Hyprland code unchanged (`desktop/hyprland.rs`); on systems
//! without a backend yet they return an actionable Spanish error, and
//! placement does nothing. Use [`crate::capabilities::detect`] to know in
//! advance what will work. All functions may block for up to a few seconds;
//! call them from a worker thread.
use crate::backend;
use anyhow::Result;
use serde::Deserialize;

#[cfg(target_os = "linux")]
pub(crate) mod hyprland;

pub const APP_ID: &str = "io.github.francoescob.Dicta";
/// Dictation overlay window class (Wayland app_id).
pub const OVERLAY_CLASS: &str = "dicta-overlay";
/// Read-aloud window class (Wayland app_id).
pub const READER_CLASS: &str = "dicta-reader";
/// Writing window class (Wayland app_id): fix and translate the selection.
pub const WRITER_CLASS: &str = dicta_core::writer::WRITER_APP_ID;

/// A window that can receive a paste. Today's fields come from Hyprland's
/// `hyprctl -j activewindow`; other backends fill the closest equivalent.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct PasteTarget {
    pub address: String,
    pub pid: i64,
    pub class: String,
    #[serde(default, rename = "initialClass")]
    pub initial_class: String,
    /// Compositor lifetime ID prevents an address reused by the same process
    /// from impersonating the window remembered when dictation began.
    #[serde(default, rename = "stableId")]
    pub stable_id: Option<String>,
}

/// Opaque plan captured when the user requests Paste. Restoration is permitted
/// only if that request originated in a window owned by this Dicta process.
#[derive(Clone, Debug)]
// Only backends read the fields; systems without one never build a plan.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub struct PastePlan {
    target: PasteTarget,
    restore_from: Option<PasteTarget>,
}
/// For backends outside this module (Windows).
#[cfg(any(windows, test))]
impl PastePlan {
    pub(crate) fn new(target: PasteTarget, restore_from: Option<PasteTarget>) -> Self {
        Self {
            target,
            restore_from,
        }
    }
    pub(crate) fn target(&self) -> &PasteTarget {
        &self.target
    }
    pub(crate) fn restore_from(&self) -> Option<&PasteTarget> {
        self.restore_from.as_ref()
    }
}

/// Floating windows this process places itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FloatingWindow {
    /// The dictation overlay, class [`OVERLAY_CLASS`].
    Overlay,
    /// The read-aloud window, class [`READER_CLASS`].
    Reader,
    /// The writing window, class [`WRITER_CLASS`].
    Writer,
}
impl FloatingWindow {
    pub fn class(self) -> &'static str {
        match self {
            Self::Overlay => OVERLAY_CLASS,
            Self::Reader => READER_CLASS,
            Self::Writer => WRITER_CLASS,
        }
    }
}

/// Copy UTF-8 text (at most 1 MiB). On Wayland, wl-copy forks its clipboard
/// owner, which remains alive after this function returns until another
/// application takes ownership.
pub fn copy(text: &str) -> Result<()> {
    backend::current().clipboard.copy(text)
}

/// Capture an external destination before displaying a focusable overlay.
/// Refuses this process's own windows.
pub fn active_target() -> Result<PasteTarget> {
    backend::current().focus.active_target()
}

/// Copy first, then explicitly paste into the currently focused external window.
/// With expected set, refuses if focus has changed. Never sends Enter or raises a
/// different window. Errors preserve the clipboard for manual paste.
/// Wayland has no atomic verify-focus-and-inject operation: focus can still race
/// after the last check. The user should keep the destination focused.
pub fn paste(text: &str, expected: Option<&PasteTarget>, keys: &str) -> Result<()> {
    backend::current().injector.paste(text, expected, keys)
}

/// Decide where a paste requested now should go: the active external window,
/// or `remembered` when the request comes from one of this process's windows.
pub fn prepare_paste(remembered: Option<&PasteTarget>) -> Result<PastePlan> {
    backend::current().injector.prepare_paste(remembered)
}

/// Copies first; restores a remembered live destination only for a request from
/// this process's UI. Rechecks focus immediately before input and never Enter.
/// `is_current` lets the controller cancel a queued/in-progress operation before
/// it changes focus or injects keys. Clipboard writes already started cannot undo.
/// Focus-check and key injection remain separate compositor operations.
pub fn paste_prepared(
    text: &str,
    plan: &PastePlan,
    keys: &str,
    is_current: impl Fn() -> bool,
) -> Result<()> {
    backend::current()
        .injector
        .paste_prepared(text, plan, keys, &is_current)
}

/// Writing "Reemplazar": copies first, then pastes into `target`, a window
/// remembered when its selection was copied ([`crate::selection::copy_selection_from`]).
/// Refuses this process's windows and a target that is gone or changed
/// identity; the text then stays in the clipboard. Focuses `target` when it is
/// not active (on Windows only while this process may set the foreground) and
/// rechecks focus right before input; never Enter. Terminals (built-in list
/// plus `extra_terminal_classes`) get Ctrl+Shift+V instead of Ctrl+V.
/// `is_current` cancels before focus changes or keys are typed.
pub fn paste_into(
    text: &str,
    target: &PasteTarget,
    keys: &str,
    extra_terminal_classes: &[String],
    is_current: impl Fn() -> bool,
) -> Result<()> {
    backend::current()
        .injector
        .paste_into(text, target, keys, extra_terminal_classes, &is_current)
}

/// Enable manual offsets from controller config. Disabling cancels pending
/// placements so an old callback cannot move a newly centered overlay.
pub fn set_overlay_placement_enabled(enabled: bool) {
    backend::current()
        .placement
        .set_placement_enabled(FloatingWindow::Overlay, enabled);
}

/// Best-effort, non-blocking one-shot placement of this process's mapped floating
/// `dicta-overlay`. Coordinates are logical pixels relative to its actual monitor.
/// UI already resolves negative offsets against its chosen display; this adapter
/// cannot reconstruct bottom/right anchoring if the compositor chose another size.
/// It clamps to the actual monitor and never moves settings or another process.
/// No repaint/snapshot loop calls this function; only the UI's open callback does.
pub fn place_overlay(x: f32, y: f32) {
    backend::current()
        .placement
        .place(FloatingWindow::Overlay, x, y);
}

/// Same contract as [`set_overlay_placement_enabled`], for the read-aloud window.
/// Independent of the dictation overlay's state.
pub fn set_reader_placement_enabled(enabled: bool) {
    backend::current()
        .placement
        .set_placement_enabled(FloatingWindow::Reader, enabled);
}

/// Same contract as [`place_overlay`], for this process's floating `dicta-reader`.
pub fn place_reader(x: f32, y: f32) {
    backend::current()
        .placement
        .place(FloatingWindow::Reader, x, y);
}

/// Same contract as [`set_overlay_placement_enabled`], for the writing window.
pub fn set_writer_placement_enabled(enabled: bool) {
    backend::current()
        .placement
        .set_placement_enabled(FloatingWindow::Writer, enabled);
}

/// Same contract as [`place_overlay`], for this process's floating `dicta-writer`.
pub fn place_writer(x: f32, y: f32) {
    backend::current()
        .placement
        .place(FloatingWindow::Writer, x, y);
}
