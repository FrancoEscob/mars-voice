//! "Copiar la selección automáticamente": for compositors where the primary
//! selection is unavailable (Hyprland with `misc:middle_click_paste = false`),
//! copy the active window's selection with its own copy shortcut, read it and
//! put the previous clipboard back.
//!
//! Order: refuse our own windows → snapshot the clipboard → clear it (and check
//! it stayed empty, so old content can never pass for the selection) → send the
//! copy shortcut to that exact window → poll the clipboard as text → restore.
//! Restoring runs after success, timeouts and errors alike. Nothing here logs or
//! reports clipboard content. Linux backend for [`super::copy_selection`].
use super::wl_paste::{Failure, Fetch, Flavor, fetch, read_with};
use super::{CopiedSelection, CopyOptions};
use crate::desktop::{PasteTarget, hyprland};
use anyhow::{Result, anyhow, ensure};
use std::{
    ffi::OsStr,
    time::{Duration, Instant},
};

/// Blocking, at most ≈1.5 s. Copies the active external window's selection
/// and returns it as text. `Ok(None)` = nothing was selected (the copy produced
/// no text). Requires Hyprland on Wayland; otherwise the error suggests
/// «Solo el portapapeles». The previous clipboard is restored as one MIME type
/// (plain text preferred, then PNG, then the first offered); content over
/// 16 MiB cannot be saved, so in that case the copied selection stays in the
/// clipboard, or the clipboard stays empty if nothing was copied (reported in
/// the error).
pub(crate) fn copy_selection(options: CopyOptions<'_>) -> Result<Option<String>> {
    ensure_copy_env(
        std::env::var_os("WAYLAND_DISPLAY").as_deref(),
        std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").as_deref(),
    )?;
    copy_with(&mut LiveDesktop, &options, std::process::id()).map(|copied| copied.text)
}

/// [`copy_selection`] plus the window that received the copy shortcut, so
/// Escritura can paste its result back there.
pub(crate) fn copy_selection_from(options: CopyOptions<'_>) -> Result<CopiedSelection> {
    ensure_copy_env(
        std::env::var_os("WAYLAND_DISPLAY").as_deref(),
        std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").as_deref(),
    )?;
    copy_with(&mut LiveDesktop, &options, std::process::id())
}

/// Ctrl+V would reach a terminal's program as a literal ^V; terminals paste
/// with Ctrl+Shift+V, the counterpart of the copy shortcut sent to them.
/// Any other combination is kept as configured.
pub(crate) fn paste_keys_for<'a>(target: &PasteTarget, keys: &'a str, extra: &[String]) -> &'a str {
    if keys.trim().eq_ignore_ascii_case("CTRL+V") && is_terminal(target, extra) {
        "CTRL+SHIFT+V"
    } else {
        keys
    }
}

/// Everything up to reading the copied text.
const BUDGET: Duration = Duration::from_millis(1100);
/// Restoring has its own short budget so it also runs after a timeout.
const RESTORE_TIMEOUT: Duration = Duration::from_millis(400);
const STEP_TIMEOUT: Duration = Duration::from_millis(300);
const POLL_WINDOW: Duration = Duration::from_millis(700);
const POLL_INTERVAL: Duration = Duration::from_millis(25);
const MIN_READ: Duration = Duration::from_millis(100);
const SNAPSHOT_LIMIT: usize = 16 << 20;
const TYPES_LIMIT: usize = 64 << 10;

/// Matched case-insensitively against class and initial class. Any class
/// containing "term" also counts: Ctrl+C would interrupt a terminal's program,
/// while Ctrl+Shift+C in a non-terminal is at worst a no-op or a panel.
const TERMINAL_CLASSES: &[&str] = &[
    "com.mitchellh.ghostty",
    "ghostty",
    "kitty",
    "Alacritty",
    "foot",
    "footclient",
    "org.wezfurlong.wezterm",
    "wezterm",
    "org.kde.konsole",
    "konsole",
    "org.gnome.Terminal",
    "org.gnome.Ptyxis",
    "org.gnome.Console",
    "kgx",
    "xfce4-terminal",
    "tilix",
    "terminator",
    "Termite",
    "st",
    "st-256color",
    "urxvt",
    "URxvt",
    "XTerm",
    "contour",
    "rio",
    "dev.warp.Warp",
    "com.raggesilver.BlackBox",
    "cosmic-term",
    "qterminal",
    "lxterminal",
    "sakura",
    "terminology",
    "yakuake",
    "guake",
];

