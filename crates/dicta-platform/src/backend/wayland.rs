//! Today's Linux backend. Pure delegation to the existing implementations,
//! which keep their own Wayland/Hyprland checks and Spanish errors:
//! `desktop/hyprland.rs`, `selection/wl_paste.rs` and `selection/copy.rs`.
use super::{Clipboard, FocusTracker, Injector, SelectionReader, WindowPlacer};
use crate::desktop::{FloatingWindow, PastePlan, PasteTarget, hyprland};
use crate::selection::{CopiedSelection, CopyOptions, SelectionSource, copy, wl_paste};
use anyhow::Result;

pub(super) struct WaylandHyprland;

impl Clipboard for WaylandHyprland {
    fn copy(&self, text: &str) -> Result<()> {
        hyprland::copy(text)
    }
}

impl FocusTracker for WaylandHyprland {
    fn active_target(&self) -> Result<PasteTarget> {
        hyprland::active_target()
    }
}

impl Injector for WaylandHyprland {
    fn paste(&self, text: &str, expected: Option<&PasteTarget>, keys: &str) -> Result<()> {
        hyprland::paste(text, expected, keys)
    }
    fn prepare_paste(&self, remembered: Option<&PasteTarget>) -> Result<PastePlan> {
        hyprland::prepare_paste(remembered)
    }
    fn paste_prepared(
        &self,
        text: &str,
        plan: &PastePlan,
        keys: &str,
        is_current: &dyn Fn() -> bool,
    ) -> Result<()> {
        hyprland::paste_prepared(text, plan, keys, is_current)
    }
    fn paste_into(
        &self,
        text: &str,
        target: &PasteTarget,
        keys: &str,
        extra_terminal_classes: &[String],
        is_current: &dyn Fn() -> bool,
    ) -> Result<()> {
        let keys = copy::paste_keys_for(target, keys, extra_terminal_classes);
        hyprland::paste_into(text, target, keys, is_current)
    }
}

impl SelectionReader for WaylandHyprland {
    fn read_text(&self, source: SelectionSource, max_bytes: usize) -> Result<Option<String>> {
        wl_paste::read_text(source, max_bytes)
    }
    fn copy_selection(&self, options: CopyOptions<'_>) -> Result<Option<String>> {
        copy::copy_selection(options)
    }
    fn copy_selection_from(&self, options: CopyOptions<'_>) -> Result<CopiedSelection> {
        copy::copy_selection_from(options)
    }
}

impl WindowPlacer for WaylandHyprland {
    fn set_placement_enabled(&self, window: FloatingWindow, enabled: bool) {
        hyprland::set_placement_enabled(window, enabled);
    }
    fn place(&self, window: FloatingWindow, x: f32, y: f32) {
        hyprland::place(window, x, y);
    }
}
