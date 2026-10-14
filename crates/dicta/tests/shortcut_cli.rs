//! The shortcut launcher is exercised only in demo mode, with private XDG paths.
//! No microphone, provider, keyring, clipboard or real compositor is accessed.
//! Unix only: it redirects `XDG_RUNTIME_DIR`, which Windows and macOS ignore.
#![cfg(unix)]
use dicta::ipc;
use dicta_core::Command;
use std::{
    fs::{self, OpenOptions},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::PathBuf,
    process::{Child, Command as Process, Output, Stdio},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

const BINARY: &str = env!("CARGO_BIN_EXE_dicta");
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "dicta-shortcut-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        Self(root)
    }
    fn runtime(&self) -> PathBuf {
        self.0.join("dicta-demo")
    }
    fn process(&self, args: &[&str]) -> ChildGuard {
        let child = Process::new(BINARY)
            .args(args)
            .env("XDG_RUNTIME_DIR", &self.0)
            .env("XDG_CONFIG_HOME", self.0.join("config"))
            .env("XDG_DATA_HOME", self.0.join("data"))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        ChildGuard(Some(child))
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
struct ChildGuard(Option<Child>);
impl ChildGuard {
    fn wait(mut self, timeout: Duration) -> Output {
        let deadline = Instant::now() + timeout;
        while self.0.as_mut().unwrap().try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "El launcher no terminó a tiempo");
            std::thread::sleep(Duration::from_millis(10));
        }
        self.0.take().unwrap().wait_with_output().unwrap()
    }
}
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
fn wait_socket(path: &std::path::Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !path.exists() {
        assert!(Instant::now() < deadline, "No apareció el socket demo");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn cold_toggle_launches_and_warm_toggle_reuses_instance_then_quit_cleans_up() {
    let temp = Temp::new();
    let mut resident = temp.process(&[
        "--demo",
        "--headless",
        "--no-tray",
        "--toggle",
        "--quit-after",
        "15",
    ]);
    let pid = resident.0.as_ref().unwrap().id();
    let runtime = temp.runtime();
    let socket = runtime.join("control.sock");
    wait_socket(&socket);
    assert_eq!(
        ipc::send(&runtime, "status").unwrap(),
        "OK Dicta está ejecutándose"
    );
    let socket_inode = fs::metadata(&socket).unwrap().ino();
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(runtime.join("instance.lock"))
        .unwrap();
    assert!(lock.try_lock().is_err());
    // Deliberately omit --headless on the second launch. Forwarding must work
    // before the binary's no-GUI validation when an instance already exists.
    let warm = temp
        .process(&["--demo", "--toggle"])
        .wait(Duration::from_secs(3));
    assert!(
        warm.status.success(),
        "{}",
        String::from_utf8_lossy(&warm.stderr)
    );
    assert_eq!(resident.0.as_ref().unwrap().id(), pid);
    assert!(resident.0.as_mut().unwrap().try_wait().unwrap().is_none());
    assert_eq!(fs::metadata(&socket).unwrap().ino(), socket_inode);
    assert!(lock.try_lock().is_err());
    ipc::send(&runtime, "quit").unwrap();
    let output = resident.wait(Duration::from_secs(5));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    assert!(!socket.exists());
    lock.try_lock().unwrap();
    assert!(!temp.0.join("config").exists());
    assert!(!temp.0.join("data").exists());
}

#[test]
fn warm_shortcut_delivers_one_toggle_and_normal_launcher_opens_settings() {
    let temp = Temp::new();
    let (tx, rx) = mpsc::channel();
    let _server = ipc::start(&temp.runtime(), tx).unwrap();
    let result = temp
        .process(&["--demo", "--toggle"])
        .wait(Duration::from_secs(3));
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(matches!(
        rx.recv_timeout(Duration::from_secs(1)).unwrap(),
        Command::Toggle
    ));
    assert!(rx.try_recv().is_err());
    let result = temp
        .process(&["--demo", "--headless"])
        .wait(Duration::from_secs(3));
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(matches!(
        rx.recv_timeout(Duration::from_secs(1)).unwrap(),
        Command::ShowSettings
    ));
    assert!(rx.try_recv().is_err());
}

#[test]
fn toggle_and_ctl_are_rejected_without_dispatch_or_launch() {
    let temp = Temp::new();
    let (tx, rx) = mpsc::channel();
    let _server = ipc::start(&temp.runtime(), tx).unwrap();
    for args in [
        vec!["--demo", "--toggle", "ctl", "toggle"],
        vec!["--demo", "ctl", "toggle", "--toggle"],
    ] {
        let result = temp.process(&args).wait(Duration::from_secs(3));
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains("no ambos"));
        assert!(rx.try_recv().is_err());
    }
}

#[test]
fn release_before_press_process_start_is_fenced_without_starting_app() {
    let temp = Temp::new();
    let released = temp
        .process(&["--demo", "--shortcut-release", "--shortcut-token", "100"])
        .wait(Duration::from_secs(3));
    assert!(
        released.status.success(),
        "{}",
        String::from_utf8_lossy(&released.stderr)
    );
    assert!(!temp.runtime().join("control.sock").exists());
    // This process gets CPU later than release. The gesture token, not launch
    // time, must prove that this press is obsolete and prevent cold startup.
    let late = temp
        .process(&["--demo", "--shortcut-press", "--shortcut-token", "100"])
        .wait(Duration::from_secs(3));
    assert!(
        late.status.success(),
        "{}",
        String::from_utf8_lossy(&late.stderr)
    );
    assert!(!temp.runtime().join("control.sock").exists());
}

#[test]
fn shortcut_events_are_ordered_and_tokenized_through_warm_ipc() {
    let temp = Temp::new();
    let (tx, rx) = mpsc::channel();
    let _server = ipc::start(&temp.runtime(), tx).unwrap();
    for action in ["--shortcut-press", "--shortcut-release"] {
        let result = temp
            .process(&["--demo", action, "--shortcut-token", "200"])
            .wait(Duration::from_secs(3));
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    assert!(matches!(
        rx.recv_timeout(Duration::from_secs(1)).unwrap(),
        Command::ShortcutPressed {
            token: Some(200),
            ..
        }
    ));
    assert!(matches!(
        rx.recv_timeout(Duration::from_secs(1)).unwrap(),
        Command::ShortcutReleased {
            token: Some(200),
            ..
        }
    ));
    let late = temp
        .process(&["--demo", "--shortcut-press", "--shortcut-token", "200"])
        .wait(Duration::from_secs(3));
    assert!(late.status.success());
    assert!(rx.try_recv().is_err());
}

#[test]
fn release_waits_for_cold_launch_delivery_and_never_starts_a_second_resident() {
    let temp = Temp::new();
    let resident = temp.process(&[
        "--demo",
        "--headless",
        "--no-tray",
        "--shortcut-press",
        "--shortcut-token",
        "300",
        "--quit-after",
        "10",
    ]);
    // Launch release immediately, without waiting for the socket. Either it
    // fences the delayed press, or it follows initial delivery under the lock.
    let released = temp
        .process(&["--demo", "--shortcut-release", "--shortcut-token", "300"])
        .wait(Duration::from_secs(4));
    assert!(
        released.status.success(),
        "{}",
        String::from_utf8_lossy(&released.stderr)
    );
    if temp.runtime().join("control.sock").exists() {
        ipc::send(&temp.runtime(), "quit").unwrap();
    }
    let result = resident.wait(Duration::from_secs(5));
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(!temp.runtime().join("control.sock").exists());
}

#[test]
fn shortcut_seed_is_monotonic_and_does_not_create_runtime_or_load_config() {
    let temp = Temp::new();
    let one = temp
        .process(&["--shortcut-seed"])
        .wait(Duration::from_secs(2));
    let two = temp
        .process(&["--shortcut-seed"])
        .wait(Duration::from_secs(2));
    assert!(one.status.success() && two.status.success());
    let one = String::from_utf8(one.stdout)
        .unwrap()
        .trim()
        .parse::<u64>()
        .unwrap();
    let two = String::from_utf8(two.stdout)
        .unwrap()
        .trim()
        .parse::<u64>()
        .unwrap();
    assert!(one > 0 && two > one);
    assert_eq!(fs::read_dir(&temp.0).unwrap().count(), 0);
}

#[test]
fn warm_read_selection_and_tts_controls_reach_the_resident_without_text() {
    let temp = Temp::new();
    let (tx, rx) = mpsc::channel();
    let _server = ipc::start(&temp.runtime(), tx).unwrap();
    let read = temp
        .process(&["--demo", "--read-selection"])
        .wait(Duration::from_secs(3));
    assert!(
        read.status.success(),
        "{}",
        String::from_utf8_lossy(&read.stderr)
    );
    assert!(matches!(
        rx.recv_timeout(Duration::from_secs(2)).unwrap(),
        Command::ReadSelection
    ));
    for (verb, check) in [
        (
            "read-clipboard",
            (|c: &Command| matches!(c, Command::ReadClipboard)) as fn(&Command) -> bool,
        ),
        ("tts-toggle", |c: &Command| matches!(c, Command::TtsToggle)),
        ("tts-stop", |c: &Command| matches!(c, Command::TtsStop)),
    ] {
        let output = temp
            .process(&["--demo", "ctl", verb])
            .wait(Duration::from_secs(3));
        assert!(output.status.success(), "{verb}");
        assert!(
            check(&rx.recv_timeout(Duration::from_secs(2)).unwrap()),
            "{verb}"
        );
    }
    let rejected = temp
        .process(&["--demo", "--read-selection", "ctl", "read"])
        .wait(Duration::from_secs(3));
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("no ambos"));
    assert!(rx.try_recv().is_err());
}

#[test]
fn warm_write_selection_and_write_clipboard_reach_the_resident_without_text() {
    let temp = Temp::new();
    let (tx, rx) = mpsc::channel();
    let _server = ipc::start(&temp.runtime(), tx).unwrap();
    let write = temp
        .process(&["--demo", "--write-selection"])
        .wait(Duration::from_secs(3));
    assert!(
        write.status.success(),
        "{}",
        String::from_utf8_lossy(&write.stderr)
    );
    assert!(matches!(
        rx.recv_timeout(Duration::from_secs(2)).unwrap(),
        Command::WriterSelection
    ));
    for (verb, check) in [
        (
            "write",
            (|c: &Command| matches!(c, Command::WriterSelection)) as fn(&Command) -> bool,
        ),
        ("write-clipboard", |c: &Command| {
            matches!(c, Command::WriterClipboard)
        }),
    ] {
        let output = temp
            .process(&["--demo", "ctl", verb])
            .wait(Duration::from_secs(3));
        assert!(output.status.success(), "{verb}");
        assert!(
            check(&rx.recv_timeout(Duration::from_secs(2)).unwrap()),
            "{verb}"
        );
    }
    for args in [
        &["--demo", "--write-selection", "ctl", "write"][..],
        &["--demo", "--write-selection", "--read-selection"][..],
        &["--demo", "--write-selection", "--toggle"][..],
    ] {
        let rejected = temp.process(args).wait(Duration::from_secs(3));
        assert!(!rejected.status.success(), "{args:?}");
        assert!(
            String::from_utf8_lossy(&rejected.stderr).contains("no ambos"),
            "{args:?}"
        );
    }
    assert!(rx.try_recv().is_err());
}
