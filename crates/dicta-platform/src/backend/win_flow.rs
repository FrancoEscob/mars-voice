//! Windows desktop flows, written against [`Desktop`] instead of Win32 so they
//! are tested on every system: the paste target from the foreground window,
//! pasting with identity and focus checks, and copying the selection by
//! watching the clipboard sequence number. `backend/windows.rs` implements
//! [`Desktop`] with Win32 and arboard.
//!
//! Only explicit user actions get here. Nothing logs or reports clipboard
//! content, and no key sequence contains Enter.
use crate::desktop::{PastePlan, PasteTarget};
use crate::selection::{CopyOptions, READ_TIMEOUT, SelectionSource};
use anyhow::{Context, Result, anyhow, bail, ensure};
use std::time::{Duration, Instant};

/// How long to wait for the user to release the shortcut's modifiers before
/// typing, so a held Alt never turns Ctrl+V into Ctrl+Alt+V.
const MODIFIER_WAIT: Duration = Duration::from_millis(1000);
/// Windows may take a moment to activate the window it was asked to.
const ACTIVATE_WAIT: Duration = Duration::from_millis(500);
/// How long the source app has to change the clipboard after the copy keys.
const POLL_WINDOW: Duration = Duration::from_millis(700);
/// After the change, how long the source app may keep writing the clipboard.
const READ_WINDOW: Duration = Duration::from_millis(300);
const POLL_INTERVAL: Duration = Duration::from_millis(20);
/// Same limit as the Wayland clipboard.
const COPY_LIMIT: usize = 1024 * 1024;

const NO_WINDOW: &str =
    "No hay una ventana activa; hacé clic en el campo de destino y repetí el atajo";
const OWN_PASTE: &str =
    "Seleccioná el campo de destino fuera de Mars voice y repetí el atajo de pegar";
const OWN_COPY: &str = "Seleccioná el texto en otra ventana y repetí el atajo de lectura";
const FOCUS_CHANGED: &str =
    "Cambió la ventana activa; el texto quedó copiado. Volvé al destino y repetí el atajo de pegar";
const HELD_PASTE: &str = "El texto quedó copiado, pero las teclas del atajo seguían pulsadas. Soltalas y pegá con Ctrl+V";
const HELD_COPY: &str = "Soltá las teclas del atajo de lectura y volvé a intentarlo";
const CANCELLED: &str = "Pegado cancelado";
const TARGET_GONE: &str =
    "El destino original ya no está abierto o cambió de identidad; el texto quedó copiado";
const NOT_ACTIVATED: &str = "Windows no permitió volver a la ventana de destino; el texto quedó copiado. Hacé clic en el destino y pegá con Ctrl+V";
const COPY_FOCUS_CHANGED: &str =
    "Cambió la ventana activa antes de copiar; seleccioná el texto y repetí el atajo de lectura";
const BUSY: &str = "Otra aplicación está usando el portapapeles; volvé a intentarlo";
const NOT_RESTORED: &str = "No se pudo restaurar el portapapeles anterior";
const LOST: &str =
    "El portapapeles anterior tenía un formato que no se puede guardar y quedó reemplazado";
const SEND_PASTE: &str = "El texto quedó copiado, pero Windows no permitió escribir en la ventana activa (por ejemplo, una app abierta como administrador). Pegá con Ctrl+V";
const SEND_COPY: &str =
    "Windows no permitió enviar Ctrl+C a la ventana activa. Copiá el texto y usá Leer portapapeles";
const NO_PRIMARY: &str = "Windows no tiene selección primaria. Elegí «Copiar la selección automáticamente» o «Solo el portapapeles» en Ajustes > Lectura";

/// Key combinations this backend ever types.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Keys {
    CtrlV,
    CtrlShiftV,
    ShiftInsert,
    CtrlC,
    CtrlShiftC,
}

/// The physical keys behind [`Keys`]. There is no Enter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Vk {
    Ctrl,
    Shift,
    V,
    C,
    Insert,
}

impl Keys {
    /// Modifiers down, key down and up, modifiers up in reverse order. The
    /// `bool` is true for a key press.
    pub(crate) fn sequence(self) -> Vec<(Vk, bool)> {
        let (modifiers, key): (&[Vk], Vk) = match self {
            Self::CtrlV => (&[Vk::Ctrl], Vk::V),
            Self::CtrlShiftV => (&[Vk::Ctrl, Vk::Shift], Vk::V),
            Self::ShiftInsert => (&[Vk::Shift], Vk::Insert),
            Self::CtrlC => (&[Vk::Ctrl], Vk::C),
            Self::CtrlShiftC => (&[Vk::Ctrl, Vk::Shift], Vk::C),
        };
        let mut sequence: Vec<(Vk, bool)> = modifiers.iter().map(|&m| (m, true)).collect();
        sequence.push((key, true));
        sequence.push((key, false));
        sequence.extend(modifiers.iter().rev().map(|&m| (m, false)));
        sequence
    }
}

