//! What this build can do on the current desktop session, so the controller
//! and UI can say up front what works and what needs a manual step.
//!
//! [`detect`] is cheap and non-blocking: it reads environment variables and
//! looks helper programs up on `PATH` (a few `stat` calls). It never spawns a
//! process, opens a socket or talks to D-Bus, so it can run on the UI thread.
//! It reports what the **current code** supports, not what the system could
//! support with later backends:
//!
//! - Hyprland on Wayland, with `wl-clipboard`, `wtype` and `hyprctl`: everything.
//! - Other Wayland sessions (Sway, KDE, GNOME…): clipboard and selection
//!   reading through `wl-clipboard`; no paste, focus, copy-selection or
//!   placement; shortcuts only as a desktop custom shortcut running
//!   `dicta --toggle`.
//! - X11: tray and toggle only (no X11 clipboard yet).
//! - Windows and macOS: nothing yet (see `docs/PLAN-MULTIPLATAFORMA.md`).
//!
//! Keep it in sync with [`crate::backend`] when a backend is added.
use serde::{Deserialize, Serialize};

/// The desktop session, from the environment. On Linux the Hyprland instance
/// signature wins, exactly as the Hyprland code paths decide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Session {
    Hyprland,
    Sway,
    KdeWayland,
    GnomeWayland,
    OtherWayland,
    X11,
    Windows,
    #[serde(rename = "macos")]
    MacOs,
    Unknown,
}

/// How global shortcuts can reach the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HotkeySupport {
    /// No global shortcut can reach the app.
    None,
    /// Only a toggle: a desktop custom shortcut that runs `dicta --toggle`.
    Toggle,
    /// Press and release both arrive, so hold-to-talk works (compositor binds
    /// with `--shortcut-press`/`--shortcut-release`, or the portal).
    Hold,
}

/// Serializable for the controller/UI. Unknown fields from a newer build are
/// ignored and missing ones default to "unsupported".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Capabilities {
    pub session: Session,
    /// [`crate::desktop::copy`] works.
    pub clipboard: bool,
    /// [`crate::desktop::paste_prepared`] can type the paste keys into another app.
    pub paste_injection: bool,
    /// [`crate::selection::copy_selection`] works ("Copiar la selección").
    pub copy_selection: bool,
    /// [`crate::selection::read_text`] can ask for the primary selection. The
    /// compositor may still have it turned off (Hyprland
    /// `misc:middle_click_paste = false`); that is reported when reading.
    pub primary_selection: bool,
    /// [`crate::desktop::active_target`] identifies the focused window, so a
    /// paste requested from Mars voice can return to it.
    pub focus_tracking: bool,
    /// [`crate::desktop::place_overlay`] / `place_reader` move the windows;
    /// otherwise they open where the system puts them.
    pub window_placement: bool,
    pub global_hotkeys: HotkeySupport,
    /// A tray backend exists. On Linux the desktop still needs a
    /// StatusNotifierItem host (waybar, KDE, GNOME's AppIndicator extension);
    /// [`crate::tray::spawn`] reports when there is none.
    pub tray: bool,
    /// The compositor binds `dicta` commands and Mars voice ships a snippet
    /// for it ([`crate::shortcuts::HYPRLAND_BIND_EXAMPLE`]).
    pub compositor_binds: bool,
    /// Helper programs this session needs that are not on `PATH` (for example
    /// `wl-copy`, from wl-clipboard). What needs them is reported as false.
    pub missing_programs: Vec<String>,
}

impl Capabilities {
    /// Nothing available: systems without a backend yet.
    pub fn none(session: Session) -> Self {
        Self {
            session,
            clipboard: false,
            paste_injection: false,
            copy_selection: false,
            primary_selection: false,
            focus_tracking: false,
            window_placement: false,
            global_hotkeys: HotkeySupport::None,
            tray: false,
            compositor_binds: false,
            missing_programs: Vec::new(),
        }
    }
}

impl Default for Capabilities {
    fn default() -> Self {
        Self::none(Session::Unknown)
    }
}

/// Detect the session and what works in it. See the module docs.
pub fn detect() -> Capabilities {
    #[cfg(target_os = "linux")]
    {
        linux(&SystemProbe)
    }
    #[cfg(target_os = "windows")]
    {
        Capabilities::none(Session::Windows)
    }
    #[cfg(target_os = "macos")]
    {
        Capabilities::none(Session::MacOs)
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        Capabilities::none(Session::Unknown)
    }
}

/// Environment access, injectable for tests.
trait Probe {
    /// Value of an environment variable; unset and empty are both `None`.
    fn var(&self, name: &str) -> Option<String>;
    /// An executable file with this name is on `PATH`.
    fn has_program(&self, name: &str) -> bool;
}

#[cfg(target_os = "linux")]
struct SystemProbe;

