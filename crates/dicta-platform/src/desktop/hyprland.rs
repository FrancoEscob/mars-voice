//! Linux backend for [`super`]: wl-clipboard (`wl-copy`) on any Wayland
//! session, plus `hyprctl` (focus, window identity, placement) and `wtype`
//! (key injection) on Hyprland. Every operation keeps its own environment
//! check and Spanish error, so other sessions see the same messages as before
//! the platform seams existed. Reached only through [`crate::backend`].
//!
//! No clipboard polling, key logging, or background focus tracking. An
//! explicit paste may restore only the external target remembered for this
//! dictation, after validating its identity.
use super::{APP_ID, FloatingWindow, OVERLAY_CLASS, PastePlan, PasteTarget, READER_CLASS};
use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use std::{
    io::{Read, Write},
    process::{Command, Stdio},
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use wait_timeout::ChildExt;

/// Copy UTF-8 text. wl-copy forks its clipboard owner, which remains alive after
/// this function returns until another application takes ownership.
pub(crate) fn copy(text: &str) -> Result<()> {
    ensure!(
        std::env::var_os("WAYLAND_DISPLAY").is_some(),
        "El portapapeles requiere Wayland y wl-clipboard; X11 todavía no está implementado"
    );
    ensure!(
        text.len() <= 1024 * 1024,
        "El texto supera el límite de 1 MiB del portapapeles"
    );
    run(
        "wl-copy",
        &["--type", "text/plain;charset=utf-8"],
        Some(text.as_bytes()),
    )
    .context("No se pudo copiar; comprobá wl-clipboard y la sesión Wayland")?;
    Ok(())
}

/// Query only; own windows remain visible to the paste planner.
pub(crate) fn active_window() -> Result<PasteTarget> {
    ensure!(
        std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some(),
        "El pegado seguro requiere Hyprland; el texto sigue disponible para copiar"
    );
    let output = run("hyprctl", &["-j", "activewindow"], None)
        .context("No se pudo consultar el foco en Hyprland")?;
    let target: PasteTarget =
        serde_json::from_slice(&output).context("No hay una ventana activa válida en Hyprland")?;
    validate_identity(&target)?;
    Ok(target)
}
/// Capture an external destination before displaying a focusable overlay.
pub(crate) fn active_target() -> Result<PasteTarget> {
    let target = active_window()?;
    validate_target(&target, std::process::id())?;
    Ok(target)
}
fn validate_identity(target: &PasteTarget) -> Result<()> {
    ensure!(
        target
            .address
            .strip_prefix("0x")
            .and_then(|hex| u64::from_str_radix(hex, 16).ok())
            .is_some_and(|address| address != 0)
            && target.pid > 0,
        "No hay un destino de pegado válido"
    );
    Ok(())
}
/// This process, or any Dicta window class (settings, overlay, reader).
pub(crate) fn is_own_window(target: &PasteTarget, own_pid: u32) -> bool {
    let is_ours = |class: &str| {
        class.eq_ignore_ascii_case(APP_ID)
            || class.eq_ignore_ascii_case("dicta")
            || class.eq_ignore_ascii_case(OVERLAY_CLASS)
            || class.eq_ignore_ascii_case(READER_CLASS)
    };
    target.pid == i64::from(own_pid) || is_ours(&target.class) || is_ours(&target.initial_class)
}
fn validate_target(target: &PasteTarget, own_pid: u32) -> Result<()> {
    validate_identity(target)?;
    ensure!(
        !is_own_window(target, own_pid),
        "Seleccioná el campo de destino fuera de Dicta y repetí el atajo de pegar"
    );
    Ok(())
}
fn validate_expected(actual: &PasteTarget, expected: Option<&PasteTarget>) -> Result<()> {
    if let Some(expected) = expected {
        ensure!(
            actual == expected,
            "Cambió la ventana activa; el texto quedó copiado. Volvé al destino y repetí el atajo de pegar"
        );
    }
    Ok(())
}

/// Copy first, then explicitly paste into the currently focused external window.
/// With expected set, refuses if focus has changed. Never sends Enter or raises a
/// different window. Errors preserve the clipboard for manual paste.
/// Wayland has no atomic verify-focus-and-inject operation: focus can still race
/// after the last check. The user should keep the destination focused.
pub(crate) fn paste(text: &str, expected: Option<&PasteTarget>, keys: &str) -> Result<()> {
    copy(text)?;
    let args = paste_args(keys)?;
    let actual = active_target()?;
    validate_expected(&actual, expected)?;
    run("wtype",&args,None).context("El texto quedó copiado, pero no se pudo pegar. Comprobá wtype y el permiso virtual-keyboard del compositor")?;
    Ok(())
}

pub(crate) fn prepare_paste(remembered: Option<&PasteTarget>) -> Result<PastePlan> {
    plan_for(active_window()?, remembered, std::process::id())
}
fn plan_for(
    active: PasteTarget,
    remembered: Option<&PasteTarget>,
    own_pid: u32,
) -> Result<PastePlan> {
    validate_identity(&active)?;
    if active.pid == i64::from(own_pid) {
        let target=remembered.context("No hay un destino externo para este dictado; seleccioná el campo de destino y repetí el atajo de pegar")?;
        validate_target(target, own_pid)?;
        ensure!(
            target.stable_id.as_ref().is_some_and(|id| !id.is_empty()),
            "El compositor no identifica de forma persistente el destino; seleccioná el campo y usá el atajo de pegar"
        );
        Ok(PastePlan {
            target: target.clone(),
            restore_from: Some(active),
        })
    } else {
        validate_target(&active, own_pid)?;
        Ok(PastePlan {
            target: active,
            restore_from: None,
        })
    }
}

/// Copies first; restores a remembered live destination only for a request from
/// this process's UI. Rechecks focus immediately before input and never Enter.
/// `is_current` lets the controller cancel a queued/in-progress operation before
/// it changes focus or injects keys. Clipboard writes already started cannot undo.
/// Focus-check and key injection remain separate compositor operations.
pub(crate) fn paste_prepared(
    text: &str,
    plan: &PastePlan,
    keys: &str,
    is_current: impl Fn() -> bool,
) -> Result<()> {
    execute_paste(
        &mut LiveDesktop,
        text,
        plan,
        keys,
        std::process::id(),
        is_current,
    )
}
trait PasteDesktop {
    fn copy(&mut self, text: &str) -> Result<()>;
    fn active(&mut self) -> Result<PasteTarget>;
    fn clients(&mut self) -> Result<Vec<PasteTarget>>;
    fn focus(&mut self, target: &PasteTarget) -> Result<()>;
    fn inject(&mut self, args: &[&str]) -> Result<()>;
}
struct LiveDesktop;
impl PasteDesktop for LiveDesktop {
    fn copy(&mut self, text: &str) -> Result<()> {
        copy(text)
    }
    fn active(&mut self) -> Result<PasteTarget> {
        active_window()
    }
    fn clients(&mut self) -> Result<Vec<PasteTarget>> {
        serde_json::from_slice(&run("hyprctl", &["-j", "clients"], None)?)
            .context("No se pudo verificar que el destino siga abierto")
    }
    fn focus(&mut self, target: &PasteTarget) -> Result<()> {
        let dispatcher = focus_dispatcher(target)?;
        run("hyprctl", &["dispatch", &dispatcher], None)
            .context("No se pudo volver al destino; el texto quedó copiado")?;
        Ok(())
    }
    fn inject(&mut self, args: &[&str]) -> Result<()> {
        run("wtype", args, None)
            .context("El texto quedó copiado, pero el compositor no permitió pegar con wtype")?;
        Ok(())
    }
}
fn focus_dispatcher(target: &PasteTarget) -> Result<String> {
    validate_identity(target)?;
    let stable_id = safe_stable_id(target)?;
    // Select by lifetime ID, not an address that could be reused after the live
    // window check. No class/title/text is interpolated into Lua.
    // Official API: https://wiki.hypr.land/Configuring/Basics/Dispatchers/
    Ok(format!(
        "hl.dsp.focus({{ window = \"stableid:{stable_id}\" }})"
    ))
}

fn execute_paste(
    backend: &mut impl PasteDesktop,
    text: &str,
    plan: &PastePlan,
    keys: &str,
    own_pid: u32,
    is_current: impl Fn() -> bool,
) -> Result<()> {
    ensure!(is_current(), "Pegado cancelado");
    backend.copy(text)?;
    let args = paste_args(keys)?;
    validate_target(&plan.target, own_pid)?;
    if let Some(origin) = &plan.restore_from {
        ensure!(
            origin.pid == i64::from(own_pid),
            "La restauración no fue solicitada desde esta instancia de Dicta"
        );
        validate_expected(&backend.active()?, Some(origin))?;
        let clients = backend.clients()?;
        ensure!(
            clients.iter().any(|window| window == &plan.target),
            "El destino original ya no está abierto o cambió de identidad; el texto quedó copiado"
        );
        ensure!(is_current(), "Pegado cancelado");
        validate_expected(&backend.active()?, Some(origin))?;
        ensure!(is_current(), "Pegado cancelado");
        backend.focus(&plan.target)?;
    }
    let current = backend.active()?;
    validate_target(&current, own_pid)?;
    validate_expected(&current, Some(&plan.target))?;
    ensure!(is_current(), "Pegado cancelado");
    backend.inject(&args)
}

fn paste_args(keys: &str) -> Result<Vec<&'static str>> {
    match keys.to_ascii_uppercase().as_str() {
        "CTRL+V" => Ok(vec!["-M", "ctrl", "-P", "v", "-p", "v", "-m", "ctrl"]),
        "CTRL+SHIFT+V" => Ok(vec![
            "-M", "ctrl", "-M", "shift", "-P", "v", "-p", "v", "-m", "shift", "-m", "ctrl",
        ]),
        "SHIFT+INSERT" => Ok(vec![
            "-M", "shift", "-P", "Insert", "-p", "Insert", "-m", "shift",
        ]),
        _ => bail!("Usá CTRL+V, CTRL+SHIFT+V o SHIFT+INSERT como combinación de pegado"),
    }
}

