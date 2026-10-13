//! Windows transport: one named pipe per user and runtime folder.
//!
//! - Name: `\\.\pipe\io.dicta.Dicta.<SID>` (`…Dicta.Demo.<SID>` for the demo).
//! - Protected DACL granting only the current user's SID; remote clients are
//!   rejected and the first instance must be ours.
//! - Each side checks that the process at the other end runs as the same SID.
//! - Overlapped I/O with deadlines, so a silent client cannot block shutdown.
//!
//! The single instance itself is the `instance.lock` file, as on Unix.
use super::{LOCK, MAX_LINE, REQUEST_TIMEOUT, SHORTCUT_LOCK, parse, read_line_with, respond};
use anyhow::{Context, Result, bail, ensure};
use dicta_core::{Command, fsx};
use std::{
    fs::File,
    io, mem,
    path::Path,
    ptr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::Sender,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use windows_sys::{
    Win32::{
        Foundation::{
            CloseHandle, ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_NO_DATA, ERROR_PIPE_BUSY,
            ERROR_PIPE_CONNECTED, ERROR_PIPE_NOT_CONNECTED, GENERIC_READ, GENERIC_WRITE,
            GetLastError, HANDLE, INVALID_HANDLE_VALUE, LocalFree, WAIT_OBJECT_0, WAIT_TIMEOUT,
        },
        Security::{
            Authorization::{
                ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
                SDDL_REVISION_1,
            },
            EqualSid, GetTokenInformation, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES,
            TOKEN_QUERY, TOKEN_USER, TokenUser,
        },
        Storage::FileSystem::{
            CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, OPEN_EXISTING,
            PIPE_ACCESS_DUPLEX, ReadFile, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT,
            WriteFile,
        },
        System::{
            IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED},
            Pipes::{
                ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe,
                GetNamedPipeClientProcessId, GetNamedPipeServerProcessId, PIPE_READMODE_BYTE,
                PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT, WaitNamedPipeW,
            },
            Threading::{
                CreateEventW, GetCurrentProcess, OpenProcess, OpenProcessToken,
                PROCESS_QUERY_LIMITED_INFORMATION, WaitForSingleObject,
            },
        },
    },
    core::{BOOL, PWSTR},
};

const BUFFER: u32 = 4096;
const CLIENT_TIMEOUT: Duration = Duration::from_secs(2);

