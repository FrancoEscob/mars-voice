//! Private, single-instance command transport. Never returns dictated text.
//! A held advisory lock owns the endpoint; the lock inode deliberately survives
//! shutdown so another process cannot race by locking a replacement inode.
//!
//! - Unix: a 0600 socket in the private 0700 runtime folder. Peers are checked
//!   with `SO_PEERCRED` (Linux) or `getpeereid` (macOS and BSD).
//! - Windows: a per-user named pipe with a DACL for the user's SID only
//!   (see `ipc/windows.rs`).
//!
//! The line protocol is identical on every system.
use anyhow::{Context, Result, bail, ensure};
use dicta_core::Command;
#[cfg(unix)]
use dicta_core::fsx;
#[cfg(unix)]
use std::{
    fs::{self, OpenOptions},
    os::{
        fd::AsRawFd,
        unix::{
            fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
            net::{UnixListener, UnixStream},
        },
    },
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
};
use std::{
    fs::{File, TryLockError},
    io::{self, Read, Seek, SeekFrom, Write},
    path::Path,
    sync::mpsc::Sender,
    thread,
    time::{Duration, Instant},
};

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{Server, send, start_with_initial};

#[cfg(unix)]
const SOCKET: &str = "control.sock";
const LOCK: &str = "instance.lock";
const SHORTCUT_LOCK: &str = "shortcut.lock";
const MAX_LINE: usize = 4096;
const REQUEST_TIMEOUT: Duration = Duration::from_millis(750);

#[cfg(unix)]
pub struct Server {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    socket: PathBuf,
    // Drop after listener/socket cleanup.
    _lock: File,
}

/// Bind a singleton server inside the application's private runtime directory.
/// Returns an actionable error if another instance holds the lock. Does not
/// start audio, UI, or any commands by itself.
pub fn start(runtime: &Path, commands: Sender<Command>) -> Result<Server> {
    start_with_initial(runtime, commands, None)
}
/// Initial event is queued before the socket can accept a concurrent release.
#[cfg(unix)]
pub fn start_with_initial(
    runtime: &Path,
    commands: Sender<Command>,
    initial: Option<Command>,
) -> Result<Server> {
    private_runtime(runtime)?;
    // Owner-only, created 0600, never through a link (O_NOFOLLOW).
    let lock = fsx::open_private_rw(&runtime.join(LOCK))
        .context("No se pudo abrir el bloqueo de Dicta")?;
    let metadata = lock.metadata()?;
    ensure!(
        metadata.is_file() && metadata.uid() == effective_uid(),
        "El bloqueo de Dicta no es un archivo propio válido"
    );
    lock.set_permissions(fs::Permissions::from_mode(0o600))?;
    lock.try_lock()
        .context("Dicta ya está ejecutándose; usá dicta ctl settings")?;
    let socket = runtime.join(SOCKET);
    match fs::symlink_metadata(&socket) {
        Ok(meta) => {
            ensure!(
                meta.file_type().is_socket() && meta.uid() == effective_uid(),
                "La ruta control.sock está ocupada por un archivo inesperado; no se eliminó"
            );
            fs::remove_file(&socket).context("No se pudo limpiar el socket anterior")?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let listener =
        UnixListener::bind(&socket).context("No se pudo abrir el socket local de Dicta")?;
    if let Err(error) = fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))
        .and_then(|_| listener.set_nonblocking(true))
    {
        let _ = fs::remove_file(&socket);
        return Err(error.into());
    }
    if let Some(initial) = initial
        && commands.send(initial).is_err()
    {
        let _ = fs::remove_file(&socket);
        bail!("El controlador no pudo recibir el atajo inicial");
    }
    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = stop.clone();
    let worker = match thread::Builder::new()
        .name("dicta-ipc".into())
        .spawn(move || {
            while !worker_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = handle_client(&mut stream, &commands);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::park_timeout(Duration::from_millis(20))
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
        }) {
        Ok(worker) => worker,
        Err(error) => {
            let _ = fs::remove_file(&socket);
            return Err(error.into());
        }
    };
    Ok(Server {
        stop,
        worker: Some(worker),
        socket,
        _lock: lock,
    })
}