const ENV_ERROR: &str = "Copiar la selección automáticamente requiere Hyprland en Wayland. Elegí «Solo el portapapeles» en Ajustes > Lectura y copiá el texto antes de leer";
const OWN_WINDOW: &str = "Seleccioná el texto en otra ventana y repetí el atajo de lectura";
const NO_STABLE_ID: &str = "Hyprland no identifica la ventana activa de forma segura. Actualizá Hyprland o elegí «Solo el portapapeles» en Ajustes > Lectura";
const CLEAR_FAILED: &str =
    "No se pudo vaciar el portapapeles antes de copiar; comprobá wl-clipboard";
const REFILLED: &str = "Otro programa volvió a llenar el portapapeles apenas se vació, así que no se puede distinguir la selección. Desactivá esa función del gestor de portapapeles o elegí «Solo el portapapeles» en Ajustes > Lectura";
const DISPATCH_FAILED: &str = "Hyprland no pudo enviar el atajo de copiar a la ventana activa. Volvé a intentarlo o elegí «Solo el portapapeles» en Ajustes > Lectura";
const TOO_LARGE_LOST: &str =
    "El portapapeles anterior era demasiado grande para guardarlo y quedó vacío";
const NOT_RESTORED: &str = "No se pudo restaurar el portapapeles anterior";

fn ensure_copy_env(wayland: Option<&OsStr>, hyprland: Option<&OsStr>) -> Result<()> {
    let set = |v: Option<&OsStr>| v.is_some_and(|v| !v.is_empty());
    ensure!(set(wayland) && set(hyprland), ENV_ERROR);
    Ok(())
}

/// Desktop operations used by the copy flow; faked in tests.
trait CopyDesktop {
    fn now(&self) -> Instant;
    fn sleep(&mut self, duration: Duration);
    fn active(&mut self) -> Result<PasteTarget>;
    /// Offered MIME types; `None` when the clipboard is empty.
    fn list_types(&mut self, timeout: Duration) -> Result<Option<Vec<String>>, Failure>;
    fn read_bytes(&mut self, mime: &str, max: usize, timeout: Duration) -> Result<Fetch, Failure>;
    fn clear(&mut self) -> Result<()>;
    fn write(&mut self, mime: &str, bytes: &[u8]) -> Result<()>;
    /// Runs one `hyprctl dispatch` Lua expression; Ok only when Hyprland says "ok".
    fn dispatch(&mut self, lua: &str) -> Result<()>;
    fn read_text(&mut self, max_bytes: usize, timeout: Duration) -> Result<Option<String>>;
}