pub struct Server {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    // Released after the worker closed the pipe.
    _lock: File,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Initial event is queued before the worker accepts a concurrent release.
pub fn start_with_initial(
    runtime: &Path,
    commands: Sender<Command>,
    initial: Option<Command>,
) -> Result<Server> {
    private_runtime(runtime)?;
    let lock = fsx::open_private_rw(&runtime.join(LOCK))
        .context("No se pudo abrir el bloqueo de Dicta")?;
    ensure!(
        lock.metadata()?.is_file(),
        "El bloqueo de Dicta no es un archivo propio válido"
    );
    lock.try_lock()
        .context("Dicta ya está ejecutándose; usá dicta ctl settings")?;
    let user = UserSid::current().context("No se pudo identificar al usuario de Windows")?;
    let pipe = create_pipe(&pipe_name(runtime, &user)?, &user)
        .context("No se pudo abrir el canal local de Dicta")?;
    if let Some(initial) = initial
        && commands.send(initial).is_err()
    {
        bail!("El controlador no pudo recibir el atajo inicial");
    }
    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = stop.clone();
    let worker = thread::Builder::new()
        .name("dicta-ipc".into())
        .spawn(move || serve(pipe, user, commands, worker_stop))?;
    Ok(Server {
        stop,
        worker: Some(worker),
        _lock: lock,
    })
}

/// Send one allowlisted command and await an ACK, as on Unix.
pub fn send(runtime: &Path, command: &str) -> Result<String> {
    parse(command)?;
    let user = UserSid::current().context("No se pudo identificar al usuario de Windows")?;
    let pipe = connect(&pipe_name(runtime, &user)?, CLIENT_TIMEOUT).map_err(|error| {
        // Keep the io::Error in the chain: NotFound means no resident.
        let message = if error.kind() == io::ErrorKind::NotFound {
            "Dicta no está ejecutándose; iniciá la aplicación primero"
        } else {
            "No se pudo contactar Dicta; el canal local está ocupado o no responde"
        };
        anyhow::Error::new(error).context(message)
    })?;
    ensure!(
        peer_is_user(&pipe, &user, Peer::Server),
        "El canal de Dicta pertenece a otro usuario"
    );
    let deadline = Instant::now() + CLIENT_TIMEOUT;
    write_all(&pipe, command.as_bytes(), deadline)?;
    write_all(&pipe, b"\n", deadline)?;
    let response = read_line(&pipe, CLIENT_TIMEOUT)?;
    ensure!(response.starts_with("OK "), "{response}");
    Ok(response)
}

/// `%LOCALAPPDATA%` is already private to the user; refuse links/junctions.
fn private_runtime(runtime: &Path) -> Result<()> {
    fsx::ensure_private_dir(runtime)
        .context("La carpeta runtime debe ser un directorio propio, no un enlace")
}
pub(super) fn open_shortcut_lock(runtime: &Path) -> Result<File> {
    private_runtime(runtime)?;
    let file = fsx::open_private_rw(&runtime.join(SHORTCUT_LOCK))?;
    ensure!(file.metadata()?.is_file(), "Bloqueo de atajo inválido");
    Ok(file)
}

fn serve(pipe: Handle, user: UserSid, commands: Sender<Command>, stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::Acquire) {
        let connected = overlapped(&pipe, Limit::Stop(&stop), true, |ov| {
            // SAFETY: valid pipe handle; `ov` stays alive until completion.
            unsafe { ConnectNamedPipe(pipe.0, ov) }
        });
        match connected {
            Ok(_) => {
                let _ = handle_client(&pipe, &user, &commands);
            }
            Err(_) if stop.load(Ordering::Acquire) => break,
            // The client left before it was served.
            Err(error) if error.raw_os_error() == Some(ERROR_NO_DATA as i32) => {}
            Err(_) => break,
        }
        // SAFETY: valid pipe handle owned by this thread.
        unsafe { DisconnectNamedPipe(pipe.0) };
    }
}
fn handle_client(pipe: &Handle, user: &UserSid, commands: &Sender<Command>) -> Result<()> {
    ensure!(
        peer_is_user(pipe, user, Peer::Client),
        "El canal pertenece a otro usuario"
    );
    let response = respond(read_line(pipe, REQUEST_TIMEOUT), commands);
    write_all(pipe, response.as_bytes(), Instant::now() + REQUEST_TIMEOUT)?;
    // Disconnecting discards unread data: let the client read the reply and
    // close its end first, within the same bound as the request.
    let deadline = Instant::now() + REQUEST_TIMEOUT;
    let mut rest = [0u8; 64];
    while read(pipe, &mut rest, deadline).is_ok_and(|count| count > 0) {}
    Ok(())
}

/// `\\.\pipe\io.dicta.Dicta[.Demo].<SID>` for the installed runtime folders
/// (`%LOCALAPPDATA%\dicta\run[-demo]`). Any other folder (tests, custom
/// setups) gets a stable per-folder suffix so instances never collide.
fn pipe_name(runtime: &Path, user: &UserSid) -> io::Result<Vec<u16>> {
    let sid = user.sid_string()?;
    let in_dicta = runtime
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|name| name.eq_ignore_ascii_case("dicta"));
    let folder = runtime.file_name().unwrap_or_default();
    let name = if in_dicta && folder.eq_ignore_ascii_case("run") {
        format!(r"\\.\pipe\io.dicta.Dicta.{sid}")
    } else if in_dicta && folder.eq_ignore_ascii_case("run-demo") {
        format!(r"\\.\pipe\io.dicta.Dicta.Demo.{sid}")
    } else {
        format!(r"\\.\pipe\io.dicta.Dicta.{sid}.{:016x}", fnv1a(runtime))
    };
    Ok(wide(&name))
}
/// Paths are case-insensitive on Windows.
fn fnv1a(path: &Path) -> u64 {
    path.to_string_lossy()
        .to_lowercase()
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
        })
}
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

