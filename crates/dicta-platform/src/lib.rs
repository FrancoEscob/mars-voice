//! Desktop integration for Linux, Windows and macOS. No microphone is opened
//! until [`audio::start`] is called. Blocking desktop/device operations belong
//! on the controller's worker thread.
//!
//! [`capabilities::detect`] reports what works on the current session;
//! [`backend`] holds the per-service traits behind [`desktop`] and
//! [`selection`]. Linux keeps the Wayland/Hyprland code and Windows has its
//! own backend; systems without one yet (macOS) compile with stubs that
//! return actionable Spanish errors.
pub mod audio;
pub mod backend;
pub mod capabilities;
pub mod desktop;
pub mod playback;
pub mod selection;
#[cfg(windows)]
mod shell_windows;
pub mod shortcuts;
pub mod tray;

pub use capabilities::{Capabilities, HotkeySupport, Session, detect};
