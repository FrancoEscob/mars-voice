//! Seams for the desktop services the controller uses. Each service is a
//! trait; [`current`] returns the implementations for this system. The free
//! functions in [`crate::desktop`] and [`crate::selection`] are thin wrappers
//! over it, so callers never name a backend.
//!
//! | System | Backend |
//! |---|---|
//! | Linux | today's code: wl-clipboard on Wayland, hyprctl + wtype on Hyprland. Each call keeps its own session check and error, so Hyprland runs exactly the previous code paths and other sessions get the previous messages |
//! | Windows | `windows`: arboard, `GetForegroundWindow`, `SendInput` and the clipboard sequence number. The flows are in `win_flow` (tested on every system); placement is a no-op because GPUI positions the windows |
//! | macOS | `unsupported`: actionable Spanish errors; placement is a no-op |
//!
//! Later phases replace individual services per session (for example
//! arboard for the clipboard on X11) without touching the controller. What
//! each session supports is reported by [`crate::capabilities::detect`];
//! keep both in sync when adding a backend.
//!
//! All methods may block (subprocesses, compositor IPC); call them from a
//! worker thread. None of them polls, watches or logs content.
use crate::desktop::{FloatingWindow, PastePlan, PasteTarget};
use crate::selection::{CopiedSelection, CopyOptions, SelectionSource};
use anyhow::Result;

#[cfg(any(not(any(target_os = "linux", windows)), test))]
mod unsupported;
#[cfg(target_os = "linux")]
mod wayland;
#[cfg(any(windows, test))]
mod win_flow;
#[cfg(windows)]
mod windows;

/// Writes text to the system clipboard.
pub trait Clipboard: Send + Sync {
    /// Copy UTF-8 text (at most 1 MiB). Only on an explicit user action.
    fn copy(&self, text: &str) -> Result<()>;
}

/// Reports which external window has keyboard focus.
pub trait FocusTracker: Send + Sync {
    /// The focused window, refusing this process's own windows. Queried once
    /// per explicit action, never tracked in the background.
    fn active_target(&self) -> Result<PasteTarget>;
}

/// Pastes into another application. Copies first, so on failure the text is
/// still in the clipboard. Never sends Enter.
pub trait Injector: Send + Sync {
    /// Paste into the focused external window; refuses if it is not `expected`.
    fn paste(&self, text: &str, expected: Option<&PasteTarget>, keys: &str) -> Result<()>;
    /// Capture the destination when the user asks to paste.
    fn prepare_paste(&self, remembered: Option<&PasteTarget>) -> Result<PastePlan>;
    /// Execute a plan. `is_current` is checked before changing focus or
    /// injecting keys, so a cancelled operation stops there.
    fn paste_prepared(
        &self,
        text: &str,
        plan: &PastePlan,
        keys: &str,
        is_current: &dyn Fn() -> bool,
    ) -> Result<()>;
    /// Paste into a remembered `target` (the source of a copied selection),
    /// focusing it when needed. Terminals get Ctrl+Shift+V for Ctrl+V.
    fn paste_into(
        &self,
        text: &str,
        target: &PasteTarget,
        keys: &str,
        extra_terminal_classes: &[String],
        is_current: &dyn Fn() -> bool,
    ) -> Result<()>;
}

/// Reads selected or copied text for read-aloud.
pub trait SelectionReader: Send + Sync {
    /// One read of the primary selection or clipboard; never sends keys.
    fn read_text(&self, source: SelectionSource, max_bytes: usize) -> Result<Option<String>>;
    /// Send the copy shortcut to the focused window, read the result and
    /// (optionally) restore the previous clipboard.
    fn copy_selection(&self, options: CopyOptions<'_>) -> Result<Option<String>>;
    /// Same, also returning the window that received the copy shortcut.
    fn copy_selection_from(&self, options: CopyOptions<'_>) -> Result<CopiedSelection>;
}

/// Moves this process's floating windows after they are mapped.
pub trait WindowPlacer: Send + Sync {
    /// Disabling cancels pending placements (the window stays centered).
    fn set_placement_enabled(&self, window: FloatingWindow, enabled: bool);
    /// Best effort and non-blocking; logical pixels relative to the window's
    /// monitor.
    fn place(&self, window: FloatingWindow, x: f32, y: f32);
}

/// The services selected for this system.
pub struct Backend {
    /// Stable identifier for diagnostics: `"wayland-hyprland"`, `"windows"` or
    /// `"unsupported"`.
    pub name: &'static str,
    pub clipboard: &'static dyn Clipboard,
    pub focus: &'static dyn FocusTracker,
    pub injector: &'static dyn Injector,
    pub selection: &'static dyn SelectionReader,
    pub placement: &'static dyn WindowPlacer,
}

#[cfg(target_os = "linux")]
static CURRENT: Backend = Backend {
    name: "wayland-hyprland",
    clipboard: &wayland::WaylandHyprland,
    focus: &wayland::WaylandHyprland,
    injector: &wayland::WaylandHyprland,
    selection: &wayland::WaylandHyprland,
    placement: &wayland::WaylandHyprland,
};

#[cfg(windows)]
static CURRENT: Backend = windows::BACKEND;

#[cfg(not(any(target_os = "linux", windows)))]
static CURRENT: Backend = unsupported::BACKEND;

/// The backend for this system, chosen at compile time. Cheap; no I/O.
pub fn current() -> &'static Backend {
    &CURRENT
}
