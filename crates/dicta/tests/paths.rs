//! Pins the Linux folders so no existing user loses configuration, history or
//! accounts. The check re-runs this test binary with XDG overrides instead of
//! changing the environment of a multi-threaded test process.
#![cfg(all(unix, not(target_os = "macos")))]
use dicta_core::AppPaths;
use std::process::Command;

const PROBE: &str = "DICTA_TEST_PATHS_PROBE";

#[test]
fn linux_paths_stay_in_xdg_dicta_folders() {
    if std::env::var_os(PROBE).is_some() {
        let paths = AppPaths::discover().unwrap();
        for (label, path) in [
            ("config", paths.config.clone()),
            ("data", paths.data.clone()),
            ("runtime", paths.runtime.clone()),
            ("demo", paths.demo_runtime()),
        ] {
            println!("PATH {label}={}", path.display());
        }
        return;
    }
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "linux_paths_stay_in_xdg_dicta_folders",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(PROBE, "1")
        .env("HOME", "/home/probe")
        .env("XDG_CONFIG_HOME", "/xdg/config")
        .env("XDG_DATA_HOME", "/xdg/data")
        .env("XDG_RUNTIME_DIR", "/run/user/4242")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let found: Vec<&str> = stdout
        .lines()
        // libtest prints "test … " before the first line without a newline.
        .filter_map(|line| line.split_once("PATH ").map(|(_, path)| path))
        .collect();
    assert_eq!(
        found,
        [
            "config=/xdg/config/dicta",
            "data=/xdg/data/dicta",
            "runtime=/run/user/4242/dicta",
            "demo=/run/user/4242/dicta-demo",
        ]
    );
    // Without XDG overrides the defaults are ~/.config and ~/.local/share.
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "linux_paths_stay_in_xdg_dicta_folders",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(PROBE, "1")
        .env("HOME", "/home/probe")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME")
        .env("XDG_RUNTIME_DIR", "/run/user/4242")
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("PATH config=/home/probe/.config/dicta\n"),
        "{stdout}"
    );
    assert!(
        stdout.contains("PATH data=/home/probe/.local/share/dicta\n"),
        "{stdout}"
    );
}
