//! Linux `start`, `stop` and `restart` for trusty-search (#11085, epic #11083).
//!
//! Why: on Linux, `stop` signalled the pid in `daemon.lock` unchecked, its
//! process scan never requested argv and so found nothing, and it exited 0
//! while a process survived; `start` returned before the daemon served; and
//! there was no `restart`, so `tctl restart` could not move the daemon onto a
//! new binary. D2 keeps macOS unchanged, so this file is the Linux path only.
//! Every stop, identity and spawn rule is `trusty_common::linux_lifecycle`'s;
//! this file supplies the descriptor and maps outcomes to exit statuses.
//! What: [`descriptor_for`] resolves the data dir the same way for every verb
//! (flag, then `TRUSTY_DATA_DIR`) and derives the socket and the `daemon.lock`
//! "stopped" signal from it. [`stop`] exits 1 when nothing runs; [`restart`]
//! starts a daemon that is not running (P-b) and refuses, sending nothing,
//! while the `trusty-search.service` systemd unit is active (AC9).
//! Test: `linux_lifecycle_tests.rs`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context as _, Result};
use colored::Colorize;
use trusty_common::linux_lifecycle::{
    self as lc, DaemonDescriptor, KernelDelivery, LifecycleError, ProcessTable, SignalDelivery,
    StopOutcome, StoppedSignal,
};
use trusty_common::DATA_DIR_OVERRIDE_ENV;

use super::start::DATA_DIR_ENV;

#[path = "linux_lifecycle_env.rs"]
mod env;
pub(crate) use env::ENV_PASSTHROUGH;

/// The executable the module spawns from the install dir and requires of the
/// socket's peer.
pub(crate) const BINARY: &str = "trusty-search";

/// The daemon's argv after `argv[0]`; a stdio bridge runs `serve`.
pub(crate) const DAEMON_ARGS: [&str; 2] = ["start", "--foreground"];

/// The systemd unit `deploy/systemd/` ships; `restart` defers to it (AC9).
pub(crate) const SYSTEMD_UNIT: &str = "trusty-search.service";

/// How long a spawned daemon may take to serve its socket. A start that hits
/// the bound kills its child, so the bound is twice the auto-start budget
/// (`daemon_guard::READY_TIMEOUT`).
const READY_TIMEOUT: Duration = Duration::from_secs(120);

/// The arguments `stop` and `restart` take on Linux.
#[derive(clap::Args, Debug, Clone, Default)]
pub struct LifecycleArgs {
    /// The daemon's data directory, as given to `start --data-dir`. Falls back
    /// to `TRUSTY_DATA_DIR`, then the default data dir.
    #[arg(long, value_name = "DIR")]
    pub data_dir: Option<PathBuf>,
}

/// The descriptor for one trusty-search instance.
///
/// Why: AC6 — `stop` read only `TRUSTY_DATA_DIR` while `start` took
/// `--data-dir`, so a daemon started with the flag could not be stopped. One
/// resolver now serves every verb. The resolved dir is pinned in
/// `instance_env` as `TRUSTY_DATA_DIR` and set on the spawned daemon, so the
/// module's `/proc` scan tells this instance from a sibling with another data
/// dir. The resolved dir — the default data dir for the default instance — is
/// also the `--data-dir` entry of `instance_path_args`, so a sibling started
/// with `--data-dir X` and no `TRUSTY_DATA_DIR` is told apart by its argv.
/// A blank `TRUSTY_DATA_DIR_OVERRIDE` is refused: the module compares
/// environ bytes exactly, so blank and unset would read as different
/// instances while both resolve the live socket.
/// What: `flag` wins over `env_data_dir` (empty counts as unset; the result
/// must be absolute). The socket is `socket_flag` or the data dir's socket;
/// the stopped signal is the `daemon.lock` flock (the daemon holds it through
/// its shutdown flush). Spawn and liveness args are `start --foreground`, plus
/// `--data-dir <dir>` when the flag was given. Stop grace is the termination
/// grace the shutdown flush is sized against (#4393).
///
/// # Errors
///
/// A blank override, a relative or non-UTF-8 data dir, or no resolvable
/// data or install dir.
///
/// Test: `descriptor_resolves_the_data_dir_the_same_way_for_every_verb`,
/// `descriptor_refuses_a_blank_override_and_a_relative_data_dir`.
pub(crate) fn descriptor_for(
    flag: Option<&Path>,
    env_data_dir: Option<OsString>,
    override_value: Option<&OsString>,
    socket_flag: Option<&Path>,
) -> Result<DaemonDescriptor> {
    if override_value.is_some_and(|v| v.to_string_lossy().trim().is_empty()) {
        bail!(
            "{DATA_DIR_OVERRIDE_ENV} is set but blank, which resolves the live data dir; \
             refusing to act on it. Unset it to manage the live daemon (#11085)"
        );
    }
    let data_dir = super::start::resolve_data_dir_override(env_data_dir, flag)?;
    let data_os = data_dir.as_deref().map(Path::as_os_str);
    let daemon_dir = crate::service::daemon::resolve_daemon_dir(data_os)
        .context("could not determine the trusty-search data directory")?;
    let lock = daemon_dir.join("daemon.lock");
    let socket = match socket_flag {
        Some(path) => path.to_path_buf(),
        None => crate::service::socket::resolve_socket_path(data_os)?,
    };
    let mut desc = DaemonDescriptor::new(BINARY, socket, StoppedSignal::LockFree(lock))
        .context("build the trusty-search lifecycle descriptor")?;
    let mut daemon_args: Vec<String> = DAEMON_ARGS.map(String::from).to_vec();
    if let Some(dir) = flag.and(data_dir.as_deref()) {
        let dir = dir
            .to_str()
            .with_context(|| format!("--data-dir must be UTF-8 (got {})", dir.display()))?;
        daemon_args.extend(["--data-dir".to_string(), dir.to_string()]);
    }
    desc.spawn_args = daemon_args.clone();
    desc.liveness_args = daemon_args;
    // #11085: pinned, so a spawned daemon carries it and the scan can tell it apart.
    desc.instance_env.push((
        DATA_DIR_ENV.to_string(),
        data_dir.map(PathBuf::into_os_string),
    ));
    // #11085 AC6: a `--data-dir X` sibling without TRUSTY_DATA_DIR is not this instance.
    desc.instance_path_args = vec![("--data-dir".to_string(), daemon_dir)];
    desc.env_passthrough = ENV_PASSTHROUGH.iter().map(|s| (*s).to_string()).collect();
    desc.stop_grace = trusty_common::shutdown::termination_grace();
    desc.ready_timeout = READY_TIMEOUT;
    Ok(desc)
}

