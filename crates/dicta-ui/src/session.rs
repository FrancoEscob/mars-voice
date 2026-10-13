//! Which desktop the UI runs on, for copy and hints that only apply there
//! ("En Hyprland: …", xdg-desktop-portal, Cmd vs Ctrl).
//!
//! This is a stopgap read once from the build target and the environment:
//! the platform layer will publish a capabilities snapshot (MP-01) and the
//! UI should take the session from it instead. Nothing here changes
//! behaviour; it only decides which hints are shown.

/// The desktop session, as far as the copy cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Session {
    Hyprland,
    /// Any other Wayland compositor (GNOME, KDE, Sway…).
    Wayland,
    X11,
    Windows,
    MacOs,
}

impl Session {
    pub fn detect() -> Self {
        if cfg!(target_os = "windows") {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::MacOs
        } else {
            Self::from_env(
                std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some(),
                std::env::var_os("WAYLAND_DISPLAY").is_some(),
            )
        }
    }

    /// Unix sessions other than macOS, from the environment.
    fn from_env(hyprland: bool, wayland: bool) -> Self {
        match (hyprland, wayland) {
            (true, _) => Self::Hyprland,
            (false, true) => Self::Wayland,
            (false, false) => Self::X11,
        }
    }

    pub fn is_hyprland(self) -> bool {
        self == Self::Hyprland
    }

    /// Linux and other freedesktop systems: primary selection, portals.
    pub fn is_linux(self) -> bool {
        matches!(self, Self::Hyprland | Self::Wayland | Self::X11)
    }

    /// Name of the platform modifier that in-app `secondary-` bindings use.
    pub fn mod_key(self) -> &'static str {
        match self {
            Self::MacOs => "Cmd",
            _ => "Ctrl",
        }
    }

    /// "Ctrl+Enter" or "Cmd+Enter".
    pub fn with_mod(self, key: &str) -> String {
        format!("{}+{key}", self.mod_key())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hyprland_is_told_apart_from_other_linux_sessions() {
        assert_eq!(Session::from_env(true, true), Session::Hyprland);
        assert_eq!(Session::from_env(false, true), Session::Wayland);
        assert_eq!(Session::from_env(false, false), Session::X11);
        assert!(Session::Wayland.is_linux() && !Session::Wayland.is_hyprland());
        assert!(!Session::Windows.is_linux() && !Session::MacOs.is_linux());
        assert_eq!(Session::MacOs.with_mod("Enter"), "Cmd+Enter");
        assert_eq!(Session::Hyprland.with_mod("Enter"), "Ctrl+Enter");
    }
}
