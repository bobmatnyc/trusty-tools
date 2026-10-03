//! `trusty-search service restart` — a restart that proves the old daemon is
//! gone (#8686).
//!
//! Why: the documented restart was `launchctl bootout` then `bootstrap`.
//! `bootout` stops only the process launchd owns, so a daemon already detached
//! from launchd (PPID 1) kept serving on :7878 at the OLD version, and the
//! restart read as successful.
//! What: [`restart_with`] orders the restart over injected effects: record the
//! daemons serving this unit's data dir, boot the unit out, terminate any
//! recorded PID still serving it, refuse to bootstrap while one survives,
//! bootstrap, then confirm a new daemon answers `/health` with this binary's
//! version and no recorded PID still serves it. Daemons on other data dirs are
//! never targeted (#4395): the candidate set comes from
//! `start::reap_orphans::plan`, and [`terminate_scoped`] re-scans before each
//! signal so a recycled PID is never killed. [`service_restart`] binds the real
//! `launchctl`, signals, and a `search.health` probe on the unit's own socket.
//! Test: `service_restart_tests`.

#[cfg(any(target_os = "macos", test))]
use std::path::{Path, PathBuf};
#[cfg(any(target_os = "macos", test))]
use std::time::Duration;

#[cfg(any(target_os = "macos", test))]
use anyhow::{bail, Result};

#[cfg(any(target_os = "macos", test))]
use super::start::reap_orphans::{plan, Candidate, ConfirmedOrphan};

/// What the restarted daemon reported (#8686).
#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RestartReport {
    /// Daemon PIDs found before the restart; none serves the data dir now.
    pub(crate) replaced: Vec<u32>,
    /// The version `/health` reported after the restart.
    pub(crate) version: String,
}

/// Order one restart over injected effects (#8686).
///
/// Why: see the module docs; injecting the effects makes the ORDER testable
/// without a real launchd unit or daemon.
/// What: `bootout` runs first and its `Err` aborts. `terminate` receives every
/// recorded PID that `still_ours` reports after the bootout (still a daemon on
/// this unit's data dir) and returns those that outlived it; any survivor
/// aborts before `bootstrap`, because a bootstrap onto a held port crash-loops
/// while the old daemon keeps serving. After `bootstrap`, `health_version` must
/// return `expected_version`, and no recorded PID may still be ours.
/// Test: `a_detached_daemon_is_terminated_before_the_bootstrap`,
/// `a_survivor_aborts_before_the_bootstrap`,
/// `an_old_version_on_health_fails_the_restart`.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn restart_with(
    before: Vec<u32>,
    expected_version: &str,
    bootout: impl FnOnce() -> Result<()>,
    still_ours: impl Fn(u32) -> bool,
    terminate: impl FnOnce(&[u32]) -> Vec<u32>,
    bootstrap: impl FnOnce() -> Result<()>,
    health_version: impl FnOnce() -> Result<String>,
) -> Result<RestartReport> {
    bootout()?;
    let lingering: Vec<u32> = before.iter().copied().filter(|p| still_ours(*p)).collect();
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
    let still: Vec<u32> = before.iter().copied().filter(|p| still_ours(*p)).collect();
    if !still.is_empty() {
        bail!("old trusty-search daemon PID(s) {still:?} are alive after the restart (#8686)");
    }
    Ok(RestartReport {
        replaced: before,
        version,
    })
}

/// The data dir an installed unit declares: `--data-dir` in its
/// `ProgramArguments`, else a non-empty `TRUSTY_DATA_DIR` in its environment;
/// `None` when it declares neither and so serves the platform default (#8686).
///
/// Why: the restart must act on the unit's daemon, not on whatever data dir the
/// CLI's own environment names.
/// Test: `the_unit_data_dir_comes_from_the_plist`.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn unit_data_dir_override(args: &[String], env: &[(String, String)]) -> Option<PathBuf> {
    let mut words = args.iter();
    while let Some(word) = words.next() {
        if let Some(value) = word.strip_prefix("--data-dir=") {
            return Some(PathBuf::from(value));
        }
        if word == "--data-dir" {
            return words.next().map(PathBuf::from);
        }
    }
    env.iter()
        .find(|(k, v)| k == "TRUSTY_DATA_DIR" && !v.is_empty())
        .map(|(_, v)| PathBuf::from(v))
}

/// The socket the unit's daemon binds, from the unit's data dir (#8686).
///
/// Why: the probe resolved the socket from the CLI's own `TRUSTY_DATA_DIR`, so a
/// shell exporting a different one reported a correct restart as failed.
/// What: `service::socket::resolve_socket_path` over the unit's override.
/// Test: `the_health_probe_uses_the_units_socket`.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn unit_socket_path(unit_override: Option<&Path>) -> Result<PathBuf> {
    crate::service::socket::resolve_socket_path(unit_override.map(Path::as_os_str))
}