/// [`descriptor_for`] from this process's environment.
fn descriptor(flag: Option<&Path>, socket_flag: Option<&Path>) -> Result<DaemonDescriptor> {
    descriptor_for(
        flag,
        std::env::var_os(DATA_DIR_ENV),
        std::env::var_os(DATA_DIR_OVERRIDE_ENV).as_ref(),
        socket_flag,
    )
}

/// `trusty-search stop` on Linux.
///
/// What: stops only the daemon serving this data dir's socket, and only once
/// it is proven the `trusty-search` binary; exits 1 when none runs and when a
/// process survives (AC3). The pid in `daemon.lock` is never signalled (AC8).
/// Test: `stop_report_maps_each_outcome_to_its_exit_status`,
/// `stop_with_the_data_dir_flag_stops_the_daemon_serving_that_dir`,
/// `stop_never_signals_a_foreign_pid_named_in_daemon_lock`.
pub async fn stop(args: LifecycleArgs) -> Result<()> {
    let desc = descriptor(args.data_dir.as_deref(), None)?;
    stop_report(lc::stop(&desc).await).map(|_| ())
}

/// `trusty-search start` (detached) on Linux.
///
/// What: spawns the install-dir binary with `extra_args` after the daemon
/// args and returns only once that pid serves the socket (AC5).
/// Test: `start_report_fails_every_module_error`.
pub(crate) async fn start(
    data_dir: Option<&Path>,
    socket: Option<&Path>,
    extra_args: Vec<String>,
) -> Result<()> {
    let mut desc = descriptor(data_dir, socket)?;
    desc.spawn_args.extend(extra_args);
    eprintln!(
        "{} Starting trusty-search daemon from {}…",
        "◉".cyan(),
        desc.program().display()
    );
    start_report(lc::start(&desc).await)
}

/// `trusty-search restart` on Linux (D1, P-b, AC9).
///
/// What: [`restart_with`] with the real systemd probe, the kernel and `/proc`.
/// Test: `restart_refuses_while_the_systemd_unit_is_active`.
pub async fn restart(args: LifecycleArgs) -> Result<()> {
    let desc = descriptor(args.data_dir.as_deref(), None)?;
    restart_with(&desc, &KernelDelivery, &ProcessTable::system(), || {
        systemd_unit_active(Path::new("systemctl"))
    })
    .await
}