/// `paste_keys` from the config; the same choices as on Hyprland.
pub(crate) fn paste_keys(keys: &str) -> Result<Keys> {
    match keys.trim().to_ascii_uppercase().as_str() {
        "CTRL+V" => Ok(Keys::CtrlV),
        "CTRL+SHIFT+V" => Ok(Keys::CtrlShiftV),
        "SHIFT+INSERT" => Ok(Keys::ShiftInsert),
        _ => bail!("Usá CTRL+V, CTRL+SHIFT+V o SHIFT+INSERT como combinación de pegado"),
    }
}

/// The paste target for a top-level window: the handle (hex) as address and
/// stable id, the owning process, the window class (`GetClassNameW`) and the
/// program's file name as initial class (`notepad.exe`).
pub(crate) fn target_from(
    hwnd: usize,
    pid: u32,
    class: &str,
    exe_path: Option<&str>,
) -> Result<PasteTarget> {
    ensure!(hwnd != 0 && pid != 0, NO_WINDOW);
    let address = format!("0x{hwnd:x}");
    Ok(PasteTarget {
        address: address.clone(),
        pid: i64::from(pid),
        class: class.to_owned(),
        initial_class: exe_path
            .and_then(|path| path.rsplit(['\\', '/']).next())
            .unwrap_or_default()
            .to_owned(),
        stable_id: Some(address),
    })
}

/// The window handle stored by [`target_from`].
pub(crate) fn hwnd_of(target: &PasteTarget) -> Option<usize> {
    target
        .address
        .strip_prefix("0x")
        .and_then(|hex| usize::from_str_radix(hex, 16).ok())
        .filter(|&hwnd| hwnd != 0)
}

/// Window classes where Ctrl+C would interrupt the running program: Windows
/// Terminal, the classic console, mintty (Git Bash, Cygwin) and PuTTY.
const TERMINAL_CLASSES: &[&str] = &[
    "CASCADIA_HOSTING_WINDOW_CLASS",
    "ConsoleWindowClass",
    "mintty",
    "PuTTY",
];
/// Terminals whose window class is generic (winit, Qt…), by program name.
const TERMINAL_PROGRAMS: &[&str] = &[
    "WindowsTerminal",
    "alacritty",
    "wezterm-gui",
    "mintty",
    "putty",
    "kitty",
    "ConEmu",
    "ConEmu64",
];

/// Terminals get Ctrl+Shift+C. Unlike Linux there is no "term" substring
/// rule: in a browser Ctrl+Shift+C opens the developer tools, so only known
/// classes and programs (and the user's extra classes, matched against the
/// class, the program or the program without `.exe`) count.
pub(crate) fn is_terminal(target: &PasteTarget, extra: &[String]) -> bool {
    let class = target.class.trim();
    let program = target.initial_class.trim();
    let stem = program
        .len()
        .checked_sub(4)
        .filter(|&end| program.is_char_boundary(end))
        .filter(|&end| program[end..].eq_ignore_ascii_case(".exe"))
        .map_or(program, |end| &program[..end]);
    let is = |name: &str, value: &str| !value.is_empty() && name.eq_ignore_ascii_case(value);
    TERMINAL_CLASSES.iter().any(|name| is(name, class))
        || TERMINAL_PROGRAMS.iter().any(|name| is(name, stem))
        || extra.iter().map(|name| name.trim()).any(|name| {
            !name.is_empty() && (is(name, class) || is(name, program) || is(name, stem))
        })
}

/// What reading the clipboard as text found.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ClipText {
    Text(String),
    /// Empty, or no text format offered.
    NoText,
    /// Another program has the clipboard open.
    Busy,
    TooLarge,
}

/// The previous clipboard, kept to put it back after copying the selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Saved<I> {
    Empty,
    Text(String),
    Image(I),
    /// Something else (files, rich formats only, too large): cannot be saved.
    Unrestorable,
}