struct LiveDesktop;
impl CopyDesktop for LiveDesktop {
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn sleep(&mut self, duration: Duration) {
        std::thread::sleep(duration);
    }
    fn active(&mut self) -> Result<PasteTarget> {
        hyprland::active_window()
    }
    fn list_types(&mut self, timeout: Duration) -> Result<Option<Vec<String>>, Failure> {
        let deadline = Instant::now() + timeout;
        match fetch(
            OsStr::new("wl-paste"),
            &["--list-types"],
            TYPES_LIMIT,
            deadline,
        )? {
            Fetch::Bytes(bytes) => Ok(parse_types(&bytes)),
            Fetch::Empty | Fetch::MissingType => Ok(None),
            Fetch::TooLarge => Err(Failure::Failed),
        }
    }
    fn read_bytes(&mut self, mime: &str, max: usize, timeout: Duration) -> Result<Fetch, Failure> {
        let deadline = Instant::now() + timeout;
        fetch(
            OsStr::new("wl-paste"),
            &["--no-newline", "--type", mime],
            max,
            deadline,
        )
    }
    fn clear(&mut self) -> Result<()> {
        hyprland::run_timeout("wl-copy", &["--clear"], None, STEP_TIMEOUT).map(drop)
    }
    fn write(&mut self, mime: &str, bytes: &[u8]) -> Result<()> {
        // Content goes through stdin, never through arguments.
        hyprland::run_timeout("wl-copy", &["--type", mime], Some(bytes), RESTORE_TIMEOUT).map(drop)
    }
    fn dispatch(&mut self, lua: &str) -> Result<()> {
        let output = hyprland::run_timeout("hyprctl", &["dispatch", lua], None, STEP_TIMEOUT)?;
        ensure!(
            String::from_utf8_lossy(&output).trim() == "ok",
            DISPATCH_FAILED
        );
        Ok(())
    }
    fn read_text(&mut self, max_bytes: usize, timeout: Duration) -> Result<Option<String>> {
        read_with(OsStr::new("wl-paste"), Flavor::Copied, max_bytes, timeout)
    }
}

fn parse_types(bytes: &[u8]) -> Option<Vec<String>> {
    let types: Vec<String> = String::from_utf8_lossy(bytes)
        .lines()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
        .collect();
    (!types.is_empty()).then_some(types)
}

/// Types come from other apps and become a wl-copy/wl-paste argument.
fn valid_mime(mime: &str) -> bool {
    !mime.is_empty()
        && mime.len() <= 255
        && !mime.starts_with('-')
        && mime.chars().all(|c| c.is_ascii_graphic() || c == ' ')
}

/// One type to save: plain text first, then PNG, then whatever comes first.
fn pick_mime(types: &[String]) -> Option<String> {
    let valid: Vec<&str> = types
        .iter()
        .map(|t| t.trim())
        .filter(|t| valid_mime(t))
        .collect();
    ["text/plain;charset=utf-8", "text/plain", "image/png"]
        .into_iter()
        .find(|preferred| valid.contains(preferred))
        .or_else(|| valid.first().copied())
        .map(str::to_owned)
}

fn is_terminal(target: &PasteTarget, extra: &[String]) -> bool {
    [target.class.trim(), target.initial_class.trim()]
        .into_iter()
        .filter(|class| !class.is_empty())
        .any(|class| {
            TERMINAL_CLASSES
                .iter()
                .any(|t| t.eq_ignore_ascii_case(class))
                || extra.iter().any(|t| t.trim().eq_ignore_ascii_case(class))
                || class.to_ascii_lowercase().contains("term")
        })
}

/// Hyprland 0.56 Lua dispatch. Only a validated stable id (ASCII alphanumerics,
/// `-`, `_`) is interpolated; mods and key are constants.
fn copy_shortcut(target: &PasteTarget, own_pid: u32, extra: &[String]) -> Result<String> {
    ensure!(!hyprland::is_own_window(target, own_pid), OWN_WINDOW);
    let id = hyprland::safe_stable_id(target).map_err(|_| anyhow!(NO_STABLE_ID))?;
    let mods = if is_terminal(target, extra) {
        "CTRL SHIFT"
    } else {
        "CTRL"
    };
    Ok(format!(
        "hl.dsp.send_shortcut({{ mods = \"{mods}\", key = \"c\", window = \"stableid:{id}\" }})"
    ))
}

enum Saved {
    Empty,
    Data {
        mime: String,
        bytes: Vec<u8>,
    },
    /// Too large or vanished while reading: nothing to put back.
    Unrestorable,
}

fn left(desk: &impl CopyDesktop, deadline: Instant) -> Duration {
    deadline
        .saturating_duration_since(desk.now())
        .max(Duration::from_millis(1))
}