/// [`restart`] with the descriptor, signal delivery, process table and systemd
/// probe supplied.
///
/// What: refuses before any signal while `probe` reports the unit active;
/// otherwise the module's restart, scanning `table` — any stop error is
/// returned before a spawn, and `NotRunning` proceeds to the start.
/// Test: `restart_refuses_while_the_systemd_unit_is_active`,
/// `restart_names_a_survivor_and_starts_nothing`,
/// `restart_of_a_daemon_that_is_not_running_goes_on_to_start`.
pub(crate) async fn restart_with(
    desc: &DaemonDescriptor,
    delivery: &dyn SignalDelivery,
    // #11085: injectable so a test never scans the host's processes.
    table: &ProcessTable,
    probe: impl Fn() -> Option<UnitScope>,
) -> Result<()> {
    refuse_under_systemd(probe)?;
    restart_report(lc::restart_with_table(desc, delivery, table).await)
}

/// Which systemd scope runs the unit, when one does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnitScope {
    /// `systemctl` (the shipped unit is a system unit).
    System,
    /// `systemctl --user`.
    User,
}

/// Refuse when `probe` reports the unit active (AC9).
///
/// Why: systemd restarts the daemon it supervises (`Restart=on-failure`); a
/// process-level restart under it races the supervisor.
/// What: `Err` naming `systemctl [--user] restart trusty-search.service` when
/// `probe` returns a scope; `Ok` otherwise.
/// Test: `restart_refuses_while_the_systemd_unit_is_active`.
pub(crate) fn refuse_under_systemd(probe: impl Fn() -> Option<UnitScope>) -> Result<()> {
    let command = match probe() {
        None => return Ok(()),
        Some(UnitScope::System) => format!("sudo systemctl restart {SYSTEMD_UNIT}"),
        Some(UnitScope::User) => format!("systemctl --user restart {SYSTEMD_UNIT}"),
    };
    bail!("{SYSTEMD_UNIT} is active, so systemd owns this daemon; run: {command}")
}

/// Whether `systemctl is-active` reports the unit active, system scope first.
///
/// Why: AC9 needs the unit's state without a systemd library.
/// What: runs `program [--user] is-active --quiet <unit>`; exit 0 is active.
/// A spawn failure (no `systemctl` on the host) or any other status is "not
/// active" — the restart then runs its own identity-checked stop, which
/// signals only a `trusty-search` serving this caller's socket.
/// Test: `systemd_probe_reads_only_exit_zero_as_active`.
pub(crate) fn systemd_unit_active(program: &Path) -> Option<UnitScope> {
    let active = |user: bool| {
        let mut cmd = std::process::Command::new(program);
        if user {
            cmd.arg("--user");
        }
        cmd.args(["is-active", "--quiet", SYSTEMD_UNIT])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            // #11085: fail-open guard `systemd_probe_reads_only_exit_zero_as_active`.
            .is_ok_and(|s| s.success())
    };
    if active(false) {
        Some(UnitScope::System)
    } else if active(true) {
        Some(UnitScope::User)
    } else {
        None
    }
}

/// The exit status of a stop: `Ok(true)` when it stopped a daemon.
///
/// Why: AC3 — the old stop printed "may still be shutting down" and exited 0
/// over a survivor. Every module error is an error here, and `NotRunning`
/// keeps exit 1 (P-b).
/// Test: `stop_report_maps_each_outcome_to_its_exit_status`.
pub(crate) fn stop_report(result: Result<StopOutcome, LifecycleError>) -> Result<bool> {
    match result.context("trusty-search stop")? {
        StopOutcome::Stopped { pid, forced } => {
            let how = if forced { " (SIGKILL)" } else { "" };
            println!("{} Daemon stopped (pid {pid}){how}", "✓".green());
            Ok(true)
        }
        StopOutcome::NotRunning => bail!("No daemon running"),
        other => bail!("trusty-search stop: unexpected outcome {other:?}"),
    }
}

/// The exit status of a start: success only when the spawned pid serves.
///
/// Why: AC5 — the old start returned right after the spawn. An already
/// serving daemon is an error, as `refuse_if_already_running` makes it.
/// Test: `start_report_fails_every_module_error`.
pub(crate) fn start_report(result: Result<lc::StartOutcome, LifecycleError>) -> Result<()> {
    let started = result.context("trusty-search start")?;
    eprintln!(
        "{} trusty-search daemon ready (pid {})",
        "✓".green(),
        started.pid
    );
    Ok(())
}

/// The exit status of a restart: success only when the new pid serves.
///
/// Test: `restart_names_a_survivor_and_starts_nothing`.
pub(crate) fn restart_report(result: Result<lc::RestartOutcome, LifecycleError>) -> Result<()> {
    let restarted = result.context("trusty-search restart")?;
    let was = match restarted.stop {
        StopOutcome::Stopped { pid, .. } => format!("pid {pid} stopped"),
        _ => "was not running".to_string(),
    };
    println!(
        "{} trusty-search restarted ({was}; pid {} serving)",
        "✓".green(),
        restarted.start.pid
    );
    Ok(())
}

#[cfg(test)]
#[path = "linux_lifecycle_tests.rs"]
mod tests;
