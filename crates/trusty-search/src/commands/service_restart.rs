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
/// `Ok(None)` when it declares neither and so serves the platform default
/// (#8686).
///
/// Why: the restart must act on the unit's daemon, not on whatever data dir the
/// CLI's own environment names. Falling back to the platform default when the
/// unit's own declaration is unreadable would signal a daemon that is not the
/// unit's.
/// What: `Err` when `args` is empty — a launchd unit always has
/// `ProgramArguments`, so none means a binary or malformed plist — and when
/// `--data-dir` has no value, matching `reap_orphans::declared_data_dir`.
/// Test: `the_unit_data_dir_comes_from_the_plist`,
/// `a_unit_without_program_arguments_signals_nothing`,
/// `a_dangling_data_dir_signals_nothing`.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn unit_data_dir_override(
    args: &[String],
    env: &[(String, String)],
) -> Result<Option<PathBuf>> {
    // #8686: an undeterminable data dir fails closed, never to the default.
    if args.is_empty() {
        bail!(
            "the installed unit declares no ProgramArguments (a binary or malformed plist), so \
             its data dir cannot be determined — not restarting (#8686)"
        );
    }
    let mut words = args.iter();
    while let Some(word) = words.next() {
        let value = if let Some(value) = word.strip_prefix("--data-dir=") {
            Some(value)
        } else if word == "--data-dir" {
            words.next().map(String::as_str)
        } else {
            continue;
        };
        return match value.filter(|v| !v.is_empty()) {
            Some(v) => Ok(Some(PathBuf::from(v))),
            None => bail!(
                "the installed unit passes `--data-dir` with no value, so its data dir cannot \
                 be determined — not restarting (#8686)"
            ),
        };
    }
    Ok(env
        .iter()
        .find(|(k, v)| k == "TRUSTY_DATA_DIR" && !v.is_empty())
        .map(|(_, v)| PathBuf::from(v)))
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

/// The process and launchd effects one restart performs (#8686).
///
/// Why: injected so [`restart_unit`]'s fail-closed arms are testable without a
/// launchd unit or a live daemon — a test records every signal.
/// What: [`LaunchdEffects`] binds the real process table, signals, `launchctl`
/// and `search.health`.
/// Test: `a_unit_without_program_arguments_signals_nothing`.
#[cfg(any(target_os = "macos", test))]
pub(crate) trait RestartEffects {
    /// Every running `trusty-search start` process.
    fn candidates(&self) -> Vec<Candidate>;
    /// Send `sig` (`"TERM"` or `"KILL"`) to `pid`.
    fn signal(&self, pid: u32, sig: &'static str);
    /// Wait up to `within` for `done` to hold.
    fn wait_until(&self, within: Duration, done: &dyn Fn() -> bool);
    /// The socket the unit's daemon binds, from its data-dir override.
    fn socket_for(&self, unit_override: Option<&Path>) -> Result<PathBuf>;
    /// Boot the unit out and wait for launchd to unload it.
    fn bootout(&self) -> Result<()>;
    /// Bootstrap the unit.
    fn bootstrap(&self) -> Result<()>;
    /// The version the daemon on `socket` reports once it answers.
    fn health_version(&self, socket: &Path) -> Result<String>;
}

/// Restart the unit described by its plist's `unit_args` / `unit_env` (#8686).
///
/// What: resolves the unit's data dir with [`unit_data_dir_override`] BEFORE
/// scanning or signalling anything, so an undeterminable one aborts with no
/// effect. Then runs [`restart_with`] with [`scoped_daemon_pids`] over
/// `fx.candidates()` and [`terminate_scoped`] over `fx.signal`.
/// Test: `a_unit_without_program_arguments_signals_nothing`,
/// `a_dangling_data_dir_signals_nothing`,
/// `the_restart_signals_only_the_units_daemon`.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn restart_unit(
    unit_args: &[String],
    unit_env: &[(String, String)],
    platform_default: &Path,
    expected_version: &str,
    fx: &impl RestartEffects,
) -> Result<RestartReport> {
    let unit_override = unit_data_dir_override(unit_args, unit_env)?;
    let unit_dir = unit_override
        .clone()
        .unwrap_or_else(|| platform_default.to_path_buf());
    let socket = fx.socket_for(unit_override.as_deref())?;
    let scoped_now = || scoped_daemon_pids(&fx.candidates(), &unit_dir, platform_default);
    restart_with(
        scoped_now(),
        expected_version,
        || fx.bootout(),
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
                |pid, sig| fx.signal(pid, sig),
                |within, done| fx.wait_until(within, done),
            )
        },
        || fx.bootstrap(),
        || fx.health_version(&socket),
    )
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