/// PIDs of the trusty-search daemons serving `unit_data_dir` (#8686, #4395).
///
/// What: `reap_orphans::plan` over `candidates`; only its confirmed orphans —
/// daemons positively identified on this data dir — are returned. A daemon on
/// another data dir, or one whose argv or environment is unreadable, is
/// spared.
/// Test: `a_daemon_on_another_data_dir_is_not_targeted`.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn scoped_daemon_pids(
    candidates: &[Candidate],
    unit_data_dir: &Path,
    platform_default: &Path,
) -> Vec<u32> {
    plan(candidates, unit_data_dir, platform_default)
        .orphans
        .iter()
        .map(ConfirmedOrphan::pid)
        .collect()
}

/// SIGTERM, wait, SIGKILL — each signal only to a PID a fresh scan still finds
/// serving this unit's data dir; returns the PIDs still serving it (#8686).
///
/// Why: a PID recorded before the bootout can exit and be reused by an
/// unrelated process during the termination grace. Killing it by number
/// would hit that process.
/// What: `scoped_now` re-scans the process table. It runs before the SIGTERM,
/// immediately before the SIGKILL, and for the final answer. `wait_until`
/// waits up to its budget for the predicate to hold.
/// Test: `a_pid_no_longer_a_scoped_daemon_is_not_sigkilled`,
/// `a_scoped_daemon_that_ignores_sigterm_is_sigkilled`.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn terminate_scoped(
    pids: &[u32],
    scoped_now: impl Fn() -> Vec<u32>,
    mut signal: impl FnMut(u32, &'static str),
    mut wait_until: impl FnMut(Duration, &dyn Fn() -> bool),
) -> Vec<u32> {
    let still_ours = || -> Vec<u32> {
        let now = scoped_now();
        pids.iter().copied().filter(|p| now.contains(p)).collect()
    };
    for pid in still_ours() {
        signal(pid, "TERM");
    }
    wait_until(trusty_common::shutdown::termination_grace(), &|| {
        still_ours().is_empty()
    });
    // #8686: re-scanned immediately before SIGKILL, so a recycled PID is spared.
    for pid in still_ours() {
        signal(pid, "KILL");
    }
    wait_until(Duration::from_secs(2), &|| still_ours().is_empty());
    still_ours()
}

/// The `version` field of a health report (#8686).
///
/// Test: `health_version_is_read_from_the_report`.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn version_from_health(report: &serde_json::Value) -> Option<String> {
    report.get("version")?.as_str().map(str::to_string)
}

/// One `search.health` call on `socket` — the same report `GET /health`
/// serves; `None` when nothing answers (#8686).
#[cfg(target_os = "macos")]
fn probe_health_version(socket: &Path) -> Option<String> {
    let client = crate::service::daemon_client::DaemonClient::at(socket);
    let report =
        tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(client.health()))
            .ok()?;
    version_from_health(&report)
}

/// Restart the launchd-supervised daemon and verify the result (#8686).
///
/// What: resolves the unit's data dir and socket from `unit_args` /
/// `unit_env` (the installed plist), then runs [`restart_with`] with
/// [`scoped_daemon_pids`] over a live `reap_orphans::observe_candidates` scan.
/// Test: the ordering is tested through [`restart_with`], the scoping through
/// [`scoped_daemon_pids`] and [`terminate_scoped`]; this binding runs real
/// `launchctl` calls.
#[cfg(target_os = "macos")]
pub(crate) fn service_restart(
    cfg: &trusty_common::launchd::LaunchdConfig,
    unit_args: &[String],
    unit_env: &[(String, String)],
) -> Result<()> {
    use super::start::reap_orphans::observe_candidates;
    use crate::service::daemon::resolve_daemon_dir;
    use std::time::Instant;

    let unit_override = unit_data_dir_override(unit_args, unit_env);
    let Some(unit_dir) = resolve_daemon_dir(unit_override.as_deref().map(Path::as_os_str)) else {
        bail!("the unit's data dir is unresolvable — not restarting (#4395)");
    };
    let Some(platform_default) = resolve_daemon_dir(None) else {
        bail!("the platform default data dir is unresolvable — not restarting (#4395)");
    };
    let socket = unit_socket_path(unit_override.as_deref())?;
    let scoped_now = || scoped_daemon_pids(&observe_candidates(), &unit_dir, &platform_default);
    let budget = trusty_common::shutdown::termination_grace().as_secs();
    let report = restart_with(
        scoped_now(),
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
        |pid| scoped_now().contains(&pid),
        |pids| {
            println!(
                "· terminating detached daemon PID(s) {pids:?} on {} that bootout left \
                 running (#8686)",
                unit_dir.display()
            );
            terminate_scoped(
                pids,
                scoped_now,
                |pid, sig| {
                    let _ = super::stop::send_signal(pid, sig);
                },
                |within, done| {
                    let deadline = Instant::now() + within;
                    while Instant::now() < deadline && !done() {
                        std::thread::sleep(Duration::from_millis(200));
                    }
                },
            )
        },
        || cfg.bootstrap(),
        || {
            let deadline = Instant::now() + Duration::from_secs(budget.max(60));
            loop {
                match probe_health_version(&socket) {
                    Some(v) if v == env!("CARGO_PKG_VERSION") => return Ok(v),
                    Some(v) if Instant::now() >= deadline => return Ok(v),
                    None if Instant::now() >= deadline => {
                        bail!(
                            "no daemon answered search.health on {} after the restart (#8686)",
                            socket.display()
                        )
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