fn snapshot_error(failure: Failure) -> anyhow::Error {
    match failure {
        Failure::NotFound => {
            anyhow!("Falta wl-paste. Instalá wl-clipboard para copiar la selección")
        }
        Failure::Timeout => anyhow!(
            "La app dueña del portapapeles no respondió y no se tocó el portapapeles. Volvé a intentarlo"
        ),
        Failure::NoPrimary | Failure::Failed => anyhow!(
            "No se pudo guardar el portapapeles antes de copiar, así que no se tocó. Comprobá wl-clipboard y volvé a intentarlo"
        ),
    }
}

fn snapshot(desk: &mut impl CopyDesktop, deadline: Instant) -> Result<Saved> {
    let types = match desk
        .list_types(left(desk, deadline))
        .map_err(snapshot_error)?
    {
        None => return Ok(Saved::Empty),
        Some(types) => types,
    };
    let Some(mime) = pick_mime(&types) else {
        return Ok(Saved::Unrestorable);
    };
    let timeout = left(desk, deadline);
    Ok(
        match desk
            .read_bytes(&mime, SNAPSHOT_LIMIT, timeout)
            .map_err(snapshot_error)?
        {
            Fetch::Bytes(bytes) => Saved::Data { mime, bytes },
            Fetch::Empty => Saved::Empty,
            Fetch::TooLarge | Fetch::MissingType => Saved::Unrestorable,
        },
    )
}

fn copy_and_read(
    desk: &mut impl CopyDesktop,
    shortcut: &str,
    max_bytes: usize,
    deadline: Instant,
) -> Result<Option<String>> {
    desk.clear().map_err(|_| anyhow!(CLEAR_FAILED))?;
    // A clipboard manager that re-offers old content would make it look copied.
    if desk
        .list_types(left(desk, deadline))
        .map_err(|_| anyhow!(CLEAR_FAILED))?
        .is_some()
    {
        return Err(anyhow!(REFILLED));
    }
    desk.dispatch(shortcut)
        .map_err(|_| anyhow!(DISPATCH_FAILED))?;
    let poll_end = (desk.now() + POLL_WINDOW).min(deadline);
    loop {
        desk.sleep(POLL_INTERVAL);
        let timeout = left(desk, deadline).max(MIN_READ);
        if let Some(text) = desk.read_text(max_bytes, timeout)? {
            return Ok(Some(text));
        }
        if desk.now() >= poll_end {
            return Ok(None);
        }
    }
}

fn restore(desk: &mut impl CopyDesktop, saved: &Saved) -> Result<()> {
    match saved {
        Saved::Empty => desk.clear(),
        Saved::Data { mime, bytes } => desk.write(mime, bytes),
        Saved::Unrestorable => Ok(()),
    }
}

