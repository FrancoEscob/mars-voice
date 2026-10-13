//! Windows backend: arboard for the clipboard, Win32 for the foreground
//! window, `SendInput` to type and `GetClipboardSequenceNumber` to tell a
//! fresh copy from old content. The flows themselves live in `win_flow.rs`,
//! where they are tested without Windows.
//!
//! Limits, also in `docs/WINDOWS.md`:
//! - Windows lets a process bring another window to the front only while it
//!   owns the foreground (the user just clicked one of its windows). Pasting
//!   from the overlay relies on that; when Windows refuses, the paste stops
//!   and the text stays copied.
//! - `SendInput` cannot type into programs running as administrator unless
//!   Mars voice runs elevated too (UIPI), and Windows reports no error.
//! - Placement does nothing: GPUI opens the overlay and the reader where the
//!   UI asks, so nothing has to be moved afterwards.
use super::win_flow::{self, ClipText, Desktop, Keys, Saved, Vk};
use super::{Backend, Clipboard, FocusTracker, Injector, SelectionReader, WindowPlacer};
use crate::desktop::{FloatingWindow, PastePlan, PasteTarget};
use crate::selection::{CopyOptions, SelectionSource};
use anyhow::{Result, anyhow, bail};
use std::time::{Duration, Instant};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HWND},
    System::{
        DataExchange::{CountClipboardFormats, GetClipboardSequenceNumber},
        Threading::{
            OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
            QueryFullProcessImageNameW,
        },
    },
    UI::{
        Input::KeyboardAndMouse::{
            GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
            KEYEVENTF_KEYUP, MAPVK_VK_TO_VSC, MapVirtualKeyW, SendInput, VIRTUAL_KEY, VK_C,
            VK_CONTROL, VK_INSERT, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT, VK_V,
        },
        WindowsAndMessaging::{
            GetClassNameW, GetForegroundWindow, GetWindowThreadProcessId, IsIconic, IsWindow,
            SW_RESTORE, SetForegroundWindow, ShowWindow,
        },
    },
};

pub(super) struct Windows;

pub(super) const BACKEND: Backend = Backend {
    name: "windows",
    clipboard: &Windows,
    focus: &Windows,
    injector: &Windows,
    selection: &Windows,
    placement: &Windows,
};

/// Largest previous clipboard image kept to restore it (RGBA bytes).
const SNAPSHOT_LIMIT: usize = 64 << 20;
const SAVE_BUSY: &str =
    "Otra aplicación está usando el portapapeles, así que no se tocó. Volvé a intentarlo";
const SEND_FAILED: &str = "Windows no permitió enviar las teclas a la ventana activa";

impl Clipboard for Windows {
    fn copy(&self, text: &str) -> Result<()> {
        win_flow::copy(&mut Live::default(), text)
    }
}

impl FocusTracker for Windows {
    fn active_target(&self) -> Result<PasteTarget> {
        win_flow::active_target(&mut Live::default())
    }
}

impl Injector for Windows {
    fn paste(&self, text: &str, expected: Option<&PasteTarget>, keys: &str) -> Result<()> {
        win_flow::paste(&mut Live::default(), text, expected, keys)
    }
    fn prepare_paste(&self, remembered: Option<&PasteTarget>) -> Result<PastePlan> {
        win_flow::prepare_paste(&mut Live::default(), remembered)
    }
    fn paste_prepared(
        &self,
        text: &str,
        plan: &PastePlan,
        keys: &str,
        is_current: &dyn Fn() -> bool,
    ) -> Result<()> {
        win_flow::paste_prepared(&mut Live::default(), text, plan, keys, is_current)
    }
}

impl SelectionReader for Windows {
    fn read_text(&self, source: SelectionSource, max_bytes: usize) -> Result<Option<String>> {
        win_flow::read_text(&mut Live::default(), source, max_bytes)
    }
    fn copy_selection(&self, options: CopyOptions<'_>) -> Result<Option<String>> {
        win_flow::copy_selection(&mut Live::default(), &options)
    }
}

/// GPUI already opens the overlay and the reader at the position the UI
/// computed, so there is nothing to move after mapping.
impl WindowPlacer for Windows {
    fn set_placement_enabled(&self, _window: FloatingWindow, _enabled: bool) {}
    fn place(&self, _window: FloatingWindow, _x: f32, _y: f32) {}
}

/// The flows' desktop on real Windows. The clipboard is opened lazily, only
/// for the calls that need it.
#[derive(Default)]
struct Live {
    board: Option<arboard::Clipboard>,
}

impl Live {
    fn board(&mut self) -> Result<&mut arboard::Clipboard, arboard::Error> {
        if self.board.is_none() {
            self.board = Some(arboard::Clipboard::new()?);
        }
        Ok(self.board.as_mut().expect("just created"))
    }
}

/// Identity of a top-level window, or the "no active window" error.
fn describe(hwnd: HWND) -> Result<PasteTarget> {
    let mut pid = 0u32;
    let mut class = [0u16; 256];
    let mut length = 0;
    // SAFETY: plain queries on a window handle with valid out-buffers; they
    // fail cleanly (0) for a null or destroyed window.
    unsafe {
        if !hwnd.is_null() && IsWindow(hwnd) != 0 {
            GetWindowThreadProcessId(hwnd, &mut pid);
            length = GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32);
        }
    }
    let class = String::from_utf16_lossy(&class[..length.max(0) as usize]);
    let program = (pid != 0).then(|| program_path(pid)).flatten();
    win_flow::target_from(hwnd as usize, pid, &class, program.as_deref())
}