/// The real [`RestartEffects`]: live process table, signals, `launchctl` and
/// `search.health` (#8686).
#[cfg(target_os = "macos")]
struct LaunchdEffects<'a> {
    cfg: &'a trusty_common::launchd::LaunchdConfig,
    budget_secs: u64,
}

#[cfg(target_os = "macos")]
impl RestartEffects for LaunchdEffects<'_> {
    fn candidates(&self) -> Vec<Candidate> {
        super::start::reap_orphans::observe_candidates()
    }

    fn signal(&self, pid: u32, sig: &'static str) {
        let _ = super::stop::send_signal(pid, sig);
    }

    fn wait_until(&self, within: Duration, done: &dyn Fn() -> bool) {
        let deadline = std::time::Instant::now() + within;
        while std::time::Instant::now() < deadline && !done() {
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    fn socket_for(&self, unit_override: Option<&Path>) -> Result<PathBuf> {
        unit_socket_path(unit_override)
    }

    fn bootout(&self) -> Result<()> {
        if self.cfg.is_loaded() {
            self.cfg.bootout()?;
            trusty_common::launchd_restart::await_unload(
                self.budget_secs,
                || self.cfg.is_loaded(),
                || std::thread::sleep(Duration::from_secs(1)),
            );
        }
        Ok(())
    }

    fn bootstrap(&self) -> Result<()> {
        self.cfg.bootstrap()
    }

    fn health_version(&self, socket: &Path) -> Result<String> {
        let deadline = std::time::Instant::now() + Duration::from_secs(self.budget_secs.max(60));
        loop {
            let expired = std::time::Instant::now() >= deadline;
            match probe_health_version(socket) {
                Some(v) if v == env!("CARGO_PKG_VERSION") || expired => return Ok(v),
                None if expired => bail!(
                    "no daemon answered search.health on {} after the restart (#8686)",
                    socket.display()
                ),
                _ => std::thread::sleep(Duration::from_millis(500)),
            }
        }
    }
}

/// Restart the launchd-supervised daemon and verify the result (#8686).
///
/// What: [`restart_unit`] over [`LaunchdEffects`], with the platform default
/// data dir as the fallback for a unit that declares none.
/// Test: the decisions are tested through [`restart_unit`]; this binding runs
/// real `launchctl` calls.
#[cfg(target_os = "macos")]
pub(crate) fn service_restart(
    cfg: &trusty_common::launchd::LaunchdConfig,
    unit_args: &[String],
    unit_env: &[(String, String)],
) -> Result<()> {
    let Some(platform_default) = crate::service::daemon::resolve_daemon_dir(None) else {
        bail!("the platform default data dir is unresolvable — not restarting (#4395)");
    };
    let fx = LaunchdEffects {
        cfg,
        budget_secs: trusty_common::shutdown::termination_grace().as_secs(),
    };
    let report = restart_unit(
        unit_args,
        unit_env,
        &platform_default,
        env!("CARGO_PKG_VERSION"),
        &fx,
    )?;
    println!(
        "✓ restarted: /health reports {}; replaced daemon PID(s) {:?} are gone",
        report.version, report.replaced
    );
    Ok(())
}