/// The Windows services the flows use; faked in tests.
pub(crate) trait Desktop {
    type Image;
    fn now(&self) -> Instant;
    fn sleep(&mut self, duration: Duration);
    fn own_pid(&self) -> u32;
    /// The foreground window, which may be one of ours.
    fn foreground(&mut self) -> Result<PasteTarget>;
    /// The window still exists with the same handle, process, class and program.
    fn is_alive(&mut self, target: &PasteTarget) -> bool;
    /// Ask Windows to bring `target` to the front. Best effort: Windows only
    /// allows it while this process owns the foreground.
    fn activate(&mut self, target: &PasteTarget);
    /// Ctrl, Alt, Shift or Win is physically held.
    fn modifiers_down(&mut self) -> bool;
    fn send(&mut self, keys: Keys) -> Result<()>;
    /// `GetClipboardSequenceNumber`: changes whenever the clipboard does.
    fn sequence(&mut self) -> u32;
    fn set_text(&mut self, text: &str) -> Result<()>;
    fn read_text(&mut self, max_bytes: usize) -> ClipText;
    fn save(&mut self) -> Result<Saved<Self::Image>>;
    fn restore(&mut self, saved: &Saved<Self::Image>) -> Result<()>;
}

fn own(desk: &impl Desktop) -> i64 {
    i64::from(desk.own_pid())
}

pub(crate) fn copy(desk: &mut impl Desktop, text: &str) -> Result<()> {
    ensure!(
        text.len() <= COPY_LIMIT,
        "El texto supera el límite de 1 MiB del portapapeles"
    );
    desk.set_text(text)
}

/// The focused external window. Our own windows are refused.
pub(crate) fn active_target(desk: &mut impl Desktop) -> Result<PasteTarget> {
    let target = desk.foreground()?;
    ensure!(target.pid != own(desk), OWN_PASTE);
    Ok(target)
}

fn wait_for_release(desk: &mut impl Desktop, error: &'static str) -> Result<()> {
    let end = desk.now() + MODIFIER_WAIT;
    while desk.modifiers_down() {
        ensure!(desk.now() < end, error);
        desk.sleep(POLL_INTERVAL);
    }
    Ok(())
}

/// Waits for the shortcut's modifiers, rechecks the foreground window, then
/// types. Focus can still change between the last check and `SendInput`.
fn type_into(
    desk: &mut impl Desktop,
    target: &PasteTarget,
    keys: Keys,
    is_current: &dyn Fn() -> bool,
) -> Result<()> {
    wait_for_release(desk, HELD_PASTE)?;
    ensure!(&desk.foreground()? == target, FOCUS_CHANGED);
    ensure!(is_current(), CANCELLED);
    desk.send(keys).map_err(|_| anyhow!(SEND_PASTE))
}

/// Copy first, then paste into the focused external window; with `expected`,
/// refuse if the focus moved. Errors leave the text in the clipboard.
pub(crate) fn paste(
    desk: &mut impl Desktop,
    text: &str,
    expected: Option<&PasteTarget>,
    keys: &str,
) -> Result<()> {
    copy(desk, text)?;
    let keys = paste_keys(keys)?;
    let actual = active_target(desk)?;
    if let Some(expected) = expected {
        ensure!(&actual == expected, FOCUS_CHANGED);
    }
    type_into(desk, &actual, keys, &|| true)
}

/// An external foreground window is the target. A request from one of our
/// windows goes back to the window remembered when dictation started.
pub(crate) fn prepare_paste(
    desk: &mut impl Desktop,
    remembered: Option<&PasteTarget>,
) -> Result<PastePlan> {
    let active = desk.foreground()?;
    if active.pid != own(desk) {
        return Ok(PastePlan::new(active, None));
    }
    let target = remembered.context(
        "No hay un destino externo para este dictado; seleccioná el campo de destino y repetí el atajo de pegar",
    )?;
    ensure!(
        target.pid > 0 && target.pid != own(desk) && hwnd_of(target).is_some(),
        "No hay un destino de pegado válido"
    );
    Ok(PastePlan::new(target.clone(), Some(active)))
}