/// Full path of a process's executable, if Windows lets us query it.
fn program_path(pid: u32) -> Option<String> {
    let mut buffer = [0u16; 1024];
    let mut length = buffer.len() as u32;
    // SAFETY: the handle is checked and closed; the buffer and its length match.
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return None;
        }
        let ok = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            buffer.as_mut_ptr(),
            &mut length,
        );
        CloseHandle(process);
        (ok != 0).then(|| String::from_utf16_lossy(&buffer[..length as usize]))
    }
}

fn key_input(key: Vk, down: bool) -> INPUT {
    let (vk, extended): (VIRTUAL_KEY, bool) = match key {
        Vk::Ctrl => (VK_CONTROL, false),
        Vk::Shift => (VK_SHIFT, false),
        Vk::V => (VK_V, false),
        Vk::C => (VK_C, false),
        Vk::Insert => (VK_INSERT, true),
    };
    let mut flags = if down { 0 } else { KEYEVENTF_KEYUP };
    if extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    // SAFETY: pure translation of a virtual key to its scan code.
    let scan = unsafe { MapVirtualKeyW(u32::from(vk), MAPVK_VK_TO_VSC) } as u16;
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn send_inputs(inputs: &[INPUT]) -> u32 {
    // SAFETY: a valid array of keyboard INPUTs with its exact length and size.
    unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            std::mem::size_of::<INPUT>() as i32,
        )
    }
}

impl Desktop for Live {
    type Image = arboard::ImageData<'static>;

    fn now(&self) -> Instant {
        Instant::now()
    }
    fn sleep(&mut self, duration: Duration) {
        std::thread::sleep(duration);
    }
    fn own_pid(&self) -> u32 {
        std::process::id()
    }
    fn foreground(&mut self) -> Result<PasteTarget> {
        // SAFETY: no arguments; returns null when no window has the focus.
        describe(unsafe { GetForegroundWindow() })
    }
    fn is_alive(&mut self, target: &PasteTarget) -> bool {
        win_flow::hwnd_of(target)
            .is_some_and(|hwnd| describe(hwnd as HWND).ok().as_ref() == Some(target))
    }
    fn activate(&mut self, target: &PasteTarget) {
        let Some(hwnd) = win_flow::hwnd_of(target) else {
            return;
        };
        let hwnd = hwnd as HWND;
        // SAFETY: window calls on a handle that was just validated; they fail
        // cleanly if the window disappeared meanwhile.
        unsafe {
            if IsIconic(hwnd) != 0 {
                ShowWindow(hwnd, SW_RESTORE);
            }
            SetForegroundWindow(hwnd);
        }
    }
    fn modifiers_down(&mut self) -> bool {
        [VK_CONTROL, VK_MENU, VK_SHIFT, VK_LWIN, VK_RWIN]
            .into_iter()
            // SAFETY: reads the physical key state; no pointers involved.
            .any(|vk| unsafe { GetAsyncKeyState(i32::from(vk)) } as u16 & 0x8000 != 0)
    }
    fn send(&mut self, keys: Keys) -> Result<()> {
        let inputs: Vec<INPUT> = keys
            .sequence()
            .into_iter()
            .map(|(key, down)| key_input(key, down))
            .collect();
        if send_inputs(&inputs) as usize != inputs.len() {
            // Never leave a modifier logically held after a partial send.
            send_inputs(&[key_input(Vk::Shift, false), key_input(Vk::Ctrl, false)]);
            bail!(SEND_FAILED);
        }
        Ok(())
    }
    fn sequence(&mut self) -> u32 {
        // SAFETY: no arguments.
        unsafe { GetClipboardSequenceNumber() }
    }
    fn set_text(&mut self, text: &str) -> Result<()> {
        self.board()
            .and_then(|board| board.set_text(text))
            .map_err(|_| {
                anyhow!("No se pudo copiar al portapapeles de Windows; volvé a intentarlo")
            })
    }
    fn read_text(&mut self, max_bytes: usize) -> ClipText {
        match self.board().and_then(|board| board.get_text()) {
            Ok(text) if text.len() > max_bytes => ClipText::TooLarge,
            Ok(text) => ClipText::Text(text),
            Err(arboard::Error::ContentNotAvailable | arboard::Error::ConversionFailure) => {
                ClipText::NoText
            }
            Err(_) => ClipText::Busy,
        }
    }
    fn save(&mut self) -> Result<Saved<Self::Image>> {
        let board = self.board().map_err(|_| anyhow!(SAVE_BUSY))?;
        match board.get_text() {
            Ok(text) => return Ok(Saved::Text(text)),
            Err(arboard::Error::ClipboardOccupied) => bail!(SAVE_BUSY),
            Err(_) => {}
        }
        match board.get_image() {
            Ok(image) if image.bytes.len() <= SNAPSHOT_LIMIT => return Ok(Saved::Image(image)),
            Ok(_) => return Ok(Saved::Unrestorable),
            Err(arboard::Error::ClipboardOccupied) => bail!(SAVE_BUSY),
            Err(_) => {}
        }
        // SAFETY: no arguments; counts formats without opening the clipboard.
        Ok(if unsafe { CountClipboardFormats() } == 0 {
            Saved::Empty
        } else {
            Saved::Unrestorable
        })
    }
    fn restore(&mut self, saved: &Saved<Self::Image>) -> Result<()> {
        use arboard::SetExtWindows;
        let board = self.board().map_err(|_| anyhow!("portapapeles"))?;
        // The previous content was already in the clipboard history once.
        match saved {
            Saved::Empty => board.clear(),
            Saved::Text(text) => board
                .set()
                .exclude_from_history()
                .exclude_from_cloud()
                .text(text.as_str()),
            Saved::Image(image) => board
                .set()
                .exclude_from_history()
                .exclude_from_cloud()
                .image(image.clone()),
            Saved::Unrestorable => Ok(()),
        }
        .map_err(|_| anyhow!("portapapeles"))
    }
}
