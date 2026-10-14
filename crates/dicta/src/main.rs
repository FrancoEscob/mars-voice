// Release builds on Windows are GUI programs: no console window opens with
// them. Commands that print reattach to the terminal that launched them.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
use anyhow::{Context, Result, bail};
use dicta::{
    controller::{self, Options},
    ipc,
};
use dicta_core::{AppConfig, AppPaths, AppSnapshot, ColorMode, Command, Theme};
use std::{
    path::Path,
    sync::mpsc,
    time::{Duration, Instant},
};

fn main() {
    if let Err(error) = run() {
        report(&format!("Mars voice: {error:#}"));
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn report(message: &str) {
    eprintln!("{message}");
}
/// Opened from Explorer there is no console to print to: show a dialog.
#[cfg(windows)]
fn report(message: &str) {
    if windows::attach_console() {
        eprintln!("{message}");
    } else {
        windows::error_dialog(message);
    }
}

/// Lets the resident instance bring its window to the front: Windows only
/// allows that to the process the user is interacting with, which is this one.
fn hand_off_foreground() {
    #[cfg(windows)]
    windows::allow_foreground();
}

#[cfg(windows)]
mod windows {
    use windows_sys::Win32::{
        System::Console::{ATTACH_PARENT_PROCESS, AttachConsole, GetConsoleWindow},
        UI::WindowsAndMessaging::{
            ASFW_ANY, AllowSetForegroundWindow, MB_ICONERROR, MB_OK, MessageBoxW,
        },
    };

    /// Attaches to the console of the parent terminal, if any. True when this
    /// process has a console to print to (always in debug builds).
    pub fn attach_console() -> bool {
        // SAFETY: no pointers; fails harmlessly without a parent console or
        // when a console is already attached.
        unsafe { AttachConsole(ATTACH_PARENT_PROCESS) != 0 || !GetConsoleWindow().is_null() }
    }

    pub fn error_dialog(message: &str) {
        let wide = |text: &str| -> Vec<u16> {
            text.encode_utf16()
                .filter(|&unit| unit != 0)
                .chain(std::iter::once(0))
                .collect()
        };
        let (text, caption) = (wide(message), wide("Mars voice"));
        // SAFETY: both strings are NUL-terminated UTF-16 that outlive the call.
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                text.as_ptr(),
                caption.as_ptr(),
                MB_OK | MB_ICONERROR,
            )
        };
    }

    pub fn allow_foreground() {
        // SAFETY: no pointers; only effective while this process may set the
        // foreground window itself.
        unsafe { AllowSetForegroundWindow(ASFW_ANY) };
    }
}