/// Copies, restores the remembered window only for a request from our own
/// window, rechecks focus right before typing and never sends Enter.
/// `is_current` cancels before focus changes or keys are typed.
pub(crate) fn paste_prepared(
    desk: &mut impl Desktop,
    text: &str,
    plan: &PastePlan,
    keys: &str,
    is_current: &dyn Fn() -> bool,
) -> Result<()> {
    ensure!(is_current(), CANCELLED);
    copy(desk, text)?;
    let keys = paste_keys(keys)?;
    let target = plan.target();
    ensure!(target.pid != own(desk), OWN_PASTE);
    if let Some(origin) = plan.restore_from() {
        ensure!(
            origin.pid == own(desk),
            "La restauración no fue solicitada desde esta instancia de Mars voice"
        );
        ensure!(desk.is_alive(target), TARGET_GONE);
        ensure!(is_current(), CANCELLED);
        // Never take the focus away from another app the user moved to.
        ensure!(desk.foreground()?.pid == own(desk), FOCUS_CHANGED);
        desk.activate(target);
        let end = desk.now() + ACTIVATE_WAIT;
        while desk.foreground().ok().as_ref() != Some(target) {
            ensure!(desk.now() < end, NOT_ACTIVATED);
            desk.sleep(POLL_INTERVAL);
        }
    }
    type_into(desk, target, keys, is_current)
}

/// One read of the clipboard. Windows has no primary selection.
pub(crate) fn read_text(
    desk: &mut impl Desktop,
    source: SelectionSource,
    max_bytes: usize,
) -> Result<Option<String>> {
    ensure!(source == SelectionSource::Clipboard, NO_PRIMARY);
    let end = desk.now() + READ_TIMEOUT;
    loop {
        match desk.read_text(max_bytes) {
            ClipText::Text(text) => return Ok((!text.trim().is_empty()).then_some(text)),
            ClipText::NoText => return Ok(None),
            ClipText::TooLarge => bail!(
                "El portapapeles supera el límite de lectura. Copiá menos texto o aumentá el límite en Ajustes > Lectura"
            ),
            ClipText::Busy if desk.now() < end => desk.sleep(POLL_INTERVAL),
            ClipText::Busy => bail!(BUSY),
        }
    }
}

/// Copies the foreground window's selection with Ctrl+C (Ctrl+Shift+C in
/// terminals) and reads it. `Ok(None)` when nothing was selected: if the
/// clipboard sequence number does not change, the clipboard was never touched
/// and its old content is never mistaken for the selection. Otherwise the
/// previous text or image is put back when `restore_clipboard` is set.
pub(crate) fn copy_selection<D: Desktop>(
    desk: &mut D,
    options: &CopyOptions<'_>,
) -> Result<Option<String>> {
    let target = desk.foreground()?;
    ensure!(target.pid != own(desk), OWN_COPY);
    let keys = if is_terminal(&target, options.extra_terminal_classes) {
        Keys::CtrlShiftC
    } else {
        Keys::CtrlC
    };
    wait_for_release(desk, HELD_COPY)?;
    let saved = if options.restore_clipboard {
        Some(desk.save()?)
    } else {
        None
    };
    let before = desk.sequence();
    ensure!(desk.foreground()? == target, COPY_FOCUS_CHANGED);
    desk.send(keys).map_err(|_| anyhow!(SEND_COPY))?;
    let end = desk.now() + POLL_WINDOW;
    loop {
        desk.sleep(POLL_INTERVAL);
        if desk.sequence() != before {
            break;
        }
        if desk.now() >= end {
            return Ok(None);
        }
    }
    let copied = read_copied(desk, options.max_bytes);
    match saved {
        None => copied,
        Some(Saved::Unrestorable) => match copied {
            // Like a manual copy: the selection replaced what was there.
            Ok(Some(text)) => Ok(Some(text)),
            Ok(None) => Err(anyhow!("No había texto seleccionado. {LOST}")),
            Err(error) => Err(anyhow!("{error}. {LOST}")),
        },
        Some(saved) => match (copied, desk.restore(&saved)) {
            (Ok(text), Ok(())) => Ok(text),
            // The selection stays in the clipboard, like a manual copy.
            (Ok(Some(text)), Err(_)) => Ok(Some(text)),
            (Ok(None), Err(_)) => Err(anyhow!("No había texto seleccionado. {NOT_RESTORED}")),
            (Err(error), Ok(())) => Err(error),
            (Err(error), Err(_)) => Err(anyhow!("{error}. {NOT_RESTORED}")),
        },
    }
}

