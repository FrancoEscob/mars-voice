//! Linux backend for [`super::read_text`]: one-shot `wl-paste` reads of the
//! Wayland primary selection or clipboard, also used by the Hyprland copy flow
//! in `copy.rs`. Content never reaches logs or error messages, and wl-paste's
//! stderr is only matched against known phrases, never echoed.
use super::{READ_TIMEOUT, SelectionSource};
use anyhow::{Result, anyhow, bail};
use std::{
    ffi::OsStr,
    io::Read,
    os::unix::process::CommandExt,
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};
use wait_timeout::ChildExt;

/// Preferred first; `text` lets wl-paste pick any textual type (UTF8_STRING…).
const MIME_TYPES: [&str; 3] = ["text/plain;charset=utf-8", "text/plain", "text"];
const STDERR_LIMIT: u64 = 4096;

/// Blocking. Reads text/plain via `wl-paste [--primary] --no-newline --type
/// text/plain;charset=utf-8`, falling back to `text/plain` and `text` when that
/// type is not offered. `Ok(None)` when nothing is selected/copied, the content is
/// only whitespace, or no textual type is offered. Content larger than
/// `max_bytes`, invalid UTF-8 and a source app that does not answer within
/// [`READ_TIMEOUT`] are actionable errors. X11 is not implemented yet.
pub(crate) fn read_text(source: SelectionSource, max_bytes: usize) -> Result<Option<String>> {
    let flavor = match source {
        SelectionSource::Primary => Flavor::Primary,
        SelectionSource::Clipboard => Flavor::Clipboard,
    };
    ensure_wayland(std::env::var_os("WAYLAND_DISPLAY").as_deref(), flavor)?;
    read_with(OsStr::new("wl-paste"), flavor, max_bytes, READ_TIMEOUT)
}

/// What is being read, only to word errors. `Copied` is the clipboard right
/// after `copy::copy_selection` sent the copy shortcut.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Flavor {
    Primary,
    Clipboard,
    Copied,
}
impl Flavor {
    fn primary(self) -> bool {
        self == Self::Primary
    }
    fn noun(self) -> &'static str {
        match self {
            Self::Primary | Self::Copied => "la selección",
            Self::Clipboard => "el portapapeles",
        }
    }
    fn subject(self) -> &'static str {
        match self {
            Self::Primary | Self::Copied => "La selección",
            Self::Clipboard => "El portapapeles",
        }
    }
    fn timeout_error(self) -> anyhow::Error {
        match self {
            Self::Primary => anyhow!(
                "La app de origen no entregó la selección a tiempo. Volvé a intentarlo o copiá el texto y usá Leer portapapeles"
            ),
            Self::Clipboard => anyhow!(
                "La app de origen no entregó el portapapeles a tiempo. Volvé a copiar el texto e intentalo de nuevo"
            ),
            Self::Copied => anyhow!(
                "La app de origen no entregó la selección copiada a tiempo. Volvé a intentarlo"
            ),
        }
    }
    fn too_large_error(self) -> anyhow::Error {
        let action = match self {
            Self::Primary | Self::Copied => "Seleccioná",
            Self::Clipboard => "Copiá",
        };
        anyhow!(
            "{} supera el límite de lectura. {action} menos texto o aumentá el límite en Ajustes > Lectura",
            self.subject()
        )
    }
    fn invalid_text_error(self) -> anyhow::Error {
        anyhow!(
            "{} no contiene texto UTF-8 válido. Copiá el texto de nuevo o pegalo en la ventana de lectura",
            self.subject()
        )
    }
    fn failed_error(self) -> anyhow::Error {
        anyhow!(
            "No se pudo leer {}; comprobá wl-clipboard y la sesión Wayland",
            self.noun()
        )
    }
    fn error(self, failure: Failure) -> anyhow::Error {
        match failure {
            Failure::Timeout => self.timeout_error(),
            Failure::NotFound => anyhow!(
                "Falta wl-paste. Instalá wl-clipboard para leer {}",
                self.noun()
            ),
            Failure::NoPrimary => anyhow!(
                "El compositor no ofrece selección primaria. Copiá el texto y usá Leer portapapeles"
            ),
            Failure::Failed => self.failed_error(),
        }
    }
}