fn copy_with(
    desk: &mut impl CopyDesktop,
    options: &CopyOptions<'_>,
    own_pid: u32,
) -> Result<CopiedSelection> {
    let deadline = desk.now() + BUDGET;
    let target = desk.active()?;
    // Validated before the clipboard is touched.
    let shortcut = copy_shortcut(&target, own_pid, options.extra_terminal_classes)?;
    let saved = if options.restore_clipboard {
        Some(snapshot(desk, deadline)?)
    } else {
        None
    };
    let copied = copy_and_read(desk, &shortcut, options.max_bytes, deadline);
    let text = match saved {
        None => copied,
        Some(Saved::Unrestorable) => match copied {
            Ok(Some(text)) => Ok(Some(text)),
            Ok(None) => Err(anyhow!("No había texto seleccionado. {TOO_LARGE_LOST}")),
            Err(error) => Err(anyhow!("{error}. {TOO_LARGE_LOST}")),
        },
        Some(saved) => match (copied, restore(desk, &saved)) {
            (Ok(text), Ok(())) => Ok(text),
            // The copied selection stays in the clipboard, like a manual copy.
            (Ok(Some(text)), Err(_)) => Ok(Some(text)),
            (Ok(None), Err(_)) => Err(anyhow!("No había texto seleccionado. {NOT_RESTORED}")),
            (Err(error), Ok(())) => Err(error),
            (Err(error), Err(_)) => Err(anyhow!("{error}. {NOT_RESTORED}")),
        },
    }?;
    Ok(CopiedSelection {
        text,
        source: target,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const UTF8: &str = "text/plain;charset=utf-8";
    const OWN_PID: u32 = 77;

    fn window(class: &str) -> PasteTarget {
        PasteTarget {
            address: "0x5a5a".into(),
            pid: 4242,
            class: class.into(),
            initial_class: class.into(),
            stable_id: Some("1f2e3d".into()),
        }
    }

    /// In-memory clipboard and compositor. Records every operation in order.
    struct Fake {
        target: PasteTarget,
        clipboard: Vec<(String, Vec<u8>)>,
        /// What the copy shortcut puts in the clipboard, and after how many polls.
        selection: Option<&'static str>,
        appears_after: usize,
        dispatch_fails: bool,
        clear_ignored: bool,
        snapshot_too_large: bool,
        pending: bool,
        polls: usize,
        clock: Instant,
        actions: Vec<String>,
        dispatched: Vec<String>,
    }
    impl Fake {
        fn new(clipboard: &[(&str, &[u8])], selection: Option<&'static str>) -> Self {
            Self {
                target: window("firefox"),
                clipboard: clipboard
                    .iter()
                    .map(|(mime, bytes)| ((*mime).to_owned(), bytes.to_vec()))
                    .collect(),
                selection,
                appears_after: 2,
                dispatch_fails: false,
                clear_ignored: false,
                snapshot_too_large: false,
                pending: false,
                polls: 0,
                clock: Instant::now(),
                actions: vec![],
                dispatched: vec![],
            }
        }
        fn run(&mut self, restore: bool, extra: &[String]) -> Result<Option<String>> {
            let options = CopyOptions {
                max_bytes: 1024,
                restore_clipboard: restore,
                extra_terminal_classes: extra,
            };
            copy_with(self, &options, OWN_PID).map(|copied| copied.text)
        }
        /// Actions with consecutive polls collapsed, for readable assertions.
        fn steps(&self) -> Vec<&str> {
            let mut steps: Vec<&str> = Vec::new();
            for action in &self.actions {
                if steps.last() != Some(&action.as_str()) || action != "poll" {
                    steps.push(action);
                }
            }
            steps
        }
        fn text(&self) -> Option<String> {
            self.clipboard
                .iter()
                .find(|(mime, _)| mime.starts_with("text/plain"))
                .map(|(_, bytes)| String::from_utf8(bytes.clone()).unwrap())
        }
    }
    impl CopyDesktop for Fake {
        fn now(&self) -> Instant {
            self.clock
        }
        fn sleep(&mut self, duration: Duration) {
            self.clock += duration;
        }
        fn active(&mut self) -> Result<PasteTarget> {
            self.actions.push("active".into());
            Ok(self.target.clone())
        }
        fn list_types(&mut self, _: Duration) -> Result<Option<Vec<String>>, Failure> {
            self.actions.push("list".into());
            let types: Vec<String> = self.clipboard.iter().map(|(m, _)| m.clone()).collect();
            Ok((!types.is_empty()).then_some(types))
        }
        fn read_bytes(&mut self, mime: &str, _: usize, _: Duration) -> Result<Fetch, Failure> {
            self.actions.push(format!("save {mime}"));
            if self.snapshot_too_large {
                return Ok(Fetch::TooLarge);
            }
            Ok(self
                .clipboard
                .iter()
                .find(|(m, _)| m == mime)
                .map_or(Fetch::MissingType, |(_, b)| Fetch::Bytes(b.clone())))
        }
        fn clear(&mut self) -> Result<()> {
            self.actions.push("clear".into());
            if !self.clear_ignored {
                self.clipboard.clear();
            }
            Ok(())
        }
        fn write(&mut self, mime: &str, bytes: &[u8]) -> Result<()> {
            self.actions.push(format!("restore {mime}"));
            self.clipboard = vec![(mime.to_owned(), bytes.to_vec())];
            Ok(())
        }
        fn dispatch(&mut self, lua: &str) -> Result<()> {
            self.actions.push("dispatch".into());
            self.dispatched.push(lua.to_owned());
            ensure!(!self.dispatch_fails, "sendshortcut: window not found");
            self.pending = self.selection.is_some();
            Ok(())
        }
        fn read_text(&mut self, _: usize, _: Duration) -> Result<Option<String>> {
            self.actions.push("poll".into());
            self.polls += 1;
            if self.pending && self.polls > self.appears_after {
                self.pending = false;
                let text = self.selection.unwrap();
                self.clipboard = vec![(UTF8.into(), text.as_bytes().to_vec())];
            }
            Ok(self.text())
        }
    }

    #[test]
    fn snapshot_clear_dispatch_poll_restore_in_that_order() {
        let mut fake = Fake::new(
            &[("image/png", b"\x89PNG"), (UTF8, b"viejo")],
            Some("nuevo"),
        );
        assert_eq!(fake.run(true, &[]).unwrap(), Some("nuevo".into()));
        assert_eq!(
            fake.steps(),
            vec![
                "active",
                "list",
                &format!("save {UTF8}"),
                "clear",
                "list",
                "dispatch",
                "poll",
                &format!("restore {UTF8}"),
            ]
        );
        assert_eq!(fake.polls, 3);
        assert_eq!(fake.clipboard, vec![(UTF8.to_owned(), b"viejo".to_vec())]);
        assert_eq!(
            fake.dispatched,
            vec![
                r#"hl.dsp.send_shortcut({ mods = "CTRL", key = "c", window = "stableid:1f2e3d" })"#
            ]
        );
    }

    #[test]
    fn terminals_get_ctrl_shift_by_class_initial_class_or_extra_list() {
        let mods = |target: PasteTarget, extra: &[String]| {
            let lua = copy_shortcut(&target, OWN_PID, extra).unwrap();
            lua.split('"').nth(1).unwrap().to_owned()
        };
        for class in [
            "kitty",
            "COM.MITCHELLH.GHOSTTY",
            "alacritty",
            "org.wezfurlong.wezterm",
            "st-256color",
            "my-custom-terminal",
        ] {
            assert_eq!(mods(window(class), &[]), "CTRL SHIFT", "{class}");
        }
        let mut renamed = window("scratchpad");
        renamed.initial_class = "foot".into();
        assert_eq!(mods(renamed, &[]), "CTRL SHIFT");
        let extra = vec![" Zellij-Box ".to_owned()];
        assert_eq!(mods(window("zellij-box"), &extra), "CTRL SHIFT");
        for class in ["firefox", "org.gnome.TextEditor", "code", "stemma"] {
            assert_eq!(mods(window(class), &extra), "CTRL", "{class}");
        }
        let mut fake = Fake::new(&[], Some("ls -la"));
        fake.target = window("kitty");
        assert_eq!(fake.run(true, &[]).unwrap(), Some("ls -la".into()));
        assert!(fake.dispatched[0].contains(r#"mods = "CTRL SHIFT", key = "c""#));
    }

    #[test]
    fn own_windows_are_refused_before_touching_the_clipboard() {
        let mut cases = Vec::new();
        let mut same_pid = window("firefox");
        same_pid.pid = i64::from(OWN_PID);
        cases.push(same_pid);
        for class in [
            "dicta",
            "dicta-overlay",
            "dicta-reader",
            crate::desktop::APP_ID,
        ] {
            cases.push(window(class));
        }
        let mut renamed = window("other");
        renamed.initial_class = "dicta-reader".into();
        cases.push(renamed);
        for target in cases {
            let mut fake = Fake::new(&[(UTF8, b"viejo")], Some("nuevo"));
            fake.target = target;
            let error = fake.run(true, &[]).unwrap_err().to_string();
            assert!(error.contains("otra ventana"), "{error}");
            assert_eq!(fake.actions, vec!["active"]);
            assert_eq!(fake.text().as_deref(), Some("viejo"));
        }
    }

    #[test]
    fn restores_after_success_timeout_and_dispatch_error() {
        // Success: covered with the full order above. Timeout: nothing copied.
        let mut fake = Fake::new(&[(UTF8, b"viejo")], None);
        assert_eq!(fake.run(true, &[]).unwrap(), None);
        assert_eq!(fake.actions.last().unwrap(), &format!("restore {UTF8}"));
        assert_eq!(fake.text().as_deref(), Some("viejo"));
        // The poll gives up after ≈700 ms and the whole call stays bounded.
        assert!(fake.polls >= 25 && fake.polls <= 30, "{}", fake.polls);
        // Dispatch error: actionable, still restored, never polled.
        let mut fake = Fake::new(&[(UTF8, b"viejo")], Some("nuevo"));
        fake.dispatch_fails = true;
        let error = fake.run(true, &[]).unwrap_err().to_string();
        assert!(error.contains("Solo el portapapeles"), "{error}");
        assert!(!error.contains("window not found"));
        assert!(!fake.actions.contains(&"poll".to_owned()));
        assert_eq!(fake.actions.last().unwrap(), &format!("restore {UTF8}"));
        assert_eq!(fake.text().as_deref(), Some("viejo"));
    }

    #[test]
    fn empty_clipboard_is_restored_by_clearing_it() {
        let mut fake = Fake::new(&[], Some("nuevo"));
        assert_eq!(fake.run(true, &[]).unwrap(), Some("nuevo".into()));
        assert_eq!(fake.actions.last().unwrap(), "clear");
        assert!(fake.clipboard.is_empty());
        assert!(!fake.actions.iter().any(|a| a.starts_with("save")));
    }

    #[test]
    fn old_content_is_never_returned_as_the_selection() {
        // Nothing selected: the cleared clipboard stays empty → None.
        let mut fake = Fake::new(&[(UTF8, b"https://enlace.example")], None);
        assert_eq!(fake.run(true, &[]).unwrap(), None);
        // A manager that refills the clipboard: refuse before sending keys.
        let mut fake = Fake::new(&[(UTF8, b"https://enlace.example")], Some("nuevo"));
        fake.clear_ignored = true;
        let error = fake.run(true, &[]).unwrap_err().to_string();
        assert!(error.contains("volvió a llenar"), "{error}");
        assert!(!error.contains("enlace"));
        assert!(fake.dispatched.is_empty());
        assert_eq!(fake.text().as_deref(), Some("https://enlace.example"));
    }

    #[test]
    fn without_restore_the_copied_text_stays_and_nothing_is_saved() {
        let mut fake = Fake::new(&[(UTF8, b"viejo")], Some("nuevo"));
        assert_eq!(fake.run(false, &[]).unwrap(), Some("nuevo".into()));
        assert_eq!(
            fake.steps(),
            vec!["active", "clear", "list", "dispatch", "poll"]
        );
        assert_eq!(fake.text().as_deref(), Some("nuevo"));
    }

    #[test]
    fn oversized_previous_clipboard_is_not_restored_and_is_reported_on_failure() {
        let mut fake = Fake::new(&[("image/png", b"huge")], Some("nuevo"));
        fake.snapshot_too_large = true;
        assert_eq!(fake.run(true, &[]).unwrap(), Some("nuevo".into()));
        assert!(!fake.actions.iter().any(|a| a.starts_with("restore")));
        assert_eq!(fake.text().as_deref(), Some("nuevo"));
        let mut fake = Fake::new(&[("image/png", b"huge")], None);
        fake.snapshot_too_large = true;
        let error = fake.run(true, &[]).unwrap_err().to_string();
        assert!(error.contains("demasiado grande"), "{error}");
    }

    #[test]
    fn dispatch_interpolates_only_a_validated_stable_id() {
        assert_eq!(
            copy_shortcut(&window("firefox"), OWN_PID, &[]).unwrap(),
            r#"hl.dsp.send_shortcut({ mods = "CTRL", key = "c", window = "stableid:1f2e3d" })"#
        );
        for id in [
            Some("1f\" }) os.execute(\"rm\") --"),
            Some("a b"),
            Some(""),
            None,
        ] {
            let mut fake = Fake::new(&[(UTF8, b"viejo")], Some("nuevo"));
            fake.target.stable_id = id.map(str::to_owned);
            let error = fake.run(true, &[]).unwrap_err().to_string();
            assert!(error.contains("Hyprland"), "{error}");
            assert_eq!(fake.actions, vec!["active"]);
            assert!(fake.dispatched.is_empty());
        }
        // Class and title never reach the Lua string.
        let lua = copy_shortcut(&window("evil\" }) x(\""), OWN_PID, &[]).unwrap();
        assert!(!lua.contains("evil"));
    }

    #[test]
    fn saves_plain_text_then_png_then_the_first_safe_type() {
        let types = |list: &[&str]| list.iter().map(|t| (*t).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            pick_mime(&types(&["image/png", "text/plain", UTF8])).as_deref(),
            Some(UTF8)
        );
        assert_eq!(
            pick_mime(&types(&["image/png", "text/plain"])).as_deref(),
            Some("text/plain")
        );
        assert_eq!(
            pick_mime(&types(&["application/x-foo", "image/png"])).as_deref(),
            Some("image/png")
        );
        assert_eq!(
            pick_mime(&types(&["--clear", "application/x-foo"])).as_deref(),
            Some("application/x-foo")
        );
        assert_eq!(pick_mime(&types(&["-x", "bad\u{7}type"])), None);
        assert_eq!(
            parse_types(b"text/plain\nimage/png\n\n"),
            Some(types(&["text/plain", "image/png"]))
        );
        assert_eq!(parse_types(b"\n"), None);
    }

    #[test]
    fn requires_hyprland_on_wayland_and_suggests_clipboard_only() {
        let wayland = Some(OsStr::new("wayland-1"));
        let hyprland = Some(OsStr::new("abc_123"));
        ensure_copy_env(wayland, hyprland).unwrap();
        for (w, h) in [
            (None, hyprland),
            (wayland, None),
            (Some(OsStr::new("")), hyprland),
        ] {
            let error = ensure_copy_env(w, h).unwrap_err().to_string();
            assert!(error.contains("Solo el portapapeles"), "{error}");
        }
    }

    #[test]
    fn copy_reports_the_source_window_for_a_later_replace() {
        let mut fake = Fake::new(&[(UTF8, b"viejo")], Some("nuevo"));
        let options = CopyOptions {
            max_bytes: 1024,
            restore_clipboard: true,
            extra_terminal_classes: &[],
        };
        let copied = copy_with(&mut fake, &options, OWN_PID).unwrap();
        assert_eq!(copied.text.as_deref(), Some("nuevo"));
        assert_eq!(copied.source, window("firefox"));
        // Nothing selected still names the window; our own window never does.
        let mut empty = Fake::new(&[], None);
        assert_eq!(copy_with(&mut empty, &options, OWN_PID).unwrap().text, None);
        let mut own = Fake::new(&[], Some("x"));
        own.target.pid = i64::from(OWN_PID);
        assert!(copy_with(&mut own, &options, OWN_PID).is_err());
    }

    #[test]
    fn terminals_paste_with_ctrl_shift_v_and_other_keys_are_kept() {
        let extra = vec!["mi-terminal".to_owned()];
        assert_eq!(
            paste_keys_for(&window("kitty"), "CTRL+V", &[]),
            "CTRL+SHIFT+V"
        );
        assert_eq!(
            paste_keys_for(&window("mi-terminal"), "ctrl+v", &extra),
            "CTRL+SHIFT+V"
        );
        assert_eq!(paste_keys_for(&window("firefox"), "CTRL+V", &[]), "CTRL+V");
        assert_eq!(
            paste_keys_for(&window("kitty"), "SHIFT+INSERT", &[]),
            "SHIFT+INSERT"
        );
    }
}