#[cfg(unix)]
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            let _ = worker.join();
        }
        let _ = fs::remove_file(&self.socket);
        // The File drop releases flock. Leave instance.lock's inode in place.
    }
}

#[cfg(unix)]
fn effective_uid() -> u32 {
    // SAFETY: geteuid has no pointer arguments or preconditions.
    unsafe { libc::geteuid() }
}
#[cfg(unix)]
fn private_runtime(runtime: &Path) -> Result<()> {
    // mkdir is atomic: concurrent cold launchers can both initialize this path.
    // Existing paths are still untrusted until the opened inode is validated.
    // Never create or chmod arbitrary XDG_RUNTIME_DIR ancestor directories.
    match fs::DirBuilder::new().mode(0o700).create(runtime) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(error)
                .context("No se pudo crear la carpeta runtime; comprobá XDG_RUNTIME_DIR");
        }
    }
    // std already opens with O_CLOEXEC.
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(runtime)
        .context("La carpeta runtime debe ser un directorio propio, no un enlace")?;
    let metadata = directory.metadata()?;
    ensure!(
        metadata.is_dir() && metadata.uid() == effective_uid(),
        "La carpeta runtime debe ser un directorio propio, no un enlace"
    );
    // Operate on the validated inode, not a pathname that could be replaced
    // between the ownership/type check and chmod.
    directory.set_permissions(fs::Permissions::from_mode(0o700))?;
    Ok(())
}
#[cfg(unix)]
fn open_shortcut_lock(runtime: &Path) -> Result<File> {
    private_runtime(runtime)?;
    let file = fsx::open_private_rw(&runtime.join(SHORTCUT_LOCK))?;
    let meta = file.metadata()?;
    ensure!(
        meta.is_file() && meta.uid() == effective_uid(),
        "Bloqueo de atajo inválido"
    );
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    Ok(file)
}
#[cfg(windows)]
use windows::open_shortcut_lock;