fn ensure_wayland(display: Option<&OsStr>, flavor: Flavor) -> Result<()> {
    if display.is_none_or(OsStr::is_empty) {
        bail!(
            "Leer {} requiere Wayland y wl-clipboard; X11 todavía no está implementado",
            flavor.noun()
        );
    }
    Ok(())
}

/// Outcome of one wl-paste run.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Fetch {
    Bytes(Vec<u8>),
    /// More than the allowed bytes; the child was killed.
    TooLarge,
    /// The requested MIME type is not offered.
    MissingType,
    /// Nothing selected or copied.
    Empty,
}
/// Content-free failure; callers word it for what they were reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Failure {
    Timeout,
    NotFound,
    NoPrimary,
    Failed,
}

pub(super) fn read_with(
    program: &OsStr,
    flavor: Flavor,
    max_bytes: usize,
    timeout: Duration,
) -> Result<Option<String>> {
    let deadline = Instant::now() + timeout;
    for mime in MIME_TYPES {
        let mut args = Vec::with_capacity(4);
        if flavor.primary() {
            args.push("--primary");
        }
        args.extend(["--no-newline", "--type", mime]);
        match fetch(program, &args, max_bytes, deadline).map_err(|f| flavor.error(f))? {
            Fetch::Bytes(bytes) => {
                let text = String::from_utf8(bytes).map_err(|_| flavor.invalid_text_error())?;
                return Ok((!text.trim().is_empty()).then_some(text));
            }
            Fetch::TooLarge => return Err(flavor.too_large_error()),
            Fetch::Empty => return Ok(None),
            Fetch::MissingType => continue,
        }
    }
    Ok(None)
}

/// Kills the whole process group (a wrapper may have forked helpers that keep the
/// pipes open) before reaping the leader, so the group id cannot be reused yet.
struct Group {
    child: Child,
    done: bool,
}
impl Group {
    fn kill(&mut self) {
        if self.done {
            return;
        }
        if let Ok(pgid) = i32::try_from(self.child.id()) {
            // SAFETY: plain syscall; the leader is not reaped yet, so the id is ours.
            unsafe { libc::kill(-pgid, libc::SIGKILL) };
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.done = true;
    }
}
impl Drop for Group {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Runs a wl-paste style reader: stdout capped at `max_bytes`, the whole
/// process group killed on timeout or overflow, stderr only classified.
pub(super) fn fetch(
    program: &OsStr,
    args: &[&str],
    max_bytes: usize,
    deadline: Instant,
) -> Result<Fetch, Failure> {
    let left = || deadline.saturating_duration_since(Instant::now());
    if left().is_zero() {
        return Err(Failure::Timeout);
    }
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let child = command.spawn().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            Failure::NotFound
        } else {
            Failure::Failed
        }
    })?;
    let mut group = Group { child, done: false };
    let stdout = group.child.stdout.take();
    let stderr = group.child.stderr.take();
    let (out_tx, out_rx) = mpsc::channel();
    if let Some(stdout) = stdout {
        let limit = u64::try_from(max_bytes)
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        thread::spawn(move || {
            let mut buf = Vec::new();
            let result = stdout.take(limit).read_to_end(&mut buf).map(|_| buf);
            let _ = out_tx.send(result);
        });
    }
    let (err_tx, err_rx) = mpsc::channel();
    if let Some(stderr) = stderr {
        thread::spawn(move || {
            let mut kept = Vec::new();
            let mut stderr = stderr;
            let _ = (&mut stderr).take(STDERR_LIMIT).read_to_end(&mut kept);
            // Keep draining so a chatty child never blocks on a full pipe.
            let _ = std::io::copy(&mut stderr, &mut std::io::sink());
            let _ = err_tx.send(kept);
        });
    }
    // Ends at EOF or as soon as the limit is exceeded, whichever is first.
    let bytes = match out_rx.recv_timeout(left()) {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(_)) | Err(mpsc::RecvTimeoutError::Disconnected) => {
            group.kill();
            return Err(Failure::Failed);
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            group.kill();
            return Err(Failure::Timeout);
        }
    };
    if bytes.len() > max_bytes {
        group.kill();
        return Ok(Fetch::TooLarge);
    }
    let status = match group.child.wait_timeout(left()) {
        Ok(Some(status)) => status,
        Ok(None) => {
            group.kill();
            return Err(Failure::Timeout);
        }
        Err(_) => {
            group.kill();
            return Err(Failure::Failed);
        }
    };
    group.done = true;
    if status.success() {
        return Ok(Fetch::Bytes(bytes));
    }
    let stderr = err_rx
        .recv_timeout(left().max(Duration::from_millis(50)))
        .unwrap_or_default();
    classify_failure(&String::from_utf8_lossy(&stderr))
}