struct PlacementState {
    enabled: AtomicBool,
    generation: AtomicU64,
    gate: Mutex<()>,
}
impl PlacementState {
    const fn new() -> Self {
        Self {
            enabled: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            gate: Mutex::new(()),
        }
    }
    fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Release);
        self.generation.fetch_add(1, Ordering::AcqRel);
    }
    fn current(&self, generation: u64) -> bool {
        self.enabled.load(Ordering::Acquire)
            && self.generation.load(Ordering::Acquire) == generation
    }
    fn request(&self) -> Option<u64> {
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        self.current(generation).then_some(generation)
    }
}
/// One floating window kind owned by this process, with its own generations so
/// placing one kind never cancels or moves the other.
struct PlacedWindow {
    class: &'static str,
    thread: &'static str,
    state: PlacementState,
}
static OVERLAY_PLACEMENT: PlacedWindow = PlacedWindow {
    class: OVERLAY_CLASS,
    thread: "dicta-overlay-place",
    state: PlacementState::new(),
};
static READER_PLACEMENT: PlacedWindow = PlacedWindow {
    class: READER_CLASS,
    thread: "dicta-reader-place",
    state: PlacementState::new(),
};

fn placed(window: FloatingWindow) -> &'static PlacedWindow {
    match window {
        FloatingWindow::Overlay => &OVERLAY_PLACEMENT,
        FloatingWindow::Reader => &READER_PLACEMENT,
    }
}