fn create_pipe(name: &[u16], user: &UserSid) -> io::Result<Handle> {
    // Protected DACL: full access for this user's SID and nobody else.
    let sddl = wide(&format!("D:P(A;;GA;;;{})", user.sid_string()?));
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: NUL-terminated input; on success `descriptor` is LocalAlloc'd.
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            ptr::null_mut(),
        )
    };
    if converted == 0 {
        return Err(io::Error::last_os_error());
    }
    let attributes = SECURITY_ATTRIBUTES {
        nLength: mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    // SAFETY: NUL-terminated name; `attributes` and `descriptor` outlive the
    // call. FIRST_PIPE_INSTANCE fails if anyone else already owns the name.
    let raw = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE | FILE_FLAG_OVERLAPPED,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            BUFFER,
            BUFFER,
            0,
            &attributes,
        )
    };
    // Reads the error before LocalFree can overwrite it.
    let pipe = Handle::new(raw);
    // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
    unsafe { LocalFree(descriptor) };
    pipe
}
/// Opens the client end. A missing pipe is `ErrorKind::NotFound`
/// (`ERROR_FILE_NOT_FOUND`); a busy one is retried until `timeout`.
fn connect(name: &[u16], timeout: Duration) -> io::Result<Handle> {
    let deadline = Instant::now() + timeout;
    loop {
        // SAFETY: NUL-terminated name; null security attributes and template
        // are allowed. Identification level: the server cannot impersonate us.
        let raw = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                ptr::null_mut(),
            )
        };
        match Handle::new(raw) {
            Ok(pipe) => return Ok(pipe),
            Err(error) if error.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "canal ocupado"));
                }
                // SAFETY: NUL-terminated name. Returns early if it vanishes.
                unsafe { WaitNamedPipeW(name.as_ptr(), left.as_millis().clamp(1, 500) as u32) };
            }
            Err(error) => return Err(error),
        }
    }
}

enum Peer {
    Client,
    Server,
}
/// True if the process at the `peer` end runs as `user`'s SID.
fn peer_is_user(pipe: &Handle, user: &UserSid, peer: Peer) -> bool {
    let mut pid = 0u32;
    // SAFETY: valid pipe handle and out pointer.
    let found = unsafe {
        match peer {
            Peer::Client => GetNamedPipeClientProcessId(pipe.0, &mut pid),
            Peer::Server => GetNamedPipeServerProcessId(pipe.0, &mut pid),
        }
    };
    found != 0 && UserSid::of_pid(pid).is_ok_and(|other| other.same_as(user))
}

