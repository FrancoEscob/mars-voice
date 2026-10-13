//! The Windows shell thread. One dedicated thread owns the hidden window of
//! `global-hotkey` (`RegisterHotKey`) and the icon of `tray-icon`, and runs
//! the Win32 message loop (`GetMessageW`) both need. The controller talks to
//! it through [`Hotkeys`] and [`crate::tray::spawn`]; requests travel through
//! a channel, and a 100 ms thread timer wakes the loop so they are served
//! even when a wake-up was swallowed by the modal loop of the tray menu.
//!
//! Key presses, menu clicks and tray clicks become [`Command`]s sent to the
//! controller; nothing is logged.
use crate::shortcuts::{
    self,
    keys::{self, Action, Binding},
};
use anyhow::{Result, anyhow};
use dicta_core::{AppConfig, Command};
use global_hotkey::{
    GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
    hotkey::{Code, HotKey, Modifiers},
};
use std::{
    collections::HashMap,
    str::FromStr,
    sync::{
        LazyLock, Mutex, MutexGuard, OnceLock,
        mpsc::{self, Receiver, Sender, SyncSender},
    },
    time::Duration,
};
use tray_icon::{
    Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
};
use windows_sys::Win32::{
    System::Threading::GetCurrentThreadId,
    UI::WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, GetSystemMetrics, MSG, PM_NOREMOVE, PeekMessageW,
        PostThreadMessageW, SM_CXSMICON, SM_CYSMICON, SetTimer, TranslateMessage, WM_NULL, WM_USER,
    },
};

/// How long the controller waits for the shell thread to answer.
const REPLY_TIMEOUT: Duration = Duration::from_secs(3);
/// Wake-up period of the message loop, for requests whose wake-up was lost.
const TICK_MS: u32 = 100;
/// Icon resource embedded by `crates/dicta/build.rs`.
const ICON_RESOURCE: u16 = 1;

/// Where events go. Shared with the event handlers, which `global-hotkey`
/// and `tray-icon` call from their window procedures (and, for hotkey
/// releases, from a short-lived polling thread).
#[derive(Default)]
struct Routes {
    hotkeys: Option<Sender<Command>>,
    actions: HashMap<u32, Action>,
    tray: Option<Sender<Command>>,
}
static ROUTES: LazyLock<Mutex<Routes>> = LazyLock::new(Mutex::default);

fn routes() -> MutexGuard<'static, Routes> {
    ROUTES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

enum Request {
    /// Replace every registered shortcut; the reply lists Spanish notices for
    /// those Windows refused.
    Hotkeys {
        bindings: Vec<Binding>,
        commands: Option<Sender<Command>>,
        reply: SyncSender<Vec<String>>,
    },
    Tray {
        commands: Sender<Command>,
        reply: SyncSender<Result<(), String>>,
    },
    RemoveTray {
        reply: SyncSender<()>,
    },
}

struct Shell {
    requests: Sender<Request>,
    thread: u32,
}
static SHELL: OnceLock<Result<Shell, String>> = OnceLock::new();

fn shell() -> Result<&'static Shell> {
    SHELL
        .get_or_init(start)
        .as_ref()
        .map_err(|error| anyhow!("{error}"))
}

fn start() -> Result<Shell, String> {
    let (requests, receiver) = mpsc::channel();
    let (ready_tx, ready) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("dicta-shell".into())
        .spawn(move || run(receiver, ready_tx))
        .map_err(|error| format!("no se pudo iniciar el hilo de atajos y bandeja: {error}"))?;
    let thread = ready
        .recv_timeout(REPLY_TIMEOUT)
        .map_err(|_| "el hilo de atajos y bandeja no respondió".to_owned())?;
    Ok(Shell { requests, thread })
}

impl Shell {
    fn ask<T>(&self, request: impl FnOnce(SyncSender<T>) -> Request) -> Result<T> {
        let (reply, answer) = mpsc::sync_channel(1);
        self.requests
            .send(request(reply))
            .map_err(|_| anyhow!("el hilo de atajos y bandeja terminó"))?;
        // SAFETY: posts an empty message to a thread id; fails harmlessly.
        unsafe { PostThreadMessageW(self.thread, WM_NULL, 0, 0) };
        answer
            .recv_timeout(REPLY_TIMEOUT)
            .map_err(|_| anyhow!("el hilo de atajos y bandeja no respondió"))
    }
}