/// Serialize shortcut launchers across cold startup. A release leaves a fence
/// even without a resident, so an older delayed press cannot open the mic.
pub struct ShortcutLaunch {
    file: File,
}
impl ShortcutLaunch {
    pub fn lock(runtime: &Path) -> Result<Self> {
        let file = open_shortcut_lock(runtime)?;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Self { file }),
                Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(5))
                }
                Err(e) => {
                    return Err(io::Error::from(e)).context("No se pudo ordenar el atajo de Dicta");
                }
            }
        }
    }
    fn last_release(&mut self) -> Result<(u64, u64)> {
        self.file.seek(SeekFrom::Start(0))?;
        let mut value = String::new();
        (&mut self.file).take(64).read_to_string(&mut value)?;
        let mut fields = value.split_whitespace();
        Ok((
            fields.next().and_then(|s| s.parse().ok()).unwrap_or(0),
            fields.next().and_then(|s| s.parse().ok()).unwrap_or(0),
        ))
    }
    pub fn was_released(&mut self, at: u64, token: Option<u64>) -> Result<bool> {
        let (last_at, last_token) = self.last_release()?;
        Ok(at <= last_at || token.is_some_and(|token| token <= last_token))
    }
    pub fn release(&mut self, at: u64, token: Option<u64>) -> Result<()> {
        let (last_at, last_token) = self.last_release()?;
        self.file.seek(SeekFrom::Start(0))?;
        self.file.set_len(0)?;
        write!(
            self.file,
            "{} {}",
            at.max(last_at),
            token.unwrap_or(0).max(last_token)
        )?;
        self.file.sync_data()?;
        Ok(())
    }
}
fn parse(command: &str) -> Result<Option<Command>> {
    for (prefix, pressed) in [("shortcut-press:", true), ("shortcut-release:", false)] {
        if let Some(stamp) = command.strip_prefix(prefix) {
            let (stamp, token) = stamp.split_once(':').unwrap_or((stamp, "0"));
            let token = token.parse::<u64>().context("Token de atajo inválido")?;
            let token = if token == 0 { None } else { Some(token) };
            let at = stamp
                .parse::<u64>()
                .context("Timestamp de atajo inválido")?;
            ensure!(at > 0, "Timestamp de atajo inválido");
            return Ok(Some(if pressed {
                Command::ShortcutPressed { at, token }
            } else {
                Command::ShortcutReleased { at, token }
            }));
        }
    }
    Ok(Some(match command {
        "start" => Command::Start,
        "stop" => Command::Stop,
        "toggle" => Command::Toggle,
        "cancel" => Command::Cancel,
        "copy" => Command::Copy,
        "paste" => Command::Paste,
        "settings" => Command::ShowSettings,
        "show" => Command::ShowOverlay,
        "hide" => Command::HideOverlay,
        "quit" => Command::Quit,
        "read" => Command::ReadSelection,
        "read-clipboard" => Command::ReadClipboard,
        "tts-toggle" => Command::TtsToggle,
        "tts-stop" => Command::TtsStop,
        "status" => return Ok(None),
        _ => bail!(
            "Comando desconocido. Usá start, stop, toggle, cancel, copy, paste, settings, show, hide, read, read-clipboard, tts-toggle, tts-stop, quit o status"
        ),
    }))
}
/// The reply to one request line. Never echoes untrusted input or arbitrary
/// I/O diagnostics.
fn respond(request: Result<String>, commands: &Sender<Command>) -> &'static str {
    match request.and_then(|line| parse(&line)) {
        Ok(None) => "OK Dicta está ejecutándose\n",
        Ok(Some(command)) => match commands.send(command) {
            Ok(()) => "OK comando recibido\n",
            Err(_) => "ERR el controlador de Dicta no está disponible\n",
        },
        Err(_) => "ERR comando inválido, incompleto o demasiado largo\n",
    }
}
/// Reads one `\n`-terminated UTF-8 line shorter than [`MAX_LINE`] before
/// `timeout`. `read` gets the time left and returns 0 at end of stream.
fn read_line_with(
    timeout: Duration,
    mut read: impl FnMut(&mut [u8], Duration) -> io::Result<usize>,
) -> Result<String> {
    let deadline = Instant::now() + timeout;
    let mut line = Vec::new();
    let mut buffer = [0u8; 512];
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .context("Se agotó el tiempo de lectura IPC")?;
        let count = read(&mut buffer, remaining.max(Duration::from_millis(1)))
            .context("No se recibió una línea IPC completa a tiempo")?;
        ensure!(
            count > 0,
            "La conexión cerró antes de completar una línea IPC"
        );
        let newline = buffer[..count].iter().position(|byte| *byte == b'\n');
        let take = newline.unwrap_or(count);
        ensure!(line.len() + take < MAX_LINE, "La línea IPC supera 4 KiB");
        line.extend_from_slice(&buffer[..take]);
        if newline.is_some() {
            return String::from_utf8(line).context("La línea IPC no es UTF-8");
        }
    }
}

/// UID of the process at the other end of a connected Unix socket.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn peer_uid(stream: &UnixStream) -> Option<u32> {
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: both pointers refer to initialized, correctly sized storage and
    // remain valid for getsockopt. The fd belongs to the live UnixStream.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut credentials as *mut libc::ucred).cast(),
            &mut length,
        )
    };
    (result == 0 && length as usize == std::mem::size_of::<libc::ucred>())
        .then_some(credentials.uid)
}
/// UID of the process at the other end of a connected Unix socket.
#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
fn peer_uid(stream: &UnixStream) -> Option<u32> {
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    // SAFETY: both pointers refer to initialized storage that outlives the
    // call. The fd belongs to the live UnixStream.
    let result = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
    (result == 0).then_some(uid)
}
#[cfg(unix)]
fn same_user(stream: &UnixStream) -> Result<()> {
    let uid = peer_uid(stream).context("No se pudo validar el usuario del socket")?;
    ensure!(uid == effective_uid(), "El socket pertenece a otro usuario");
    Ok(())
}
#[cfg(unix)]
fn read_line(stream: &mut UnixStream, timeout: Duration) -> Result<String> {
    read_line_with(timeout, |buffer, remaining| {
        stream.set_read_timeout(Some(remaining))?;
        stream.read(buffer)
    })
}
#[cfg(unix)]
fn handle_client(stream: &mut UnixStream, commands: &Sender<Command>) -> Result<()> {
    same_user(stream)?;
    stream.set_write_timeout(Some(REQUEST_TIMEOUT))?;
    let response = respond(read_line(stream, REQUEST_TIMEOUT), commands);
    stream.write_all(response.as_bytes())?;
    Ok(())
}