fn run() -> Result<()> {
    let shortcut_at = dicta_platform::shortcuts::event_stamp();
    let args: Vec<String> = std::env::args().skip(1).collect();
    #[cfg(windows)]
    if args.iter().any(|arg| {
        matches!(
            arg.as_str(),
            "ctl" | "--help" | "-h" | "--version" | "--shortcut-seed"
        )
    }) {
        windows::attach_console();
    }
    if args.len() == 1 && args[0] == "--shortcut-seed" {
        // Used once when loading compositor bindings, before any config, IPC,
        // keyring or graphical initialization. Same boot-wide clock as events.
        println!("{shortcut_at}");
        return Ok(());
    }
    if args.iter().any(|s| s == "--help" || s == "-h") {
        println!(
            "Mars voice — dictado nativo para {}\n\n  dicta [--demo] [--theme signal|regolith|orbit] [--portal-shortcuts]\n  dicta --headless [--no-tray]\n  dicta --read-selection\n  dicta --write-selection\n  dicta ctl toggle|start|stop|cancel|copy|paste|settings|show|hide|read|read-clipboard|tts-toggle|tts-stop|write|write-clipboard|quit|status\n\nOpciones:\n  --demo                 Ejemplo sin micrófono, API, llavero ni portapapeles real\n  --background           Iniciar residente sin abrir ajustes\n  --toggle               Iniciar o detener dictado; abre Dicta si hace falta\n  --read-selection       Leer en voz alta la selección; abre Mars voice si hace falta\n  --write-selection      Arreglar o traducir la selección; abre Mars voice si hace falta\n  --shortcut-press       Pulsación del atajo; respeta modo alternar/mantener\n  --shortcut-release     Liberación del atajo; nunca inicia Dicta\n  --shortcut-token N     Identificador creciente compartido por pulsación/liberación\n  --shortcut-seed        Imprimir reloj monotónico para inicializar binds y salir\n  --headless             Ejecutar sin interfaz gráfica\n  --no-tray              No crear icono en la bandeja\n  --color-mode MODO      light o dark, independiente del estilo\n  --theme TEMA           Apariencia inicial (se guarda al editar ajustes)\n  --portal-shortcuts     Solicitar al portal los atajos globales\n  --allow-env-api-key    Permitir DICTA_XAI_API_KEY en modo API explícito\n  --quit-after SEGUNDOS  Salir tras el plazo (pruebas)\n\nPara controlar la demo: dicta --demo ctl COMANDO.\nLos binds manuales pueden usar dicta --toggle. Al abrir normalmente no se activa el micrófono.",
            if cfg!(windows) { "Windows" } else { "Linux" }
        );
        return Ok(());
    }
    if args.iter().any(|s| s == "--version") {
        println!("Mars voice {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let mut options = Options::default();
    let mut headless = false;
    let mut background = false;
    let mut toggle = false;
    let mut read = false;
    let mut write = false;
    let mut shortcut = None;
    let mut shortcut_token = None;
    let mut theme = None;
    let mut color_mode = None;
    let mut quit_after = None;
    let mut ctl = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--background" => background = true,
            "--toggle" => toggle = true,
            "--read-selection" => read = true,
            "--write-selection" => write = true,
            "--shortcut-press" | "--shortcut-release" => {
                if shortcut.is_some() {
                    bail!("Usá una sola acción de atajo por lanzamiento");
                }
                shortcut = Some(arg.as_str() == "--shortcut-press");
            }
            "--shortcut-token" => {
                let token = it.next().context("Falta token de gesto")?.parse::<u64>()?;
                if token == 0 {
                    bail!("Token de gesto inválido");
                }
                shortcut_token = Some(token);
            }
            "--demo" => options.demo = true,
            "--headless" => headless = true,
            "--no-tray" => options.no_tray = true,
            "--portal-shortcuts" => options.portal = true,
            "--allow-env-api-key" => options.allow_env_key = true,
            "--color-mode" => {
                color_mode = Some(match it.next().map(String::as_str) {
                    Some("light" | "claro") => ColorMode::Light,
                    Some("dark" | "oscuro") => ColorMode::Dark,
                    _ => bail!("Modo inválido; usá light o dark"),
                });
            }
            "--theme" => {
                theme = Some(match it.next().map(String::as_str) {
                    Some("signal" | "señal") => Theme::Signal,
                    Some("regolith" | "regolito") => Theme::Regolith,
                    Some("night" | "noche") => Theme::Night,
                    Some("orbit" | "orbita" | "órbita") => Theme::Orbit,
                    _ => bail!("Tema inválido; usá signal, regolith u orbit"),
                })
            }
            "--quit-after" => {
                quit_after = Some(it.next().context("Faltan segundos")?.parse::<u64>()?)
            }
            "ctl" => ctl = Some(it.next().context("Falta el comando de control")?.clone()),
            _ => bail!("Opción desconocida: {arg}. Usá --help"),
        }
    }
    if (toggle && ctl.is_some())
        || (shortcut.is_some() && (toggle || ctl.is_some()))
        || (read && (toggle || ctl.is_some() || shortcut.is_some()))
        || (write && (read || toggle || ctl.is_some() || shortcut.is_some()))
    {
        bail!("Usá una acción de atajo o ctl COMANDO, no ambos en el mismo lanzamiento");
    }
    if shortcut_token.is_some() && shortcut.is_none() {
        bail!("--shortcut-token requiere pulsación o liberación");
    }
    let mut paths = AppPaths::discover()?;
    if options.demo {
        paths.runtime = paths.demo_runtime();
    }
    if let Some(command) = ctl {
        hand_off_foreground();
        println!("{}", ipc::send(&paths.runtime, &command)?);
        return Ok(());
    }
    // Hold this only through initial command delivery, never for the lifetime
    // of the resident. A concurrent release then sees either the server or a
    // persisted release fence, including while the press is starting cold.
    let mut shortcut_lock = if shortcut.is_some() {
        Some(ipc::ShortcutLaunch::lock(&paths.runtime)?)
    } else {
        None
    };
    let shortcut_command = shortcut.map(|press| {
        format!(
            "shortcut-{}:{shortcut_at}:{}",
            if press { "press" } else { "release" },
            shortcut_token.unwrap_or(0)
        )
    });
    if let Some(press) = shortcut {
        let guard = shortcut_lock.as_mut().unwrap();
        if press && guard.was_released(shortcut_at, shortcut_token)? {
            return Ok(());
        }
        if !press {
            guard.release(shortcut_at, shortcut_token)?;
        }
        match ipc::send(&paths.runtime, shortcut_command.as_deref().unwrap()) {
            Ok(_) => return Ok(()),
            Err(error) if server_absent(&error) => {
                if !press {
                    return Ok(());
                }
            }
            Err(error) => return Err(error).context("No se confirmó el atajo"),
        }
    }
    if toggle {
        // A warm shortcut must not load settings, touch credentials or start a
        // second UI. Retry only failures that prove nothing was delivered.
        hand_off_foreground();
        match ipc::send(&paths.runtime, "toggle") {
            Ok(_) => return Ok(()),
            Err(error) if server_absent(&error) => {}
            Err(error) => {
                return Err(error)
                    .context("No se confirmó el atajo; no se repitió para evitar un doble toggle");
            }
        }
    }
    if read {
        // Igual que --toggle: el texto nunca viaja por el socket; lo lee el residente.
        hand_off_foreground();
        match ipc::send(&paths.runtime, "read") {
            Ok(_) => return Ok(()),
            Err(error) if server_absent(&error) => {}
            Err(error) => {
                return Err(error)
                    .context("No se confirmó la lectura; no se repitió para evitar dos lecturas");
            }
        }
    }
    if write {
        // Igual que --read-selection: el residente copia la selección; el texto
        // nunca pasa por la línea de comandos ni por el socket.
        hand_off_foreground();
        match ipc::send(&paths.runtime, "write") {
            Ok(_) => return Ok(()),
            Err(error) if server_absent(&error) => {}
            Err(error) => {
                return Err(error).context(
                    "No se confirmó la escritura; no se repitió para evitar copiar dos veces",
                );
            }
        }
    }
    if !cfg!(feature = "gui") && !headless {
        bail!(
            "Este binario se compiló sin GPUI. Usá --headless o recompilá con las features predeterminadas"
        );
    }
    let mut config = if options.demo {
        AppConfig::default()
    } else {
        paths.load()?
    };
    // OAuthClient resolves an explicit config/env override or an installed
    // public client ID when needed. Keep an empty config value empty so a later
    // connection can detect changes; startup never begins OAuth or reads tokens.
    if let Some(theme) = theme {
        config.theme = theme;
    }
    config.normalize();
    if let Some(mode) = color_mode {
        config.color_mode = mode;
    }
    let mut initial = AppSnapshot::new(config);
    initial.demo = options.demo;
    if background || toggle || read || write || shortcut.is_some() {
        initial.show_settings = false;
        initial.show_overlay = initial.config.show_idle_overlay;
    }
    if options.demo {
        initial.status = "Demo · sin micrófono ni conexión".into();
    }
    let (command_tx, commands) = mpsc::channel();
    let initial_command = if let Some(press) = shortcut {
        Some(if press {
            Command::ShortcutPressed {
                at: shortcut_at,
                token: shortcut_token,
            }
        } else {
            Command::ShortcutReleased {
                at: shortcut_at,
                token: shortcut_token,
            }
        })
    } else if toggle {
        Some(Command::Toggle)
    } else if read {
        Some(Command::ReadSelection)
    } else if write {
        Some(Command::WriterSelection)
    } else {
        None
    };
    let server = match ipc::start_with_initial(&paths.runtime, command_tx.clone(), initial_command)
    {
        Ok(server) => server,
        Err(error) => {
            let startup_command = shortcut_command.as_deref().unwrap_or(if toggle {
                "toggle"
            } else if read {
                "read"
            } else if write {
                "write"
            } else {
                "settings"
            });
            // Another launcher may hold the lock just before publishing its
            // socket. Never turn a shortcut into ShowSettings in that race.
            if let Err(forward_error) = forward_startup(&paths.runtime, startup_command) {
                return Err(forward_error).with_context(|| {
                    format!("No se pudo iniciar ni contactar el proceso residente: {error}")
                });
            }
            return Ok(());
        }
    };
    let (snapshots_tx, snapshots_rx) = mpsc::channel();
    if let Some(seconds) = quit_after {
        let tx = command_tx.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(seconds));
            let _ = tx.send(Command::Quit);
        });
    }
    let initial_controller = initial.clone();
    let tx = command_tx.clone();
    let controller = std::thread::Builder::new()
        .name("dicta-controller".into())
        .spawn(move || {
            let result = controller::run(
                paths,
                initial_controller,
                commands,
                tx,
                snapshots_tx,
                options,
            );
            drop(server);
            result
        })?;
    drop(shortcut_lock.take());
    let ui_result = if headless {
        // Drain snapshots, retaining no transcripts on disk or stdout.
        while snapshots_rx.recv().is_ok() {}
        Ok(())
    } else {
        #[cfg(feature = "gui")]
        {
            dicta_platform::desktop::set_overlay_placement_enabled(
                !initial.config.overlay_centered,
            );
            dicta_ui::set_overlay_placer(|origin| {
                dicta_platform::desktop::place_overlay(origin.x, origin.y);
            });
            dicta_platform::desktop::set_reader_placement_enabled(
                !initial.config.tts.window_centered,
            );
            dicta_ui::set_reader_placer(|origin| {
                dicta_platform::desktop::place_reader(origin.x, origin.y);
            });
            dicta_platform::desktop::set_writer_placement_enabled(
                !initial.config.writer.window_centered,
            );
            dicta_ui::set_writer_placer(|origin| {
                dicta_platform::desktop::place_writer(origin.x, origin.y);
            });
            let result = dicta_ui::run(initial, command_tx.clone(), snapshots_rx);
            let _ = command_tx.send(Command::Quit);
            result
        }
        #[cfg(not(feature = "gui"))]
        {
            unreachable!("headless validated above")
        }
    };
    controller
        .join()
        .map_err(|_| anyhow::anyhow!("El controlador terminó inesperadamente"))??;
    ui_result
}

fn server_absent(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause.downcast_ref::<std::io::Error>().is_some_and(|error| {
            matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            )
        })
    })
}
fn forward_startup(runtime: &Path, command: &str) -> Result<()> {
    hand_off_foreground();
    let deadline = Instant::now() + Duration::from_millis(750);
    loop {
        match ipc::send(runtime, command) {
            Ok(_) => return Ok(()),
            Err(error) if server_absent(&error) && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            // A timeout could mean the command was delivered but the ACK was
            // lost. Repeating a toggle would reverse the user's requested action.
            Err(error) => return Err(error),
        }
    }
}