fn run(requests: Receiver<Request>, ready: SyncSender<u32>) {
    // SAFETY: MSG is plain data; all-zero is a valid value.
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    // SAFETY: valid MSG out-parameter. Creates this thread's message queue
    // before its id is published, so PostThreadMessageW cannot miss it.
    unsafe {
        PeekMessageW(
            &mut msg,
            std::ptr::null_mut(),
            WM_USER,
            WM_USER,
            PM_NOREMOVE,
        )
    };
    let manager = GlobalHotKeyManager::new().map_err(|error| error.to_string());
    install_handlers();
    // SAFETY: a thread timer without window or callback: WM_TIMER wakes the loop.
    unsafe { SetTimer(std::ptr::null_mut(), 0, TICK_MS, None) };
    // SAFETY: no arguments.
    if ready.send(unsafe { GetCurrentThreadId() }).is_err() {
        return;
    }
    let mut state = State {
        manager,
        registered: Vec::new(),
        tray: None,
    };
    loop {
        // SAFETY: valid MSG out-parameter; 0 is WM_QUIT and -1 an error.
        let result = unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) };
        if result == 0 || result == -1 {
            break;
        }
        // SAFETY: a message just returned by GetMessageW.
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        while let Ok(request) = requests.try_recv() {
            state.handle(request);
        }
    }
}

fn install_handlers() {
    GlobalHotKeyEvent::set_event_handler(Some(|event: GlobalHotKeyEvent| {
        let at = shortcuts::event_stamp();
        let routes = routes();
        let command = routes
            .actions
            .get(&event.id)
            .and_then(|action| action.command(event.state == HotKeyState::Pressed, at));
        if let (Some(command), Some(commands)) = (command, &routes.hotkeys) {
            let _ = commands.send(command);
        }
    }));
    MenuEvent::set_event_handler(Some(|event: MenuEvent| {
        if let (Some(command), Some(commands)) =
            (crate::tray::menu_command(event.id.as_ref()), &routes().tray)
        {
            let _ = commands.send(command);
        }
    }));
    TrayIconEvent::set_event_handler(Some(|event: TrayIconEvent| {
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = event
            && let Some(commands) = &routes().tray
        {
            let _ = commands.send(Command::ShowSettings);
        }
    }));
}

struct State {
    manager: Result<GlobalHotKeyManager, String>,
    registered: Vec<HotKey>,
    tray: Option<TrayIcon>,
}

impl State {
    fn handle(&mut self, request: Request) {
        match request {
            Request::Hotkeys {
                bindings,
                commands,
                reply,
            } => {
                let notices = self.register(bindings, commands);
                let _ = reply.send(notices);
            }
            Request::Tray { commands, reply } => {
                let result = match self.tray {
                    Some(_) => Ok(()),
                    None => build_tray().map(|tray| self.tray = Some(tray)),
                };
                if result.is_ok() {
                    routes().tray = Some(commands);
                }
                let _ = reply.send(result.map_err(|error| error.to_string()));
            }
            Request::RemoveTray { reply } => {
                routes().tray = None;
                // Dropping the icon removes it from the notification area.
                self.tray = None;
                let _ = reply.send(());
            }
        }
    }

    fn register(
        &mut self,
        bindings: Vec<Binding>,
        commands: Option<Sender<Command>>,
    ) -> Vec<String> {
        let manager = match &self.manager {
            Ok(manager) => manager,
            // Unregistering everything needs no manager.
            Err(_) if bindings.is_empty() => return Vec::new(),
            Err(error) => {
                return vec![format!(
                    "Los atajos globales no están disponibles en esta sesión de Windows: {error}"
                )];
            }
        };
        for hotkey in self.registered.drain(..) {
            let _ = manager.unregister(hotkey);
        }
        let mut actions = HashMap::new();
        let mut notices = Vec::new();
        for binding in bindings {
            let registered = hotkey(&binding).and_then(|hotkey| {
                manager.register(hotkey).ok()?;
                Some(hotkey)
            });
            match registered {
                Some(hotkey) => {
                    actions.insert(hotkey.id(), binding.action);
                    self.registered.push(hotkey);
                }
                None => notices.push(keys::taken_notice(&binding)),
            }
        }
        let mut routes = routes();
        routes.actions = actions;
        routes.hotkeys = commands;
        notices
    }
}

fn hotkey(binding: &Binding) -> Option<HotKey> {
    let combo = binding.combo;
    let mut modifiers = Modifiers::empty();
    for (on, modifier) in [
        (combo.modifiers.ctrl, Modifiers::CONTROL),
        (combo.modifiers.alt, Modifiers::ALT),
        (combo.modifiers.shift, Modifiers::SHIFT),
        (combo.modifiers.super_key, Modifiers::SUPER),
    ] {
        if on {
            modifiers |= modifier;
        }
    }
    let code = Code::from_str(&combo.key.code_name()).ok()?;
    Some(HotKey::new(Some(modifiers), code))
}