/// Enable manual offsets from controller config. Disabling cancels pending
/// placements so an old callback cannot move a newly centered window. Each
/// window kind keeps its own state.
pub(crate) fn set_placement_enabled(window: FloatingWindow, enabled: bool) {
    placed(window).state.set_enabled(enabled);
}

/// Best-effort, non-blocking one-shot placement of this process's mapped
/// floating `dicta-overlay` / `dicta-reader`. Coordinates are logical pixels
/// relative to its actual monitor. UI already resolves negative offsets against
/// its chosen display; this adapter cannot reconstruct bottom/right anchoring
/// if the compositor chose another size. It clamps to the actual monitor and
/// never moves settings or another process.
pub(crate) fn place(window: FloatingWindow, x: f32, y: f32) {
    place_window(placed(window), x, y);
}

fn place_window(window: &'static PlacedWindow, x: f32, y: f32) {
    let Some(generation) = window.state.request() else {
        return;
    };
    if !x.is_finite() || !y.is_finite() || std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_none()
    {
        return;
    }
    let _ = std::thread::Builder::new()
        .name(window.thread.into())
        .spawn(move || {
            let Ok(_guard) = window.state.gate.lock() else {
                return;
            };
            if !window.state.current(generation) {
                return;
            }
            let _ = place_window_once(window, x, y, generation);
        });
}
#[derive(Clone, Debug, Deserialize)]
struct OverlayClient {
    #[serde(flatten)]
    target: PasteTarget,
    mapped: bool,
    floating: bool,
    monitor: i64,
    size: [f64; 2],
}
#[derive(Clone, Debug, Deserialize)]
struct OverlayMonitor {
    id: i64,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    scale: f64,
    #[serde(default)]
    transform: u8,
    #[serde(default)]
    disabled: bool,
}
fn select_window<'a>(
    clients: &'a [OverlayClient],
    own_pid: u32,
    class: &str,
) -> Option<&'a OverlayClient> {
    let mut matches = clients.iter().filter(|client| {
        client.mapped
            && client.floating
            && client.monitor >= 0
            && client.target.pid == i64::from(own_pid)
            && client.target.class == class
            && validate_identity(&client.target).is_ok()
            && safe_stable_id(&client.target).is_ok()
    });
    let first = matches.next()?;
    // Reopening may briefly expose both old and new surfaces. Never guess.
    matches.next().is_none().then_some(first)
}
fn overlay_position(
    x: f32,
    y: f32,
    window: [f64; 2],
    monitor: &OverlayMonitor,
) -> Option<[i32; 2]> {
    if monitor.disabled
        || monitor.transform > 7
        || ![
            monitor.x,
            monitor.y,
            monitor.width,
            monitor.height,
            monitor.scale,
            window[0],
            window[1],
            x as f64,
            y as f64,
        ]
        .into_iter()
        .all(f64::is_finite)
        || monitor.scale <= 0.
        || monitor.width <= 0.
        || monitor.height <= 0.
        || window[0] <= 0.
        || window[1] <= 0.
    {
        return None;
    }
    // Hyprland's width/height are physical mode size; its desktop coordinates
    // use transformed and scaled logical size (official monitor positioning API).
    let (width, height) = if monitor.transform % 2 == 1 {
        (monitor.height, monitor.width)
    } else {
        (monitor.width, monitor.height)
    };
    let logical = [
        (width / monitor.scale).round(),
        (height / monitor.scale).round(),
    ];
    let absolute = [
        monitor.x + (x as f64).clamp(0., (logical[0] - window[0]).max(0.)),
        monitor.y + (y as f64).clamp(0., (logical[1] - window[1]).max(0.)),
    ];
    if !absolute
        .into_iter()
        .all(|v| v >= i32::MIN as f64 && v <= i32::MAX as f64)
    {
        return None;
    }
    Some([absolute[0].round() as i32, absolute[1].round() as i32])
}
pub(crate) fn safe_stable_id(target: &PasteTarget) -> Result<&str> {
    let id = target
        .stable_id
        .as_deref()
        .context("El destino no tiene identidad persistente")?;
    ensure!(
        !id.is_empty()
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')),
        "Identidad persistente inválida"
    );
    Ok(id)
}
fn window_move_dispatcher(
    client: &OverlayClient,
    position: [i32; 2],
    own_pid: u32,
    class: &str,
) -> Result<String> {
    ensure!(
        select_window(std::slice::from_ref(client), own_pid, class).is_some(),
        "No es una ventana propia elegible"
    );
    let id = safe_stable_id(&client.target)?;
    Ok(format!(
        "hl.dsp.window.move({{ x = {}, y = {}, relative = false, window = \"stableid:{id}\" }})",
        position[0], position[1]
    ))
}
fn place_window_once(window: &PlacedWindow, x: f32, y: f32, generation: u64) -> Result<()> {
    let deadline = Instant::now() + Duration::from_millis(1800);
    // Let xdg_toplevel map and any outgoing window unmap before the first read.
    std::thread::sleep(Duration::from_millis(40));
    let query = |args: &[&str]| -> Result<Vec<u8>> {
        ensure!(window.state.current(generation), "Posición obsoleta");
        let left = deadline
            .checked_duration_since(Instant::now())
            .context("Se agotó la espera del overlay")?;
        run_timeout("hyprctl", args, None, left.min(Duration::from_millis(300)))
    };
    while Instant::now() < deadline && window.state.current(generation) {
        let clients: Vec<OverlayClient> = serde_json::from_slice(&query(&["-j", "clients"])?)?;
        let Some(client) = select_window(&clients, std::process::id(), window.class) else {
            std::thread::sleep(Duration::from_millis(40));
            continue;
        };
        let monitors: Vec<OverlayMonitor> = serde_json::from_slice(&query(&["-j", "monitors"])?)?;
        let monitor = monitors
            .iter()
            .find(|m| m.id == client.monitor)
            .context("Monitor no disponible")?;
        // Re-query before moving: refuse stale identity, changed monitor, or an
        // old surface that overlapped a newly opened overlay during mapping.
        let current: Vec<OverlayClient> = serde_json::from_slice(&query(&["-j", "clients"])?)?;
        let current = select_window(&current, std::process::id(), window.class)
            .context("Ventana ambigua o cerrada")?;
        ensure!(
            current.target == client.target && current.monitor == client.monitor,
            "La ventana cambió mientras se colocaba"
        );
        let position =
            overlay_position(x, y, current.size, monitor).context("Geometría no válida")?;
        let dispatcher =
            window_move_dispatcher(current, position, std::process::id(), window.class)?;
        query(&["dispatch", &dispatcher])?;
        return Ok(());
    }
    Ok(())
}