fn read_line(pipe: &Handle, timeout: Duration) -> Result<String> {
    read_line_with(timeout, |buffer, remaining| {
        read(pipe, buffer, Instant::now() + remaining)
    })
}
/// Returns 0 when the other end closed.
fn read(pipe: &Handle, buffer: &mut [u8], deadline: Instant) -> io::Result<usize> {
    let length = buffer.len().min(u32::MAX as usize) as u32;
    let data = buffer.as_mut_ptr();
    let result = overlapped(pipe, Limit::Until(deadline), false, |ov| {
        // SAFETY: `buffer` outlives the operation, which `overlapped` awaits.
        unsafe { ReadFile(pipe.0, data, length, ptr::null_mut(), ov) }
    });
    match result {
        Ok(count) => Ok(count as usize),
        Err(error)
            if matches!(
                error.raw_os_error(),
                Some(code) if code == ERROR_BROKEN_PIPE as i32 || code == ERROR_PIPE_NOT_CONNECTED as i32
            ) =>
        {
            Ok(0)
        }
        Err(error) => Err(error),
    }
}
fn write_all(pipe: &Handle, mut data: &[u8], deadline: Instant) -> io::Result<()> {
    while !data.is_empty() {
        let length = data.len().min(u32::MAX as usize) as u32;
        let bytes = data.as_ptr();
        let written = overlapped(pipe, Limit::Until(deadline), false, |ov| {
            // SAFETY: `data` outlives the operation, which `overlapped` awaits.
            unsafe { WriteFile(pipe.0, bytes, length, ptr::null_mut(), ov) }
        })? as usize;
        if written == 0 {
            return Err(io::ErrorKind::WriteZero.into());
        }
        data = &data[written.min(data.len())..];
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum Limit<'a> {
    Until(Instant),
    Stop(&'a AtomicBool),
}
/// Starts one overlapped operation and waits for it. On timeout or stop the
/// operation is cancelled and awaited, so the kernel never touches the buffer
/// or the OVERLAPPED after this returns. `connect` treats a client that was
/// already connected as success.
fn overlapped(
    pipe: &Handle,
    limit: Limit,
    connect: bool,
    start: impl FnOnce(*mut OVERLAPPED) -> BOOL,
) -> io::Result<u32> {
    // SAFETY: unnamed manual-reset event, initially unsignalled.
    let event = Handle::new(unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) })?;
    // Boxed so its address stays fixed while the kernel may write to it.
    // SAFETY: OVERLAPPED is plain data; all-zero is its documented initial state.
    let mut state: Box<OVERLAPPED> = Box::new(unsafe { mem::zeroed() });
    state.hEvent = event.0;
    let ov: *mut OVERLAPPED = &mut *state;
    if start(ov) == 0 {
        // SAFETY: no preconditions; read right after the failed call.
        let error = unsafe { GetLastError() };
        if connect && error == ERROR_PIPE_CONNECTED {
            return Ok(0);
        }
        if error != ERROR_IO_PENDING {
            return Err(io::Error::from_raw_os_error(error as i32));
        }
        wait(pipe, &event, ov, limit)?;
    }
    let mut transferred = 0u32;
    // SAFETY: the operation completed; `ov` is still alive.
    if unsafe { GetOverlappedResult(pipe.0, ov, &mut transferred, 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(transferred)
}
fn wait(pipe: &Handle, event: &Handle, ov: *mut OVERLAPPED, limit: Limit) -> io::Result<()> {
    loop {
        let slice_ms = match limit {
            Limit::Until(deadline) => {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    break;
                }
                left.as_millis().clamp(1, 1000) as u32
            }
            Limit::Stop(stop) => {
                if stop.load(Ordering::Acquire) {
                    break;
                }
                50
            }
        };
        // SAFETY: valid event handle.
        match unsafe { WaitForSingleObject(event.0, slice_ms) } {
            WAIT_OBJECT_0 => return Ok(()),
            WAIT_TIMEOUT => {}
            _ => {
                let error = io::Error::last_os_error();
                cancel(pipe, ov);
                return Err(error);
            }
        }
    }
    cancel(pipe, ov);
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "tiempo de IPC agotado",
    ))
}
fn cancel(pipe: &Handle, ov: *mut OVERLAPPED) {
    let mut ignored = 0u32;
    // SAFETY: `ov` belongs to an operation started on `pipe`. Waiting for its
    // completion (bWait = TRUE) guarantees the kernel is done with it.
    unsafe {
        CancelIoEx(pipe.0, ov);
        GetOverlappedResult(pipe.0, ov, &mut ignored, 1);
    }
}

struct Handle(HANDLE);
// SAFETY: kernel handles can be used and closed from any thread.
unsafe impl Send for Handle {}
impl Handle {
    /// Wraps a handle returned by Win32, or the last error if it failed.
    fn new(raw: HANDLE) -> io::Result<Self> {
        if raw.is_null() || raw == INVALID_HANDLE_VALUE {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self(raw))
        }
    }
}
impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: owned, valid handle closed exactly once.
        unsafe { CloseHandle(self.0) };
    }
}

