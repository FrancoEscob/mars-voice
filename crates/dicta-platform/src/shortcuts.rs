//! Global shortcuts and the shared shortcut clock.
//!
//! On Linux, [`run`] drives the opt-in XDG GlobalShortcuts portal
//! (`shortcuts/portal.rs`), which may display compositor authorization UI. If
//! unsupported, offer [`HYPRLAND_BIND_EXAMPLE`] rather than writing user config.
//! Other systems have no global-shortcut backend yet: [`run`] returns an
//! actionable Spanish error. [`event_stamp`] works everywhere.
use anyhow::Result;
use dicta_core::{AppConfig, Command};
use std::sync::mpsc::Sender;
use tokio::sync::watch;

#[cfg(target_os = "linux")]
mod portal;

pub const HYPRLAND_BIND_EXAMPLE: &str = r#"-- Usá packaging/hyprland.lua.example completo.
-- Define --shortcut-press al pulsar Ctrl+Space y --shortcut-release al soltar
-- Space o Control. Incluye guard contra repetición y liberación al recargar.
-- No uses --toggle si querés mantener para hablar; no combines portal y binds.
"#;

/// Run until stop changes to true / its sender is dropped. Portal support and
/// user permission are required. Errors should be shown with the bind fallback.
/// Do not bind portal and manual compositor shortcuts to the same combination.
pub async fn run(
    config: &AppConfig,
    commands: Sender<Command>,
    stop: watch::Receiver<bool>,
) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        portal::run(config, commands, stop).await
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (config, commands);
        unavailable(&stop)
    }
}

/// No backend on this system. A stop requested before starting is still a
/// clean exit, like the portal's.
#[cfg(any(not(target_os = "linux"), test))]
fn unavailable(stop: &watch::Receiver<bool>) -> Result<()> {
    if *stop.borrow() {
        return Ok(());
    }
    anyhow::bail!("Los atajos globales todavía no están disponibles en este sistema")
}

/// Shared monotonic clock for independently launched press/release commands.
/// Nanoseconds from an arbitrary origin that is the same for every process on
/// this boot, unaffected by wall-clock changes: `CLOCK_MONOTONIC` on Linux and
/// macOS, `QueryPerformanceCounter` on Windows.
#[cfg(unix)]
pub fn event_stamp() -> u64 {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: a valid, writable timespec is supplied for the monotonic clock.
    let result = unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) };
    assert_eq!(result, 0, "CLOCK_MONOTONIC unavailable");
    (now.tv_sec as u64).saturating_mul(1_000_000_000) + now.tv_nsec as u64
}

/// Same contract as the Unix version: the performance counter is system-wide.
#[cfg(windows)]
pub fn event_stamp() -> u64 {
    use windows_sys::Win32::System::Performance::{
        QueryPerformanceCounter, QueryPerformanceFrequency,
    };
    let (mut ticks, mut frequency) = (0i64, 0i64);
    // SAFETY: valid, writable out-pointers; both calls cannot fail since Windows XP.
    let ok = unsafe {
        QueryPerformanceCounter(&mut ticks) != 0 && QueryPerformanceFrequency(&mut frequency) != 0
    };
    assert!(
        ok && ticks >= 0 && frequency > 0,
        "QueryPerformanceCounter unavailable"
    );
    ticks_to_nanos(ticks as u64, frequency as u64)
}

/// Exact for any uptime: the product cannot overflow u128.
#[cfg(any(windows, test))]
fn ticks_to_nanos(ticks: u64, frequency: u64) -> u64 {
    let nanos = u128::from(ticks) * 1_000_000_000 / u128::from(frequency.max(1));
    u64::try_from(nanos).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn performance_counter_ticks_convert_to_nanoseconds_without_overflow() {
        assert_eq!(ticks_to_nanos(15, 10_000_000), 1_500);
        assert_eq!(ticks_to_nanos(3, 3), 1_000_000_000);
        // A year of uptime at 3 GHz still fits in u64 nanoseconds.
        let year = 365 * 24 * 3600;
        assert_eq!(
            ticks_to_nanos(year * 3_000_000_000, 3_000_000_000),
            year * 1_000_000_000
        );
    }

    #[test]
    fn event_stamp_never_goes_backwards() {
        let first = event_stamp();
        let second = event_stamp();
        assert!(first > 0 && second >= first);
    }

    #[test]
    fn systems_without_backend_report_shortcuts_unavailable_unless_stopped() {
        let (stop_tx, stop) = watch::channel(false);
        let error = unavailable(&stop).unwrap_err().to_string();
        assert!(error.contains("todavía no están disponibles"), "{error}");
        stop_tx.send(true).unwrap();
        unavailable(&stop).unwrap();
    }
}