/// The source app may still hold the clipboard open right after it changed.
fn read_copied(desk: &mut impl Desktop, max_bytes: usize) -> Result<Option<String>> {
    let end = desk.now() + READ_WINDOW;
    loop {
        match desk.read_text(max_bytes) {
            ClipText::Text(text) => return Ok(Some(text)),
            ClipText::TooLarge => bail!(
                "La selección supera el límite de lectura. Seleccioná menos texto o aumentá el límite en Ajustes > Lectura"
            ),
            ClipText::NoText | ClipText::Busy if desk.now() < end => desk.sleep(POLL_INTERVAL),
            ClipText::NoText => return Ok(None),
            ClipText::Busy => bail!(BUSY),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    const OWN_PID: u32 = 77;

    fn window(hwnd: usize, pid: u32, class: &str, exe: &str) -> PasteTarget {
        target_from(hwnd, pid, class, Some(exe)).unwrap()
    }
    fn notepad() -> PasteTarget {
        window(0x1a2b, 4242, "Notepad", r"C:\Windows\System32\notepad.exe")
    }
    fn ours() -> PasteTarget {
        window(
            0x99,
            OWN_PID,
            "Zed::Window",
            r"C:\Apps\MarsVoice\MarsVoice.exe",
        )
    }

    /// In-memory Windows: a clock advanced by `sleep`, the foreground window,
    /// held modifiers, the clipboard and what Ctrl+C would copy.
    struct Fake {
        clock: Instant,
        start: Instant,
        foreground: Option<PasteTarget>,
        alive: bool,
        activation_works: bool,
        modifiers_until: Duration,
        clipboard: Saved<u8>,
        sequence: u32,
        /// What the copy keys put in the clipboard (None: nothing selected).
        selection: Option<&'static str>,
        /// Sleeps until the copy lands, if it was sent.
        pending: Option<usize>,
        busy_reads: usize,
        restore_fails: bool,
        actions: Vec<String>,
    }
    impl Fake {
        fn new(foreground: PasteTarget) -> Self {
            let now = Instant::now();
            Self {
                clock: now,
                start: now,
                foreground: Some(foreground),
                alive: true,
                activation_works: true,
                modifiers_until: Duration::ZERO,
                clipboard: Saved::Text("anterior".into()),
                sequence: 10,
                selection: Some("texto elegido"),
                pending: None,
                busy_reads: 0,
                restore_fails: false,
                actions: vec![],
            }
        }
        fn sent(&self) -> Vec<&str> {
            self.actions
                .iter()
                .filter_map(|a| a.strip_prefix("send "))
                .collect()
        }
        fn copy(&mut self, restore: bool, extra: &[String]) -> Result<Option<String>> {
            let options = CopyOptions {
                max_bytes: 64,
                restore_clipboard: restore,
                extra_terminal_classes: extra,
            };
            copy_selection(self, &options)
        }
    }
    impl Desktop for Fake {
        type Image = u8;
        fn now(&self) -> Instant {
            self.clock
        }
        fn sleep(&mut self, duration: Duration) {
            self.clock += duration;
            if let Some(left) = self.pending.as_mut() {
                if *left <= 1 {
                    self.pending = None;
                    self.clipboard = Saved::Text(self.selection.unwrap().into());
                    self.sequence += 1;
                } else {
                    *left -= 1;
                }
            }
        }
        fn own_pid(&self) -> u32 {
            OWN_PID
        }
        fn foreground(&mut self) -> Result<PasteTarget> {
            self.foreground.clone().context(NO_WINDOW)
        }
        fn is_alive(&mut self, _target: &PasteTarget) -> bool {
            self.alive
        }
        fn activate(&mut self, target: &PasteTarget) {
            self.actions.push(format!("activate {}", target.address));
            if self.activation_works {
                self.foreground = Some(target.clone());
            }
        }
        fn modifiers_down(&mut self) -> bool {
            self.clock - self.start < self.modifiers_until
        }
        fn send(&mut self, keys: Keys) -> Result<()> {
            self.actions.push(format!("send {keys:?}"));
            if matches!(keys, Keys::CtrlC | Keys::CtrlShiftC) && self.selection.is_some() {
                self.pending = Some(2);
            }
            Ok(())
        }
        fn sequence(&mut self) -> u32 {
            self.sequence
        }
        fn set_text(&mut self, text: &str) -> Result<()> {
            self.actions.push("set_text".into());
            self.clipboard = Saved::Text(text.into());
            self.sequence += 1;
            Ok(())
        }
        fn read_text(&mut self, max_bytes: usize) -> ClipText {
            if self.busy_reads > 0 {
                self.busy_reads -= 1;
                return ClipText::Busy;
            }
            match &self.clipboard {
                Saved::Text(text) if text.len() > max_bytes => ClipText::TooLarge,
                Saved::Text(text) => ClipText::Text(text.clone()),
                _ => ClipText::NoText,
            }
        }
        fn save(&mut self) -> Result<Saved<u8>> {
            self.actions.push("save".into());
            Ok(self.clipboard.clone())
        }
        fn restore(&mut self, saved: &Saved<u8>) -> Result<()> {
            self.actions.push("restore".into());
            ensure!(!self.restore_fails, "fallo");
            assert_ne!(saved, &Saved::Unrestorable, "never restored");
            self.clipboard = saved.clone();
            self.sequence += 1;
            Ok(())
        }
    }

    #[test]
    fn targets_carry_the_handle_process_class_and_program() {
        let target = notepad();
        assert_eq!(target.address, "0x1a2b");
        assert_eq!(target.stable_id.as_deref(), Some("0x1a2b"));
        assert_eq!(target.pid, 4242);
        assert_eq!(target.class, "Notepad");
        assert_eq!(target.initial_class, "notepad.exe");
        assert_eq!(hwnd_of(&target), Some(0x1a2b));
        assert_eq!(
            target_from(1, 2, "X", None).unwrap().initial_class,
            "",
            "an unreadable program name stays empty"
        );
        assert!(target_from(0, 2, "X", None).is_err());
        assert!(target_from(1, 0, "X", None).is_err());
        let mut forged = notepad();
        forged.address = "0x0".into();
        assert_eq!(hwnd_of(&forged), None);
    }

    #[test]
    fn terminals_get_ctrl_shift_c_and_browsers_do_not() {
        let terminal = |class: &str, exe: &str| is_terminal(&window(1, 2, class, exe), &[]);
        assert!(terminal(
            "CASCADIA_HOSTING_WINDOW_CLASS",
            "WindowsTerminal.exe"
        ));
        assert!(terminal("ConsoleWindowClass", "conhost.exe"));
        assert!(terminal("mintty", "mintty.exe"));
        assert!(terminal("PuTTY", "putty.exe"));
        assert!(terminal("Window Class", r"C:\Tools\Alacritty.EXE"));
        assert!(terminal("org.wezfurlong.wezterm", "wezterm-gui.exe"));
        assert!(!terminal("Chrome_WidgetWin_1", "chrome.exe"));
        assert!(!terminal("MozillaWindowClass", "firefox.exe"));
        // No substring guess: "term" in a name is not enough.
        assert!(!terminal("TermsViewer", "termsviewer.exe"));
        let tabby = window(1, 2, "Chrome_WidgetWin_1", "Tabby.exe");
        assert!(!is_terminal(&tabby, &[]));
        for extra in ["tabby", "Tabby.exe", " TABBY "] {
            assert!(is_terminal(&tabby, &[extra.into()]), "{extra}");
        }
        assert!(!is_terminal(&tabby, &["  ".into()]));
    }

    #[test]
    fn key_sequences_release_everything_and_paste_keys_match_hyprland() {
        assert_eq!(
            Keys::CtrlShiftC.sequence(),
            vec![
                (Vk::Ctrl, true),
                (Vk::Shift, true),
                (Vk::C, true),
                (Vk::C, false),
                (Vk::Shift, false),
                (Vk::Ctrl, false),
            ]
        );
        assert_eq!(
            Keys::ShiftInsert.sequence(),
            vec![
                (Vk::Shift, true),
                (Vk::Insert, true),
                (Vk::Insert, false),
                (Vk::Shift, false),
            ]
        );
        assert_eq!(paste_keys(" ctrl+v ").unwrap(), Keys::CtrlV);
        assert_eq!(paste_keys("CTRL+SHIFT+V").unwrap(), Keys::CtrlShiftV);
        assert_eq!(paste_keys("Shift+Insert").unwrap(), Keys::ShiftInsert);
        assert!(paste_keys("CTRL+V+ENTER").is_err());
    }

    #[test]
    fn paste_waits_for_the_shortcut_modifiers_then_types_into_the_same_window() {
        let mut desk = Fake::new(notepad());
        desk.modifiers_until = Duration::from_millis(300);
        paste(&mut desk, "hola", Some(&notepad()), "CTRL+V").unwrap();
        assert_eq!(desk.sent(), vec!["CtrlV"]);
        assert!(desk.clock - desk.start >= Duration::from_millis(300));
        assert_eq!(desk.clipboard, Saved::Text("hola".into()));
    }

    #[test]
    fn paste_never_types_with_held_modifiers_moved_focus_or_into_our_windows() {
        let mut desk = Fake::new(notepad());
        desk.modifiers_until = Duration::from_secs(5);
        let error = paste(&mut desk, "hola", None, "CTRL+V").unwrap_err();
        assert!(error.to_string().contains("seguían pulsadas"), "{error}");
        assert!(desk.sent().is_empty());
        assert_eq!(desk.clipboard, Saved::Text("hola".into()));

        let mut desk = Fake::new(notepad());
        let other = window(0x77, 4242, "Notepad", "notepad.exe");
        let error = paste(&mut desk, "hola", Some(&other), "CTRL+V").unwrap_err();
        assert!(error.to_string().contains("Cambió la ventana activa"));
        assert!(desk.sent().is_empty());

        let mut desk = Fake::new(ours());
        let error = paste(&mut desk, "hola", None, "CTRL+V").unwrap_err();
        assert!(error.to_string().contains("fuera de Mars voice"), "{error}");
        assert!(desk.sent().is_empty());
        assert!(active_target(&mut desk).is_err());
    }

    #[test]
    fn prepare_paste_targets_the_external_window_or_the_remembered_one_from_our_ui() {
        let mut desk = Fake::new(notepad());
        let plan = prepare_paste(&mut desk, None).unwrap();
        assert_eq!(plan.target(), &notepad());
        assert!(plan.restore_from().is_none());

        let mut desk = Fake::new(ours());
        assert!(prepare_paste(&mut desk, None).is_err());
        assert!(prepare_paste(&mut desk, Some(&ours())).is_err());
        let plan = prepare_paste(&mut desk, Some(&notepad())).unwrap();
        assert_eq!(plan.target(), &notepad());
        assert_eq!(plan.restore_from(), Some(&ours()));
    }

    #[test]
    fn paste_from_our_ui_returns_to_the_remembered_window_before_typing() {
        let mut desk = Fake::new(ours());
        let plan = prepare_paste(&mut desk, Some(&notepad())).unwrap();
        paste_prepared(&mut desk, "hola", &plan, "CTRL+V", &|| true).unwrap();
        assert_eq!(
            desk.actions,
            vec!["set_text", "activate 0x1a2b", "send CtrlV"]
        );

        // Windows refused to switch: nothing is typed into our own window.
        let mut desk = Fake::new(ours());
        desk.activation_works = false;
        let error = paste_prepared(&mut desk, "hola", &plan, "CTRL+V", &|| true).unwrap_err();
        assert!(error.to_string().contains("no permitió volver"), "{error}");
        assert!(desk.sent().is_empty());

        // The remembered window closed (or its handle was reused).
        let mut desk = Fake::new(ours());
        desk.alive = false;
        assert!(paste_prepared(&mut desk, "hola", &plan, "CTRL+V", &|| true).is_err());
        assert!(desk.sent().is_empty());
        assert!(!desk.actions.iter().any(|a| a.starts_with("activate")));

        // The user moved to another app meanwhile: its focus is not taken.
        let mut desk = Fake::new(window(0x55, 99, "Chrome_WidgetWin_1", "chrome.exe"));
        assert!(paste_prepared(&mut desk, "hola", &plan, "CTRL+V", &|| true).is_err());
        assert!(desk.sent().is_empty());
        assert!(!desk.actions.iter().any(|a| a.starts_with("activate")));
    }

    #[test]
    fn cancelled_pastes_stop_before_focus_or_keys() {
        let mut desk = Fake::new(ours());
        let plan = prepare_paste(&mut desk, Some(&notepad())).unwrap();
        assert!(paste_prepared(&mut desk, "hola", &plan, "CTRL+V", &|| false).is_err());
        assert!(desk.actions.is_empty());

        // Cancelled after copying (e.g. the text was edited): no focus change.
        let calls = Cell::new(0);
        let current = || {
            calls.set(calls.get() + 1);
            calls.get() == 1
        };
        let mut desk = Fake::new(ours());
        assert!(paste_prepared(&mut desk, "hola", &plan, "CTRL+V", &current).is_err());
        assert_eq!(desk.actions, vec!["set_text"]);
    }

    #[test]
    fn unchanged_clipboard_means_nothing_selected_and_is_left_untouched() {
        let mut desk = Fake::new(notepad());
        desk.selection = None;
        assert_eq!(desk.copy(true, &[]).unwrap(), None);
        assert_eq!(desk.actions, vec!["save", "send CtrlC"]);
        assert_eq!(desk.clipboard, Saved::Text("anterior".into()));
        assert!(desk.clock - desk.start >= POLL_WINDOW);
    }

    #[test]
    fn copied_selection_is_read_and_the_previous_clipboard_restored() {
        let mut desk = Fake::new(notepad());
        desk.modifiers_until = Duration::from_millis(100);
        assert_eq!(
            desk.copy(true, &[]).unwrap().as_deref(),
            Some("texto elegido")
        );
        assert_eq!(desk.actions, vec!["save", "send CtrlC", "restore"]);
        assert_eq!(desk.clipboard, Saved::Text("anterior".into()));

        // An image and an empty clipboard come back as they were.
        for previous in [Saved::Image(7), Saved::Empty] {
            let mut desk = Fake::new(notepad());
            desk.clipboard = previous.clone();
            assert!(desk.copy(true, &[]).unwrap().is_some());
            assert_eq!(desk.clipboard, previous);
        }

        // Without restoring, the selection stays, like a manual copy.
        let mut desk = Fake::new(notepad());
        assert!(desk.copy(false, &[]).unwrap().is_some());
        assert_eq!(desk.actions, vec!["send CtrlC"]);
        assert_eq!(desk.clipboard, Saved::Text("texto elegido".into()));
    }

    #[test]
    fn copy_uses_ctrl_shift_c_in_terminals_and_refuses_our_windows_first() {
        let mut desk = Fake::new(window(
            5,
            6,
            "CASCADIA_HOSTING_WINDOW_CLASS",
            "WindowsTerminal.exe",
        ));
        assert!(desk.copy(true, &[]).unwrap().is_some());
        assert_eq!(desk.sent(), vec!["CtrlShiftC"]);

        let mut desk = Fake::new(ours());
        let error = desk.copy(true, &[]).unwrap_err();
        assert!(error.to_string().contains("otra ventana"), "{error}");
        assert!(desk.actions.is_empty(), "the clipboard is not touched");

        let mut desk = Fake::new(notepad());
        desk.modifiers_until = Duration::from_secs(3);
        assert!(desk.copy(true, &[]).is_err());
        assert!(desk.actions.is_empty());
    }

    #[test]
    fn busy_or_unrestorable_clipboards_are_reported_without_content() {
        // The source app still holds the clipboard right after changing it.
        let mut desk = Fake::new(notepad());
        desk.busy_reads = 3;
        assert_eq!(
            desk.copy(true, &[]).unwrap().as_deref(),
            Some("texto elegido")
        );

        let mut desk = Fake::new(notepad());
        desk.busy_reads = 1_000;
        let error = desk.copy(true, &[]).unwrap_err().to_string();
        assert!(error.contains("usando el portapapeles"), "{error}");
        assert_eq!(desk.clipboard, Saved::Text("anterior".into()));

        // Files in the clipboard cannot be saved: the selection replaces them.
        let mut desk = Fake::new(notepad());
        desk.clipboard = Saved::Unrestorable;
        assert!(desk.copy(true, &[]).unwrap().is_some());
        assert!(!desk.actions.contains(&"restore".to_owned()));

        let mut desk = Fake::new(notepad());
        desk.restore_fails = true;
        assert!(desk.copy(true, &[]).unwrap().is_some());

        let mut desk = Fake::new(notepad());
        desk.selection =
            Some("una selección demasiado larga para el límite de esta prueba, sin duda");
        let error = desk.copy(true, &[]).unwrap_err().to_string();
        assert!(error.contains("supera el límite"), "{error}");
        assert!(!error.contains("demasiado larga"), "{error}");
        assert_eq!(desk.clipboard, Saved::Text("anterior".into()));
    }

    #[test]
    fn clipboard_reads_skip_whitespace_and_primary_is_not_offered() {
        let mut desk = Fake::new(notepad());
        let error = read_text(&mut desk, SelectionSource::Primary, 64).unwrap_err();
        assert!(error.to_string().contains("selección primaria"), "{error}");
        assert_eq!(
            read_text(&mut desk, SelectionSource::Clipboard, 64)
                .unwrap()
                .as_deref(),
            Some("anterior")
        );
        desk.clipboard = Saved::Text(" \n\t".into());
        assert_eq!(
            read_text(&mut desk, SelectionSource::Clipboard, 64).unwrap(),
            None
        );
        desk.clipboard = Saved::Image(1);
        assert_eq!(
            read_text(&mut desk, SelectionSource::Clipboard, 64).unwrap(),
            None
        );
        desk.clipboard = Saved::Text("x".repeat(65));
        assert!(read_text(&mut desk, SelectionSource::Clipboard, 64).is_err());
        assert!(copy(&mut desk, &"x".repeat(COPY_LIMIT + 1)).is_err());
    }
}