/// The user SID of a process, from its `TOKEN_USER`.
struct UserSid {
    // u64 storage keeps TOKEN_USER (which holds a pointer) aligned.
    buffer: Vec<u64>,
}
impl UserSid {
    fn current() -> io::Result<Self> {
        // SAFETY: the current-process pseudo handle needs no closing.
        Self::of_process(unsafe { GetCurrentProcess() })
    }
    fn of_pid(pid: u32) -> io::Result<Self> {
        // SAFETY: plain call; the handle is checked and owned below.
        let process =
            Handle::new(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) })?;
        Self::of_process(process.0)
    }
    fn of_process(process: HANDLE) -> io::Result<Self> {
        let mut raw: HANDLE = ptr::null_mut();
        // SAFETY: valid process handle and out pointer.
        if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut raw) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let token = Handle::new(raw)?;
        let mut length = 0u32;
        // SAFETY: a null buffer of length 0 only asks for the required size.
        unsafe { GetTokenInformation(token.0, TokenUser, ptr::null_mut(), 0, &mut length) };
        if (length as usize) < mem::size_of::<TOKEN_USER>() {
            return Err(io::Error::last_os_error());
        }
        let mut buffer = vec![0u64; (length as usize).div_ceil(8)];
        // SAFETY: `buffer` has at least `length` writable, 8-aligned bytes.
        if unsafe {
            GetTokenInformation(
                token.0,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                length,
                &mut length,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { buffer })
    }
    fn sid(&self) -> PSID {
        // SAFETY: GetTokenInformation(TokenUser) filled an aligned TOKEN_USER
        // whose SID points inside `buffer`, which lives as long as `self`.
        unsafe { (*self.buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid }
    }
    fn same_as(&self, other: &Self) -> bool {
        // SAFETY: both SIDs are valid for the lifetime of the borrows.
        unsafe { EqualSid(self.sid(), other.sid()) != 0 }
    }
    fn sid_string(&self) -> io::Result<String> {
        let mut raw: PWSTR = ptr::null_mut();
        // SAFETY: valid SID; on success `raw` is a LocalAlloc'd C string.
        if unsafe { ConvertSidToStringSidW(self.sid(), &mut raw) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `raw` is NUL-terminated UTF-16 until LocalFree.
        let text = unsafe {
            let length = (0..).take_while(|&i| *raw.add(i) != 0).count();
            String::from_utf16_lossy(std::slice::from_raw_parts(raw, length))
        };
        // SAFETY: allocated by ConvertSidToStringSidW.
        unsafe { LocalFree(raw.cast()) };
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        path::PathBuf,
        sync::{atomic::AtomicU64, mpsc},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "dicta-pipe-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn text(name: &[u16]) -> String {
        String::from_utf16(name.strip_suffix(&[0]).unwrap()).unwrap()
    }

    #[test]
    fn pipe_names_are_per_user_with_separate_demo_and_test_folders() {
        let user = UserSid::current().unwrap();
        let sid = user.sid_string().unwrap();
        assert!(sid.starts_with("S-1-"), "{sid}");
        let base = Path::new(r"C:\Users\x\AppData\Local\dicta");
        let normal = text(&pipe_name(&base.join("run"), &user).unwrap());
        assert_eq!(normal, format!(r"\\.\pipe\io.dicta.Dicta.{sid}"));
        let demo = text(&pipe_name(&base.join("run-demo"), &user).unwrap());
        assert_eq!(demo, format!(r"\\.\pipe\io.dicta.Dicta.Demo.{sid}"));
        let other = text(&pipe_name(Path::new(r"C:\Temp\x\dicta"), &user).unwrap());
        assert!(other.starts_with(&format!(r"\\.\pipe\io.dicta.Dicta.{sid}.")));
        assert_ne!(other, normal);
        let same = text(&pipe_name(Path::new(r"c:\TEMP\X\Dicta"), &user).unwrap());
        assert_eq!(same, other);
    }
    #[test]
    fn missing_pipe_is_not_found_and_peers_are_the_same_user() {
        let temp = Temp::new();
        let runtime = temp.0.join("dicta");
        let user = UserSid::current().unwrap();
        let name = pipe_name(&runtime, &user).unwrap();
        let missing = connect(&name, Duration::from_millis(200)).err().unwrap();
        assert_eq!(missing.kind(), io::ErrorKind::NotFound);
        let (tx, _rx) = mpsc::channel();
        let _server = super::super::start(&runtime, tx).unwrap();
        let client = connect(&name, CLIENT_TIMEOUT).unwrap();
        assert!(peer_is_user(&client, &user, Peer::Server));
        assert!(UserSid::current().unwrap().same_as(&user));
    }
    #[test]
    fn overlong_and_incomplete_clients_get_err_without_dispatch() {
        let temp = Temp::new();
        let runtime = temp.0.join("dicta");
        let (tx, rx) = mpsc::channel();
        let server = super::super::start(&runtime, tx).unwrap();
        let name = pipe_name(&runtime, &UserSid::current().unwrap()).unwrap();
        let deadline = || Instant::now() + CLIENT_TIMEOUT;
        let long = connect(&name, CLIENT_TIMEOUT).unwrap();
        write_all(&long, &vec![b'a'; MAX_LINE + 1], deadline()).unwrap();
        assert!(
            read_line(&long, CLIENT_TIMEOUT)
                .unwrap()
                .starts_with("ERR ")
        );
        drop(long);
        let partial = connect(&name, CLIENT_TIMEOUT).unwrap();
        write_all(&partial, b"toggle", deadline()).unwrap();
        assert!(
            read_line(&partial, CLIENT_TIMEOUT)
                .unwrap()
                .starts_with("ERR ")
        );
        assert!(rx.try_recv().is_err());
        let stopped = Instant::now();
        drop(server);
        assert!(stopped.elapsed() < Duration::from_secs(3));
        drop(partial);
    }
}