// No shell. Text is stdin, never a command argument. Suppress tool stderr because
// a clipboard helper might echo sensitive content; errors remain actionable.
fn run(program: &str, args: &[&str], input: Option<&[u8]>) -> Result<Vec<u8>> {
    run_timeout(program, args, input, Duration::from_secs(3))
}
pub(crate) fn run_timeout(
    program: &str,
    args: &[&str],
    input: Option<&[u8]>,
    timeout: Duration,
) -> Result<Vec<u8>> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(if program == "hyprctl" {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("No se pudo ejecutar {program}"))?;
    // wl-copy's long-lived owner must not inherit a reader pipe that would
    // keep us waiting for EOF after its parent returns.
    let reader = child.stdout.take().map(|output| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            output
                .take(1024 * 1024 + 1)
                .read_to_end(&mut buf)
                .map(|_| buf)
        })
    });
    let writer = input.map(|bytes| {
        let bytes = bytes.to_vec();
        let mut stdin = child.stdin.take().expect("piped stdin");
        std::thread::spawn(move || stdin.write_all(&bytes))
    });
    let status = match child.wait_timeout(timeout)? {
        Some(status) => status,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            bail!("{program} no respondió dentro del plazo");
        }
    };
    if let Some(writer) = writer {
        writer
            .join()
            .map_err(|_| anyhow::anyhow!("Falló el envío al portapapeles"))??;
    }
    let bytes = match reader {
        Some(reader) => reader
            .join()
            .map_err(|_| anyhow::anyhow!("Falló la lectura del proceso"))??,
        None => Vec::new(),
    };
    ensure!(
        status.success(),
        "{program} rechazó la operación; revisá la sesión y sus permisos"
    );
    ensure!(
        bytes.len() <= 1024 * 1024,
        "Respuesta demasiado grande de {program}"
    );
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn target() -> PasteTarget {
        PasteTarget {
            address: "0x123abc".into(),
            pid: 1234,
            class: "editor".into(),
            initial_class: "editor".into(),
            stable_id: Some("test-editor-window".into()),
        }
    }
    #[test]
    fn refuses_self_unknown_and_changed_focus() {
        let mut t = target();
        validate_target(&t, 77).unwrap();
        t.pid = 77;
        assert!(validate_target(&t, 77).is_err());
        t.pid = 1234;
        t.initial_class = APP_ID.into();
        assert!(validate_target(&t, 77).is_err());
        t = target();
        t.address = "0x0".into();
        assert!(validate_target(&t, 77).is_err());
        let mut other = target();
        other.address = "0x888".into();
        assert!(validate_expected(&target(), Some(&other)).is_err());
        validate_expected(&target(), Some(&target())).unwrap();
    }
    fn overlay() -> PasteTarget {
        PasteTarget {
            address: "0x777".into(),
            pid: 77,
            class: "dicta-overlay".into(),
            initial_class: "dicta-overlay".into(),
            stable_id: Some("own-overlay".into()),
        }
    }
    struct FakeDesktop {
        active: std::collections::VecDeque<PasteTarget>,
        clients: Vec<PasteTarget>,
        actions: Vec<&'static str>,
        focused: Vec<PasteTarget>,
        focus_fails: bool,
    }
    impl FakeDesktop {
        fn new(active: Vec<PasteTarget>, clients: Vec<PasteTarget>) -> Self {
            Self {
                active: active.into(),
                clients,
                actions: vec![],
                focused: vec![],
                focus_fails: false,
            }
        }
    }
    impl PasteDesktop for FakeDesktop {
        fn copy(&mut self, _: &str) -> Result<()> {
            self.actions.push("copy");
            Ok(())
        }
        fn active(&mut self) -> Result<PasteTarget> {
            self.actions.push("active");
            self.active.pop_front().context("No más fixtures de foco")
        }
        fn clients(&mut self) -> Result<Vec<PasteTarget>> {
            self.actions.push("clients");
            Ok(self.clients.clone())
        }
        fn focus(&mut self, target: &PasteTarget) -> Result<()> {
            self.actions.push("focus");
            self.focused.push(target.clone());
            ensure!(!self.focus_fails, "Compositor rechazó foco");
            Ok(())
        }
        fn inject(&mut self, args: &[&str]) -> Result<()> {
            assert!(!args.contains(&"Enter"));
            self.actions.push("paste");
            Ok(())
        }
    }
    #[test]
    fn overlay_paste_restores_only_the_remembered_live_window() {
        let plan = plan_for(overlay(), Some(&target()), 77).unwrap();
        let mut backend = FakeDesktop::new(vec![overlay(), overlay(), target()], vec![target()]);
        execute_paste(&mut backend, "fixture", &plan, "CTRL+V", 77, || true).unwrap();
        assert_eq!(backend.focused, vec![target()]);
        assert_eq!(
            backend.actions,
            vec![
                "copy", "active", "clients", "active", "focus", "active", "paste"
            ]
        );
    }
    #[test]
    fn external_hotkey_uses_current_app_without_restoring_an_old_destination() {
        let mut current = target();
        current.address = "0x999".into();
        current.stable_id = Some("new-destination".into());
        let plan = plan_for(current.clone(), Some(&target()), 77).unwrap();
        let mut backend = FakeDesktop::new(vec![current], vec![]);
        execute_paste(&mut backend, "fixture", &plan, "CTRL+SHIFT+V", 77, || true).unwrap();
        assert_eq!(backend.actions, vec!["copy", "active", "paste"]);
        assert!(backend.focused.is_empty());
    }
    #[test]
    fn dead_or_reused_destination_never_receives_focus_or_input() {
        let plan = plan_for(overlay(), Some(&target()), 77).unwrap();
        let mut reused = target();
        reused.stable_id = Some("replacement-window-same-pid-address".into());
        for clients in [vec![], vec![reused]] {
            let mut backend = FakeDesktop::new(vec![overlay()], clients);
            assert!(execute_paste(&mut backend, "fixture", &plan, "CTRL+V", 77, || true).is_err());
            assert!(backend.actions.contains(&"copy"));
            assert!(!backend.actions.contains(&"focus"));
            assert!(!backend.actions.contains(&"paste"));
        }
    }
    #[test]
    fn focus_change_or_restore_failure_never_pastes() {
        let plan = plan_for(overlay(), Some(&target()), 77).unwrap();
        for active in [
            vec![target()],
            vec![overlay(), target()],
            vec![overlay(), overlay(), overlay()],
        ] {
            let mut backend = FakeDesktop::new(active, vec![target()]);
            assert!(execute_paste(&mut backend, "fixture", &plan, "CTRL+V", 77, || true).is_err());
            assert!(!backend.actions.contains(&"paste"));
        }
        let mut backend = FakeDesktop::new(vec![overlay(), overlay()], vec![target()]);
        backend.focus_fails = true;
        assert!(execute_paste(&mut backend, "fixture", &plan, "CTRL+V", 77, || true).is_err());
        assert!(!backend.actions.contains(&"paste"));
    }
    #[test]
    fn cancelled_operation_does_not_restore_or_inject() {
        let plan = plan_for(overlay(), Some(&target()), 77).unwrap();
        let mut backend = FakeDesktop::new(vec![overlay()], vec![target()]);
        let checks = std::cell::Cell::new(0);
        assert!(
            execute_paste(&mut backend, "fixture", &plan, "CTRL+V", 77, || {
                checks.set(checks.get() + 1);
                checks.get() == 1
            })
            .is_err()
        );
        assert_eq!(backend.actions, vec!["copy", "active", "clients"]);
    }
    #[test]
    fn other_dicta_process_and_unidentified_destination_are_rejected() {
        let mut other = overlay();
        other.pid = 999;
        assert!(validate_target(&other, 77).is_err());
        assert!(plan_for(other, Some(&target()), 77).is_err());
        assert!(plan_for(overlay(), None, 77).is_err());
        let mut unknown = target();
        unknown.stable_id = None;
        assert!(plan_for(overlay(), Some(&unknown), 77).is_err());
        assert_eq!(
            focus_dispatcher(&target()).unwrap(),
            "hl.dsp.focus({ window = \"stableid:test-editor-window\" })"
        );
        let mut invalid = target();
        invalid.address = "0x1\"; arbitrary()".into();
        assert!(focus_dispatcher(&invalid).is_err());
        invalid.address = "0x00".into();
        assert!(validate_target(&invalid, 77).is_err());
    }
    fn overlay_client() -> OverlayClient {
        OverlayClient {
            target: overlay(),
            mapped: true,
            floating: true,
            monitor: 2,
            size: [400., 200.],
        }
    }
    fn monitor() -> OverlayMonitor {
        OverlayMonitor {
            id: 2,
            x: -1920.,
            y: 0.,
            width: 3840.,
            height: 2160.,
            scale: 2.,
            transform: 0,
            disabled: false,
        }
    }
    #[test]
    fn placement_uses_actual_monitor_origin_scale_and_window_bounds() {
        assert_eq!(
            overlay_position(24., 30., [400., 200.], &monitor()),
            Some([-1896, 30])
        );
        assert_eq!(
            overlay_position(5000., 5000., [400., 200.], &monitor()),
            Some([-400, 880])
        );
        assert_eq!(
            overlay_position(-30., -5., [400., 200.], &monitor()),
            Some([-1920, 0])
        );
        let mut rotated = monitor();
        rotated.x = 0.;
        rotated.width = 1920.;
        rotated.height = 1080.;
        rotated.scale = 1.;
        rotated.transform = 1;
        assert_eq!(
            overlay_position(900., 1800., [400., 200.], &rotated),
            Some([680, 1720])
        );
        assert_eq!(
            overlay_position(f32::NAN, 30., [400., 200.], &monitor()),
            None
        );
        rotated.scale = 0.;
        assert_eq!(overlay_position(0., 0., [400., 200.], &rotated), None);
    }
    #[test]
    fn placement_never_selects_settings_foreign_unmapped_or_ambiguous_windows() {
        let own = overlay_client();
        assert!(select_window(std::slice::from_ref(&own), 77, OVERLAY_CLASS).is_some());
        let mut cases = Vec::new();
        let mut other = own.clone();
        other.target.pid = 88;
        cases.push(other);
        let mut settings = own.clone();
        settings.target.class = APP_ID.into();
        cases.push(settings);
        let mut unmapped = own.clone();
        unmapped.mapped = false;
        cases.push(unmapped);
        let mut tiled = own.clone();
        tiled.floating = false;
        cases.push(tiled);
        let mut unstaged = own.clone();
        unstaged.monitor = -1;
        cases.push(unstaged);
        let mut unstable = own.clone();
        unstable.target.stable_id = None;
        cases.push(unstable);
        for invalid in cases {
            assert!(select_window(std::slice::from_ref(&invalid), 77, OVERLAY_CLASS).is_none());
            assert!(window_move_dispatcher(&invalid, [10, 20], 77, OVERLAY_CLASS).is_err());
        }
        assert!(select_window(&[own.clone(), own.clone()], 77, OVERLAY_CLASS).is_none());
        assert_eq!(
            window_move_dispatcher(&own, [-1896, 30], 77, OVERLAY_CLASS).unwrap(),
            "hl.dsp.window.move({ x = -1896, y = 30, relative = false, window = \"stableid:own-overlay\" })"
        );
    }
    #[test]
    fn reader_and_overlay_are_never_paste_targets_or_each_others_placement() {
        let mut reader = overlay();
        reader.pid = 1234;
        reader.class = READER_CLASS.into();
        reader.initial_class = READER_CLASS.into();
        assert!(validate_target(&reader, 77).is_err());
        reader.class = "editor".into();
        assert!(validate_target(&reader, 77).is_err());
        let mut reader_window = overlay_client();
        reader_window.target.class = READER_CLASS.into();
        let overlay_window = overlay_client();
        let both = [overlay_window.clone(), reader_window.clone()];
        assert_eq!(
            select_window(&both, 77, READER_CLASS).map(|c| &c.target),
            Some(&reader_window.target)
        );
        assert_eq!(
            select_window(&both, 77, OVERLAY_CLASS).map(|c| &c.target),
            Some(&overlay_window.target)
        );
        assert!(window_move_dispatcher(&overlay_window, [0, 0], 77, READER_CLASS).is_err());
        assert!(window_move_dispatcher(&reader_window, [0, 0], 77, READER_CLASS).is_ok());
    }
    #[test]
    fn reader_placement_state_is_independent_of_the_overlay() {
        set_placement_enabled(FloatingWindow::Reader, true);
        let reader = READER_PLACEMENT.state.request().unwrap();
        set_placement_enabled(FloatingWindow::Overlay, false);
        let _ = OVERLAY_PLACEMENT.state.request();
        assert!(READER_PLACEMENT.state.current(reader));
        set_placement_enabled(FloatingWindow::Reader, false);
        assert!(!READER_PLACEMENT.state.current(reader));
    }
    #[test]
    fn placement_generations_cancel_old_requests_and_centered_mode() {
        let state = PlacementState::new();
        assert!(state.request().is_none());
        state.set_enabled(true);
        let first = state.request().unwrap();
        assert!(state.current(first));
        let second = state.request().unwrap();
        assert!(!state.current(first));
        assert!(state.current(second));
        state.set_enabled(false);
        assert!(!state.current(second));
        assert!(state.request().is_none());
        state.set_enabled(true);
        assert!(!state.current(second));
        assert!(state.request().is_some());
    }
    #[test]
    fn key_arguments_never_accept_arbitrary_commands_or_enter() {
        assert_eq!(
            paste_args("ctrl+v").unwrap(),
            vec!["-M", "ctrl", "-P", "v", "-p", "v", "-m", "ctrl"]
        );
        for malicious in ["ENTER", "ctrl+v;rm -rf /", "$(touch /tmp/x)", "-k Enter"] {
            assert!(paste_args(malicious).is_err());
        }
        assert!(paste_args("CTRL+SHIFT+V").unwrap().contains(&"shift"));
    }
}