/// Send one allowlisted command and await an ACK. The response confirms delivery
/// only; status reports process availability and never includes dictated text.
#[cfg(unix)]
pub fn send(runtime: &Path, command: &str) -> Result<String> {
    parse(command)?;
    let metadata =
        fs::symlink_metadata(runtime).context("Dicta no está ejecutándose en esta sesión")?;
    ensure!(
        metadata.is_dir()
            && !metadata.file_type().is_symlink()
            && metadata.uid() == effective_uid()
            && metadata.mode() & 0o077 == 0,
        "La carpeta runtime de Dicta no es privada"
    );
    let socket = runtime.join(SOCKET);
    let metadata = fs::symlink_metadata(&socket)
        .context("Dicta no está ejecutándose; iniciá la aplicación primero")?;
    ensure!(
        metadata.file_type().is_socket()
            && metadata.uid() == effective_uid()
            && metadata.mode() & 0o077 == 0,
        "El socket de Dicta no es privado"
    );
    let mut stream = UnixStream::connect(socket)
        .context("No se pudo contactar Dicta; puede quedar un socket de una sesión anterior")?;
    same_user(&stream)?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    stream.write_all(command.as_bytes())?;
    stream.write_all(b"\n")?;
    let response = read_line(&mut stream, Duration::from_secs(2))?;
    ensure!(response.starts_with("OK "), "{response}");
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dicta_core::fsx;
    use std::{
        fs,
        path::PathBuf,
        sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
            mpsc,
        },
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "dicta-ipc-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[cfg(unix)]
    #[test]
    fn concurrent_runtime_creation_accepts_only_private_owned_directories() {
        let temp = Temp::new();
        for round in 0..8 {
            let runtime = temp.0.join(format!("concurrent-{round}"));
            let barrier = Arc::new(std::sync::Barrier::new(16));
            let workers: Vec<_> = (0..16)
                .map(|_| {
                    let runtime = runtime.clone();
                    let barrier = barrier.clone();
                    thread::spawn(move || {
                        barrier.wait();
                        private_runtime(&runtime)
                    })
                })
                .collect();
            for worker in workers {
                worker.join().unwrap().unwrap();
            }
            assert_eq!(fs::metadata(&runtime).unwrap().mode() & 0o777, 0o700);
        }
        let regular = temp.0.join("regular");
        fs::write(&regular, "untouched").unwrap();
        fs::set_permissions(&regular, fs::Permissions::from_mode(0o640)).unwrap();
        assert!(private_runtime(&regular).is_err());
        assert_eq!(fs::metadata(&regular).unwrap().mode() & 0o777, 0o640);
        let target = temp.0.join("target");
        fs::create_dir(&target).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o750)).unwrap();
        let link = temp.0.join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(private_runtime(&link).is_err());
        assert_eq!(fs::metadata(&target).unwrap().mode() & 0o777, 0o750);
    }
    #[test]
    fn singleton_permissions_dispatch_and_restart() {
        let temp = Temp::new();
        let runtime = temp.0.join("dicta");
        let (tx, rx) = mpsc::channel();
        let server = start(&runtime, tx.clone()).unwrap();
        #[cfg(unix)]
        {
            assert_eq!(fs::metadata(&runtime).unwrap().mode() & 0o777, 0o700);
            assert_eq!(
                fs::metadata(runtime.join(SOCKET)).unwrap().mode() & 0o777,
                0o600
            );
        }
        assert!(start(&runtime, tx.clone()).is_err());
        assert_eq!(
            send(&runtime, "status").unwrap(),
            "OK Dicta está ejecutándose"
        );
        assert!(rx.try_recv().is_err());
        send(&runtime, "toggle").unwrap();
        assert!(matches!(
            rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            Command::Toggle
        ));
        assert!(send(&runtime, "paste\nquit").is_err());
        drop(server);
        #[cfg(unix)]
        assert!(!runtime.join(SOCKET).exists());
        // Without a resident, the launcher sees "not found" and may start one.
        let absent = send(&runtime, "status").unwrap_err();
        assert!(
            absent.chain().any(|cause| cause
                .downcast_ref::<io::Error>()
                .is_some_and(|error| error.kind() == io::ErrorKind::NotFound)),
            "{absent:#}"
        );
        let _restarted = start(&runtime, tx).unwrap();
    }
    #[test]
    fn shortcut_fence_persists_and_its_lock_is_exclusive() {
        let temp = Temp::new();
        let runtime = temp.0.join("dicta");
        let mut guard = ShortcutLaunch::lock(&runtime).unwrap();
        assert!(!guard.was_released(10, Some(3)).unwrap());
        guard.release(10, Some(3)).unwrap();
        assert!(guard.was_released(10, None).unwrap());
        assert!(guard.was_released(11, Some(3)).unwrap());
        assert!(!guard.was_released(11, Some(4)).unwrap());
        // A lower stamp never moves the fence back.
        guard.release(5, Some(1)).unwrap();
        assert!(guard.was_released(10, Some(3)).unwrap());
        let other = fsx::open_private_rw(&runtime.join(SHORTCUT_LOCK)).unwrap();
        assert!(matches!(other.try_lock(), Err(TryLockError::WouldBlock)));
        drop(guard);
        other.try_lock().unwrap();
        drop(other);
        let mut again = ShortcutLaunch::lock(&runtime).unwrap();
        assert!(again.was_released(10, Some(3)).unwrap());
        assert!(!again.was_released(11, Some(4)).unwrap());
    }
    #[cfg(unix)]
    #[test]
    fn peer_check_reports_the_connected_user() {
        let (left, right) = UnixStream::pair().unwrap();
        assert_eq!(peer_uid(&left), Some(effective_uid()));
        assert_eq!(peer_uid(&right), Some(effective_uid()));
        same_user(&left).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn stale_socket_cleanup_preserves_unexpected_files() {
        let temp = Temp::new();
        let runtime = temp.0.join("dicta");
        private_runtime(&runtime).unwrap();
        let stale = UnixListener::bind(runtime.join(SOCKET)).unwrap();
        drop(stale);
        let (tx, _rx) = mpsc::channel();
        let server = start(&runtime, tx.clone()).unwrap();
        drop(server);
        fs::write(runtime.join(SOCKET), "keep me").unwrap();
        assert!(start(&runtime, tx).is_err());
        assert_eq!(fs::read_to_string(runtime.join(SOCKET)).unwrap(), "keep me");
    }
    #[cfg(unix)]
    #[test]
    fn overlong_client_is_rejected_without_dispatch() {
        let temp = Temp::new();
        let runtime = temp.0.join("dicta");
        let (tx, rx) = mpsc::channel();
        let _server = start(&runtime, tx).unwrap();
        let mut stream = UnixStream::connect(runtime.join(SOCKET)).unwrap();
        stream.write_all(&vec![b'a'; MAX_LINE + 1]).unwrap();
        let response = read_line(&mut stream, Duration::from_secs(1)).unwrap();
        assert!(response.starts_with("ERR "));
        assert!(rx.try_recv().is_err());
    }
    #[cfg(unix)]
    #[test]
    fn incomplete_client_cannot_block_shutdown_indefinitely() {
        let temp = Temp::new();
        let runtime = temp.0.join("dicta");
        let (tx, rx) = mpsc::channel();
        let server = start(&runtime, tx).unwrap();
        let mut stream = UnixStream::connect(runtime.join(SOCKET)).unwrap();
        stream.write_all(b"toggle").unwrap();
        let response = read_line(&mut stream, Duration::from_secs(2)).unwrap();
        assert!(response.starts_with("ERR "));
        assert!(rx.try_recv().is_err());
        drop(server);
        assert!(!runtime.join(SOCKET).exists());
    }
    #[cfg(unix)]
    #[test]
    fn refuses_runtime_and_lock_symlinks() {
        use std::os::unix::fs::symlink;
        let temp = Temp::new();
        let real = temp.0.join("real");
        fs::create_dir(&real).unwrap();
        let alias = temp.0.join("alias");
        symlink(&real, &alias).unwrap();
        let (tx, _) = mpsc::channel();
        assert!(start(&alias, tx.clone()).is_err());
        fs::write(real.join("other"), "untouched").unwrap();
        symlink(real.join("other"), real.join(LOCK)).unwrap();
        assert!(start(&real, tx).is_err());
    }
}
