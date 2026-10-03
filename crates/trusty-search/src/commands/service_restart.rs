//! `trusty-search service restart` — a restart that proves the old daemon is
//! gone (#8686).
//!
//! Why: the documented restart was `launchctl bootout` then `bootstrap`.
//! `bootout` stops only the process launchd owns, so a daemon already detached
//! from launchd (PPID 1) kept serving on :7878 at the OLD version, and the
//! restart read as successful.
//! What: [`restart_with`] orders the restart over injected effects: record every
//! running daemon PID, boot the unit out, terminate any recorded PID still
//! alive, refuse to bootstrap while one survives, bootstrap, then confirm a new
//! daemon answers `/health` with this binary's version and no recorded PID is
//! still alive. [`service_restart`] binds the real `launchctl`, signals, and
//! `search.health` probe over the daemon socket.
//! Test: `service_restart_tests`.

#[cfg(any(target_os = "macos", test))]
use anyhow::{bail, Result};

/// What the restarted daemon reported (#8686).
#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RestartReport {
    /// Daemon PIDs found before the restart; every one is now gone.
    pub(crate) replaced: Vec<u32>,
    /// The version `/health` reported after the restart.
    pub(crate) version: String,
}

/// Order one restart over injected effects (#8686).
///
/// Why: see the module docs; injecting the effects makes the ORDER testable
/// without a real launchd unit or daemon.
/// What: `bootout` runs first and its `Err` aborts. `terminate` receives every
/// recorded PID that `alive` still reports after the bootout and returns those
/// that outlived it; any survivor aborts before `bootstrap`, because a bootstrap
/// onto a held port crash-loops while the old daemon keeps serving. After
/// `bootstrap`, `health_version` must return `expected_version`, and no recorded
/// PID may be alive.
/// Test: `a_detached_daemon_is_terminated_before_the_bootstrap`,
/// `a_survivor_aborts_before_the_bootstrap`,
/// `an_old_version_on_health_fails_the_restart`.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn restart_with(
    before: Vec<u32>,
    expected_version: &str,
    bootout: impl FnOnce() -> Result<()>,
    alive: impl Fn(u32) -> bool,
    terminate: impl FnOnce(&[u32]) -> Vec<u32>,
    bootstrap: impl FnOnce() -> Result<()>,
    health_version: impl FnOnce() -> Result<String>,
) -> Result<RestartReport> {
    bootout()?;
    let lingering: Vec<u32> = before.iter().copied().filter(|p| alive(*p)).collect();
    if !lingering.is_empty() {
        let survivors = terminate(&lingering);
        if !survivors.is_empty() {
            bail!(
                "old trusty-search daemon PID(s) {survivors:?} survived bootout, SIGTERM and \
                 SIGKILL — not bootstrapping onto a port they may still hold (#8686)"
            );
        }
    }
    bootstrap()?;
    let version = health_version()?;
    if version != expected_version {
        bail!(
            "/health reports version {version}, not {expected_version} — an old daemon is \
             still answering (#8686)"
        );
    }
    let still: Vec<u32> = before.iter().copied().filter(|p| alive(*p)).collect();
    if !still.is_empty() {
        bail!("old trusty-search daemon PID(s) {still:?} are alive after the restart (#8686)");
    }
    Ok(RestartReport {
        replaced: before,
        version,
    })
}

/// The `version` field of a health report (#8686).
///
/// Test: `health_version_is_read_from_the_report`.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn version_from_health(report: &serde_json::Value) -> Option<String> {
    report.get("version")?.as_str().map(str::to_string)
}

/// One `search.health` call over the daemon socket — the same report
/// `GET /health` serves; `None` when nothing answers (#8686).
#[cfg(target_os = "macos")]
fn probe_health_version() -> Option<String> {
    let client = crate::service::daemon_client::DaemonClient::resolve().ok()?;
    let report =
        tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(client.health()))
            .ok()?;
    version_from_health(&report)
}

/// SIGTERM `pids`, wait the termination grace, SIGKILL stragglers; returns the
/// PIDs still alive afterwards (#8686).
#[cfg(target_os = "macos")]
fn terminate_pids(pids: &[u32]) -> Vec<u32> {
    use crate::service::daemon::pid_alive;
    use std::time::{Duration, Instant};
    println!("· terminating detached daemon PID(s) {pids:?} that bootout left running (#8686)");
    for pid in pids {
        let _ = super::stop::send_signal(*pid, "TERM");
    }
    let deadline = Instant::now() + trusty_common::shutdown::termination_grace();
    while Instant::now() < deadline && pids.iter().any(|p| pid_alive(*p)) {
        std::thread::sleep(Duration::from_millis(200));
    }
    for pid in pids.iter().filter(|p| pid_alive(**p)) {
        let _ = super::stop::send_signal(*pid, "KILL");
    }
    std::thread::sleep(Duration::from_millis(500));
    pids.iter().copied().filter(|p| pid_alive(*p)).collect()
}

/// Restart the launchd-supervised daemon and verify the result (#8686).
///
/// Test: the ordering is tested through [`restart_with`]; this binding runs
/// real `launchctl` calls.
#[cfg(target_os = "macos")]
pub(crate) fn service_restart(cfg: &trusty_common::launchd::LaunchdConfig) -> Result<()> {
    use std::time::{Duration, Instant};
    let before = super::stop::find_daemon_pids();
    let budget = trusty_common::shutdown::termination_grace().as_secs();
    let report = restart_with(
        before,
        env!("CARGO_PKG_VERSION"),
        || {
            if cfg.is_loaded() {
                cfg.bootout()?;
                trusty_common::launchd_restart::await_unload(
                    budget,
                    || cfg.is_loaded(),
                    || std::thread::sleep(Duration::from_secs(1)),
                );
            }
            Ok(())
        },
        crate::service::daemon::pid_alive,
        terminate_pids,
        || cfg.bootstrap(),
        || {
            let deadline = Instant::now() + Duration::from_secs(budget.max(60));
            loop {
                match probe_health_version() {
                    Some(v) if v == env!("CARGO_PKG_VERSION") => return Ok(v),
                    Some(v) if Instant::now() >= deadline => return Ok(v),
                    None if Instant::now() >= deadline => {
                        bail!("no daemon answered search.health after the restart (#8686)")
                    }
                    _ => std::thread::sleep(Duration::from_millis(500)),
                }
            }
        },
    )?;
    println!(
        "✓ restarted: /health reports {}; replaced daemon PID(s) {:?} are gone",
        report.version, report.replaced
    );
    Ok(())
}