/// Known wl-clipboard diagnostics across versions. Anything else is a generic,
/// content-free failure.
fn classify_failure(stderr: &str) -> Result<Fetch, Failure> {
    let lower = stderr.to_lowercase();
    if lower.contains("nothing is copied") || lower.contains("no selection") {
        Ok(Fetch::Empty)
    } else if lower.contains("not available as") || lower.contains("no suitable type") {
        Ok(Fetch::MissingType)
    } else if lower.contains("does not seem to support primary") {
        Err(Failure::NoPrimary)
    } else {
        Err(Failure::Failed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        path::{Path, PathBuf},
        sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    };

    // Writing an executable while another test thread forks can make exec fail
    // with ETXTBSY. Serialize script creation and execution in this module.
    static SPAWN: Mutex<()> = Mutex::new(());
    static NEXT: AtomicUsize = AtomicUsize::new(0);

    struct Fake {
        dir: PathBuf,
        path: PathBuf,
    }
    impl Drop for Fake {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
    /// A stand-in for wl-paste. The real binary is never executed by tests.
    fn fake(body: &str) -> Fake {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!(
            "dicta-platform-selection-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("wl-paste");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        Fake { dir, path }
    }
    fn read(path: &Path, flavor: Flavor, max: usize) -> Result<Option<String>> {
        let _guard = SPAWN.lock().unwrap_or_else(|e| e.into_inner());
        read_with(path.as_os_str(), flavor, max, Duration::from_millis(800))
    }

    #[test]
    fn reads_utf8_from_the_requested_selection_without_trailing_newline_flag() {
        let script = fake(
            r#"case " $* " in *" --no-newline --type text/plain;charset=utf-8 "*) ;; *) exit 9;; esac
case " $* " in *" --primary "*) printf 'Hola, ñandú 🙂\nlínea';; *) printf 'portapapeles';; esac"#,
        );
        assert_eq!(
            read(&script.path, Flavor::Primary, 1024).unwrap(),
            Some("Hola, ñandú 🙂\nlínea".into())
        );
        assert_eq!(
            read(&script.path, Flavor::Clipboard, 1024).unwrap(),
            Some("portapapeles".into())
        );
    }

    #[test]
    fn empty_or_whitespace_output_is_no_text() {
        for body in ["exit 0", r"printf ' \n\t '"] {
            let script = fake(body);
            assert_eq!(read(&script.path, Flavor::Primary, 64).unwrap(), None);
        }
    }

    #[test]
    fn nothing_selected_is_no_text_for_old_and_new_wl_paste() {
        for message in ["No selection", "Nothing is copied"] {
            let script = fake(&format!("echo '{message}' >&2; exit 1"));
            assert_eq!(read(&script.path, Flavor::Primary, 64).unwrap(), None);
        }
    }

    #[test]
    fn falls_back_to_plain_text_types_and_reports_no_text_type_as_none() {
        let script = fake(
            r#"for a in "$@"; do mime=$a; done
if [ "$mime" = "text/plain" ]; then printf 'plano'; exit 0; fi
echo "Clipboard content is not available as requested type \"$mime\"" >&2; exit 1"#,
        );
        assert_eq!(
            read(&script.path, Flavor::Clipboard, 64).unwrap(),
            Some("plano".into())
        );
        let image_only =
            fake(r#"echo 'Clipboard content is not available as requested type "x"' >&2; exit 1"#);
        assert_eq!(read(&image_only.path, Flavor::Clipboard, 64).unwrap(), None);
        let old = fake("echo 'No suitable type of content copied' >&2; exit 1");
        assert_eq!(read(&old.path, Flavor::Primary, 64).unwrap(), None);
    }

    #[test]
    fn invalid_utf8_is_an_actionable_error() {
        let script = fake(r"printf '\377\376abc'");
        let error = read(&script.path, Flavor::Primary, 64)
            .unwrap_err()
            .to_string();
        assert!(error.contains("UTF-8"), "{error}");
        assert!(error.contains("ventana de lectura"), "{error}");
    }

    #[test]
    fn oversized_content_stops_reading_and_fails() {
        // An endless writer must end with the size error, not the timeout.
        let endless = fake("exec yes 'texto largo'");
        let error = read(&endless.path, Flavor::Primary, 1000)
            .unwrap_err()
            .to_string();
        assert!(error.contains("límite de lectura"), "{error}");
        let just_over = fake("printf '12345678901'");
        assert!(read(&just_over.path, Flavor::Clipboard, 10).is_err());
        let exact = fake("printf '1234567890'");
        assert_eq!(
            read(&exact.path, Flavor::Clipboard, 10).unwrap(),
            Some("1234567890".into())
        );
    }

    #[test]
    fn hanging_source_is_killed_after_the_timeout() {
        // `sleep` is a separate process holding the pipe: the group kill ends it.
        let script = fake("printf 'parcial'; sleep 30");
        let started = Instant::now();
        let error = {
            let _guard = SPAWN.lock().unwrap_or_else(|e| e.into_inner());
            read_with(
                script.path.as_os_str(),
                Flavor::Primary,
                64,
                Duration::from_millis(250),
            )
            .unwrap_err()
            .to_string()
        };
        assert!(error.contains("a tiempo"), "{error}");
        assert!(error.contains("Leer portapapeles"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn failures_never_echo_content_or_stderr() {
        let script = fake("echo 'secreto del usuario' >&2; exit 3");
        let error = read(&script.path, Flavor::Clipboard, 64)
            .unwrap_err()
            .to_string();
        assert!(!error.contains("secreto"), "{error}");
        assert!(error.contains("wl-clipboard"), "{error}");
        let no_primary =
            fake("echo 'The compositor does not seem to support primary selection' >&2; exit 1");
        assert!(
            read(&no_primary.path, Flavor::Primary, 64)
                .unwrap_err()
                .to_string()
                .contains("Leer portapapeles")
        );
        let missing = Path::new("/nonexistent/dicta-test/wl-paste");
        assert!(
            read(missing, Flavor::Primary, 64)
                .unwrap_err()
                .to_string()
                .contains("Instalá wl-clipboard")
        );
    }

    #[test]
    fn x11_or_missing_display_is_reported_without_running_anything() {
        let error = ensure_wayland(None, Flavor::Primary)
            .unwrap_err()
            .to_string();
        assert!(error.contains("X11"), "{error}");
        assert!(ensure_wayland(Some(OsStr::new("")), Flavor::Clipboard).is_err());
        ensure_wayland(Some(OsStr::new("wayland-1")), Flavor::Primary).unwrap();
    }
}