#[cfg(target_os = "linux")]
impl Probe for SystemProbe {
    fn var(&self, name: &str) -> Option<String> {
        std::env::var_os(name)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_string_lossy().into_owned())
    }
    fn has_program(&self, name: &str) -> bool {
        use std::os::unix::fs::PermissionsExt;
        let Some(path) = std::env::var_os("PATH") else {
            return false;
        };
        std::env::split_paths(&path).any(|dir| {
            std::fs::metadata(dir.join(name))
                .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        })
    }
}

// The Linux rules are pure and tested on every platform.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn linux_session(probe: &impl Probe) -> Session {
    // Same test as the Hyprland code paths: the instance signature alone.
    if probe.var("HYPRLAND_INSTANCE_SIGNATURE").is_some() {
        return Session::Hyprland;
    }
    let desktops = probe.var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let desktop_is = |name: &str| {
        desktops
            .split(':')
            .any(|desktop| desktop.trim().eq_ignore_ascii_case(name))
    };
    let wayland = probe.var("WAYLAND_DISPLAY").is_some();
    if probe.var("SWAYSOCK").is_some() || (wayland && desktop_is("sway")) {
        return Session::Sway;
    }
    if wayland {
        return if desktop_is("KDE") {
            Session::KdeWayland
        } else if desktop_is("GNOME") {
            Session::GnomeWayland
        } else {
            Session::OtherWayland
        };
    }
    if probe.var("DISPLAY").is_some() {
        return Session::X11;
    }
    Session::Unknown
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn linux(probe: &impl Probe) -> Capabilities {
    let session = linux_session(probe);
    let wayland = probe.var("WAYLAND_DISPLAY").is_some();
    let hyprland = session == Session::Hyprland;
    let mut missing_programs = Vec::new();
    // Only programs the current code would run in this session are probed.
    let mut needs = |program: &str, needed: bool| {
        let found = needed && probe.has_program(program);
        if needed && !found {
            missing_programs.push(program.to_owned());
        }
        found
    };
    let wl_copy = needs("wl-copy", wayland);
    let wl_paste = needs("wl-paste", wayland);
    let hyprctl = needs("hyprctl", hyprland);
    let wtype = needs("wtype", hyprland);
    Capabilities {
        session,
        clipboard: wl_copy,
        paste_injection: wl_copy && hyprctl && wtype,
        copy_selection: wl_copy && wl_paste && hyprctl,
        primary_selection: wl_paste,
        focus_tracking: hyprctl,
        window_placement: hyprctl,
        global_hotkeys: match session {
            Session::Hyprland => HotkeySupport::Hold,
            Session::Unknown => HotkeySupport::None,
            _ => HotkeySupport::Toggle,
        },
        tray: true,
        compositor_binds: hyprland,
        missing_programs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const ALL: &[&str] = &["wl-copy", "wl-paste", "hyprctl", "wtype"];

    struct Fake {
        vars: HashMap<&'static str, &'static str>,
        programs: &'static [&'static str],
    }
    impl Probe for Fake {
        fn var(&self, name: &str) -> Option<String> {
            self.vars
                .get(name)
                .filter(|value| !value.is_empty())
                .map(|value| (*value).to_owned())
        }
        fn has_program(&self, name: &str) -> bool {
            self.programs.contains(&name)
        }
    }
    fn env(vars: &[(&'static str, &'static str)], programs: &'static [&'static str]) -> Fake {
        Fake {
            vars: vars.iter().copied().collect(),
            programs,
        }
    }
    const HYPRLAND: &[(&str, &str)] = &[
        ("HYPRLAND_INSTANCE_SIGNATURE", "abc_123"),
        ("WAYLAND_DISPLAY", "wayland-1"),
        ("XDG_CURRENT_DESKTOP", "Hyprland"),
        ("DISPLAY", ":0"),
    ];

    #[test]
    fn hyprland_with_its_helpers_supports_everything() {
        let caps = linux(&env(HYPRLAND, ALL));
        assert_eq!(
            caps,
            Capabilities {
                session: Session::Hyprland,
                clipboard: true,
                paste_injection: true,
                copy_selection: true,
                primary_selection: true,
                focus_tracking: true,
                window_placement: true,
                global_hotkeys: HotkeySupport::Hold,
                tray: true,
                compositor_binds: true,
                missing_programs: vec![],
            }
        );
    }

    #[test]
    fn sessions_are_told_apart_from_the_environment() {
        let cases: &[(&[(&str, &str)], Session)] = &[
            (HYPRLAND, Session::Hyprland),
            // The signature wins over whatever the desktop name says.
            (
                &[
                    ("HYPRLAND_INSTANCE_SIGNATURE", "abc"),
                    ("XDG_CURRENT_DESKTOP", "GNOME"),
                ],
                Session::Hyprland,
            ),
            (
                &[("SWAYSOCK", "/run/user/1000/sway-ipc.sock")],
                Session::Sway,
            ),
            (
                &[
                    ("WAYLAND_DISPLAY", "wayland-1"),
                    ("XDG_CURRENT_DESKTOP", "sway"),
                ],
                Session::Sway,
            ),
            (
                &[
                    ("WAYLAND_DISPLAY", "wayland-0"),
                    ("XDG_CURRENT_DESKTOP", "KDE"),
                ],
                Session::KdeWayland,
            ),
            (
                &[
                    ("WAYLAND_DISPLAY", "wayland-0"),
                    ("XDG_CURRENT_DESKTOP", "ubuntu:GNOME"),
                    ("DISPLAY", ":0"),
                ],
                Session::GnomeWayland,
            ),
            (
                &[
                    ("WAYLAND_DISPLAY", "wayland-1"),
                    ("XDG_CURRENT_DESKTOP", "niri"),
                ],
                Session::OtherWayland,
            ),
            // GNOME on Xorg and an empty signature/display are not Wayland.
            (
                &[
                    ("XDG_CURRENT_DESKTOP", "GNOME"),
                    ("DISPLAY", ":1"),
                    ("WAYLAND_DISPLAY", ""),
                    ("HYPRLAND_INSTANCE_SIGNATURE", ""),
                ],
                Session::X11,
            ),
            (&[], Session::Unknown),
        ];
        for (vars, expected) in cases {
            assert_eq!(linux_session(&env(vars, ALL)), *expected, "{vars:?}");
        }
    }

    #[test]
    fn other_sessions_get_only_what_todays_code_supports() {
        for vars in [
            &[("SWAYSOCK", "/tmp/sway"), ("WAYLAND_DISPLAY", "wayland-1")][..],
            &[
                ("WAYLAND_DISPLAY", "wayland-0"),
                ("XDG_CURRENT_DESKTOP", "KDE"),
            ],
            &[
                ("WAYLAND_DISPLAY", "wayland-0"),
                ("XDG_CURRENT_DESKTOP", "GNOME"),
            ],
        ] {
            let caps = linux(&env(vars, ALL));
            assert!(caps.clipboard && caps.primary_selection, "{caps:?}");
            assert!(
                !caps.paste_injection
                    && !caps.copy_selection
                    && !caps.focus_tracking
                    && !caps.window_placement
                    && !caps.compositor_binds,
                "{caps:?}"
            );
            assert_eq!(caps.global_hotkeys, HotkeySupport::Toggle);
            assert!(caps.tray && caps.missing_programs.is_empty());
        }
        let x11 = linux(&env(&[("DISPLAY", ":0")], ALL));
        assert_eq!(x11.session, Session::X11);
        assert!(!x11.clipboard && !x11.primary_selection && x11.tray);
        assert_eq!(x11.global_hotkeys, HotkeySupport::Toggle);
        let headless = linux(&env(&[], ALL));
        assert_eq!(headless.global_hotkeys, HotkeySupport::None);
        assert!(!headless.clipboard);
    }

    #[test]
    fn missing_helpers_turn_off_only_what_needs_them() {
        let caps = linux(&env(HYPRLAND, &["wl-copy", "wl-paste", "hyprctl"]));
        assert!(!caps.paste_injection);
        assert!(caps.clipboard && caps.copy_selection && caps.focus_tracking);
        assert_eq!(caps.missing_programs, vec!["wtype"]);

        let caps = linux(&env(HYPRLAND, &["hyprctl", "wtype"]));
        assert!(!caps.clipboard && !caps.primary_selection);
        assert!(!caps.paste_injection && !caps.copy_selection);
        assert!(caps.focus_tracking && caps.window_placement);
        assert_eq!(caps.missing_programs, vec!["wl-copy", "wl-paste"]);

        // Hyprland's tools are not asked for where they would never run.
        let sway = linux(&env(
            &[("SWAYSOCK", "/tmp/sway"), ("WAYLAND_DISPLAY", "w")],
            &[],
        ));
        assert_eq!(sway.missing_programs, vec!["wl-copy", "wl-paste"]);
        assert!(
            linux(&env(&[("DISPLAY", ":0")], &[]))
                .missing_programs
                .is_empty()
        );
    }

    #[test]
    fn serializes_with_stable_names_and_tolerates_older_payloads() {
        let caps = linux(&env(HYPRLAND, ALL));
        let json = serde_json::to_value(&caps).unwrap();
        assert_eq!(json["session"], "hyprland");
        assert_eq!(json["global_hotkeys"], "hold");
        assert_eq!(
            serde_json::to_value(Session::KdeWayland).unwrap(),
            "kde_wayland"
        );
        assert_eq!(serde_json::to_value(Session::MacOs).unwrap(), "macos");
        let back: Capabilities = serde_json::from_value(json).unwrap();
        assert_eq!(back, caps);
        let partial: Capabilities =
            serde_json::from_str(r#"{"session":"x11","tray":true}"#).unwrap();
        assert_eq!(partial.session, Session::X11);
        assert!(partial.tray && !partial.clipboard);
        assert_eq!(partial.global_hotkeys, HotkeySupport::None);
    }
}
