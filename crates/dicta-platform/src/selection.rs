//! One-shot reads of the primary selection or clipboard for "read aloud".
//! Runs only when the controller calls [`read_text`] or [`copy_selection`] for
//! an explicit user action: no polling between actions, no watching.
//! [`read_text`] never synthesizes keys; [`copy_selection`] sends the copy
//! shortcut (Ctrl+Shift+C to terminals, where Ctrl+C would interrupt the
//! running program) to the active window only when asked. Content never
//! reaches logs or error messages.
//!
//! Thin wrappers over [`crate::backend::current`]. On Linux they run today's
//! wl-paste (`selection/wl_paste.rs`) and Hyprland copy (`selection/copy.rs`)
//! code unchanged; on systems without a backend yet they return an actionable
//! Spanish error.
use crate::backend;
use anyhow::Result;
use std::time::Duration;

#[cfg(target_os = "linux")]
pub(crate) mod copy;
#[cfg(target_os = "linux")]
pub(crate) mod wl_paste;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionSource {
    Primary,
    Clipboard,
}

/// Total budget for one read, fallbacks included. The child is killed on expiry.
pub const READ_TIMEOUT: Duration = Duration::from_millis(800);

pub struct CopyOptions<'a> {
    /// Larger selections are an error, like [`read_text`].
    pub max_bytes: usize,
    /// Put the previous clipboard back afterwards. When false the copied
    /// selection stays in the clipboard, exactly like a manual copy.
    pub restore_clipboard: bool,
    /// Window classes (besides the built-in list) that get Ctrl+Shift+C.
    pub extra_terminal_classes: &'a [String],
}

/// Blocking. Reads the selection or clipboard as text. `Ok(None)` when nothing
/// is selected/copied, the content is only whitespace, or no textual type is
/// offered. Content larger than `max_bytes`, invalid UTF-8 and a source app
/// that does not answer within [`READ_TIMEOUT`] are actionable errors. On
/// Linux this needs Wayland and wl-clipboard; X11 is not implemented yet.
pub fn read_text(source: SelectionSource, max_bytes: usize) -> Result<Option<String>> {
    backend::current().selection.read_text(source, max_bytes)
}

/// Blocking, at most ≈1.5 s. Copies the active external window's selection
/// and returns it as text. `Ok(None)` = nothing was selected (the copy produced
/// no text). Requires Hyprland on Wayland; otherwise the error suggests
/// «Solo el portapapeles». The previous clipboard is restored as one MIME type
/// (plain text preferred, then PNG, then the first offered); content over
/// 16 MiB cannot be saved, so in that case the copied selection stays in the
/// clipboard, or the clipboard stays empty if nothing was copied (reported in
/// the error).
pub fn copy_selection(options: CopyOptions<'_>) -> Result<Option<String>> {
    backend::current().selection.copy_selection(options)
}