fn build_tray() -> Result<TrayIcon> {
    let menu = Menu::new();
    for (id, label) in crate::tray::MENU {
        if *id == crate::tray::QUIT_ID {
            menu.append(&PredefinedMenuItem::separator())?;
        }
        menu.append(&MenuItem::with_id(*id, *label, true, None))?;
    }
    Ok(TrayIconBuilder::new()
        .with_id("dicta")
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .with_tooltip("Mars voice")
        .with_icon(tray_icon_image()?)
        .build()?)
}

/// The app icon embedded in the executable, at the small-icon size; a plain
/// generated mark if the resource is missing (for example in tests).
fn tray_icon_image() -> Result<Icon> {
    // SAFETY: no arguments.
    let (width, height) = unsafe { (GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON)) };
    let size = (width.max(16) as u32, height.max(16) as u32);
    Icon::from_resource(ICON_RESOURCE, Some(size)).or_else(|_| {
        let (rgba, side) = crate::tray::fallback_icon();
        Ok(Icon::from_rgba(rgba, side, side)?)
    })
}

/// Global shortcuts registered with Windows on the shell thread. Toggle sends
/// press and release, so hold-to-talk and toggle both work through the
/// controller; dropping the handle unregisters everything.
pub struct Hotkeys {
    commands: Sender<Command>,
    /// The shortcut strings last applied, to skip needless re-registration.
    applied: Option<[String; 5]>,
}

impl Hotkeys {
    /// Registers the configured shortcuts. The notice (Spanish) names every
    /// shortcut that is invalid or that Windows refused, for the UI.
    pub fn start(config: &AppConfig, commands: Sender<Command>) -> (Self, Option<String>) {
        let mut hotkeys = Self {
            commands,
            applied: None,
        };
        let notice = hotkeys.apply(config);
        (hotkeys, notice)
    }

    /// Re-registers when a shortcut changed in the config; `None` when nothing
    /// changed or everything registered.
    pub fn update(&mut self, config: &AppConfig) -> Option<String> {
        if self.applied.as_ref() == Some(&signature(config)) {
            return None;
        }
        self.apply(config)
    }

    fn apply(&mut self, config: &AppConfig) -> Option<String> {
        let signature = signature(config);
        if self
            .applied
            .as_ref()
            .is_some_and(|applied| applied[0] != signature[0])
        {
            // The old dictation key can no longer report its release.
            let _ = self.commands.send(Command::ShortcutReleased {
                at: shortcuts::event_stamp(),
                token: None,
            });
        }
        let (bindings, mut notices) = keys::bindings(config);
        let commands = Some(self.commands.clone());
        match shell().and_then(|shell| {
            shell.ask(|reply| Request::Hotkeys {
                bindings,
                commands,
                reply,
            })
        }) {
            Ok(refused) => notices.extend(refused),
            Err(error) => notices.push(format!(
                "Los atajos globales no están disponibles: {error}. Usá la bandeja o la ventana de Mars voice"
            )),
        }
        self.applied = Some(signature);
        (!notices.is_empty()).then(|| notices.join(" "))
    }
}

impl Drop for Hotkeys {
    fn drop(&mut self) {
        if let Ok(shell) = shell() {
            let _ = shell.ask(|reply| Request::Hotkeys {
                bindings: Vec::new(),
                commands: None,
                reply,
            });
        }
    }
}

fn signature(config: &AppConfig) -> [String; 5] {
    [
        config.toggle_shortcut.trim().to_owned(),
        config.paste_shortcut.trim().to_owned(),
        config.tts.read_shortcut.trim().to_owned(),
        config.tts.pause_shortcut.trim().to_owned(),
        config.tts.stop_shortcut.trim().to_owned(),
    ]
}

/// Keeps the tray icon; dropping it removes the icon.
pub struct TrayHandle(());

impl Drop for TrayHandle {
    fn drop(&mut self) {
        if let Ok(shell) = shell() {
            let _ = shell.ask(|reply| Request::RemoveTray { reply });
        }
    }
}

/// Shows the tray icon (once per process). Blocks for at most a few seconds.
pub(crate) fn spawn_tray(commands: Sender<Command>) -> Result<TrayHandle> {
    shell()?
        .ask(|reply| Request::Tray { commands, reply })?
        .map_err(|error| anyhow!("No se pudo crear el icono de la bandeja: {error}"))?;
    Ok(TrayHandle(()))
}
