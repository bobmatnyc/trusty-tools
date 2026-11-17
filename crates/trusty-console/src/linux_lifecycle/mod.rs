//! Linux `start`, `stop` and `restart` for trusty-console (#11085, epic #11083).
//!
//! Why: console had no lifecycle verbs, so `tctl restart` could not move it
//! onto a newly installed binary on Linux. D2 keeps macOS unchanged, so this
//! module compiles on Linux only. Every stop, identity and spawn rule is
//! `trusty_common::linux_lifecycle`'s; this module supplies the descriptor,
//! the `http_addr` fallback and the exit statuses.
//! What: identity and readiness go through console's UDS socket, because
//! every console TCP route can need a token (#11074, #11089). A console whose
//! socket bind failed serves HTTP only (#10902); when the socket does not
//! serve, [`stop_with`] and [`restart_with`] find the pid behind `http_addr`
//! ([`addr_pid`]) and hand it to `stop_pid`/`restart_pid`, which check it is
//! this console before any signal (AC10). `stop` on a console that is not
//! running exits 1; `restart` starts it (P-b). Every `LifecycleError` is a
//! non-zero exit with its message.
//!
//! Limits: `restart` respawns plain `serve`, so `--http`, `--tailscale` and
//! `--poll-interval` from the original start are not carried over;
//! `TRUSTY_CONSOLE_BIND` is the form that persists. A restart through
//! `http_addr` of an HTTP-only console whose socket bind keeps failing stops
//! it; the new child then fails readiness after 60 s, the console stays down,
//! and the exit is non-zero (`NotReady`).
//! Test: `tests.rs`.

pub(crate) mod addr_pid;
mod env;

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use trusty_common::DATA_DIR_OVERRIDE_ENV;
use trusty_common::linux_lifecycle::{
    self as lc, DaemonDescriptor, KernelDelivery, LifecycleError, ProcessTable, RestartOutcome,
    SignalDelivery, StartOutcome, StopOutcome, StoppedSignal,
};

use self::addr_pid::AddrLookup;

/// The executable spawned from the install dir and required of the peer.
pub(crate) const BINARY: &str = "trusty-console";

/// The daemon's argv after `argv[0]`. Only `serve` runs the daemon, so a
/// `port` or `config` CLI process of the same binary never passes the
/// liveness or identity check.
pub(crate) const DAEMON_ARGS: [&str; 1] = ["serve"];

/// `serve` polls every connector once before it binds, so readiness gets
/// twice the module's default bound. A start that hits it kills its child.
const READY_TIMEOUT: Duration = Duration::from_secs(60);

/// The descriptor plus the `http_addr` file of one console instance.
#[derive(Debug, Clone)]
pub(crate) struct Target {
    /// The module's descriptor.
    pub(crate) desc: DaemonDescriptor,
    /// `<data dir>/http_addr`, the fallback identity (AC10).
    pub(crate) addr_file: PathBuf,
}

/// How the daemon that was acted on was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Route {
    /// Through the UDS socket's peer (or nothing ran).
    Socket,
    /// Through the listener named by `http_addr`.
    HttpAddr(u32),
}

/// The target for this process's environment.
fn target() -> Result<Target> {
    let data_dir = trusty_common::resolve_data_dir(BINARY)
        .context("could not resolve the trusty-console data directory")?;
    let socket = trusty_common::daemon_socket_path(BINARY)?;
    target_for(
        std::env::var_os(DATA_DIR_OVERRIDE_ENV).as_ref(),
        socket,
        data_dir.join("http_addr"),
    )
}

/// The target for one override value, socket and `http_addr` file.
///
/// Why: #11085 — the module compares `environ` bytes exactly, so a blank or
/// non-UTF-8 override is a different instance from an unset one, while
/// `resolve_data_dir` reads all three as the live data dir. Such a value would
/// address the live socket yet miss the live console in the `/proc` scan.
/// What: `serve` as spawn and liveness args, the socket-refuses stopped
/// signal, [`env::effective_passthrough`], and `instance_env` pinned to the override
/// (the module default's key, with the value passed here).
///
/// # Errors
///
/// A blank or non-UTF-8 override, or no resolvable install dir.
///
/// Test: `descriptor_names_serve_args_socket_signal_and_instance`,
/// `descriptor_refuses_blank_and_non_utf8_override`.
pub(crate) fn target_for(
    override_value: Option<&OsString>,
    socket: PathBuf,
    addr_file: PathBuf,
) -> Result<Target> {
    match override_value.map(|v| v.to_str()) {
        Some(None) => bail!(
            "{DATA_DIR_OVERRIDE_ENV} is not valid UTF-8, which resolves the live data dir; \
             refusing to act on it. Unset it to manage the live console (#11085)"
        ),
        Some(Some(v)) if v.trim().is_empty() => bail!(
            "{DATA_DIR_OVERRIDE_ENV} is set but blank, which resolves the live data dir; \
             refusing to act on it. Unset it to manage the live console (#11085)"
        ),
        _ => {}
    }
    let mut desc = DaemonDescriptor::new(BINARY, socket, StoppedSignal::SocketRefuses)
        .context("build the trusty-console lifecycle descriptor")?;
    desc.spawn_args = DAEMON_ARGS.map(String::from).to_vec();
    desc.liveness_args = DAEMON_ARGS.map(String::from).to_vec();
    // #11085: the bridges' auto-started daemons inherit this environment.
    desc.env_passthrough = env::effective_passthrough();
    desc.instance_env = vec![(DATA_DIR_OVERRIDE_ENV.to_string(), override_value.cloned())];
    desc.ready_timeout = READY_TIMEOUT;
    Ok(Target { desc, addr_file })
}

/// `trusty-console stop` on Linux.
///
/// What: exits 1 when no console runs and on any error; removes the stale
/// `http_addr` after a stop, since a SIGKILLed console leaves it behind.
/// Test: `stop_report_maps_each_outcome_to_its_exit_status`.
pub async fn stop() -> Result<()> {
    let target = target()?;
    let stopped = stop_report(stop_with(&target, &KernelDelivery, &ProcessTable::system()).await)?;
    if stopped {
        let _ = std::fs::remove_file(&target.addr_file);
    }
    Ok(())
}

/// `trusty-console start` on Linux: an already-serving console is success.
///
/// Test: `start_report_treats_only_already_running_as_success`.
pub async fn start() -> Result<()> {
    let target = target()?;
    start_report(lc::start(&target.desc).await)
}

/// `trusty-console restart` on Linux (D1, P-b).
///
/// Test: `restart_through_http_addr_reports_a_survivor_and_starts_nothing`.
pub async fn restart() -> Result<()> {
    restart_report(restart_with(&target()?, &KernelDelivery, &ProcessTable::system()).await)
}

/// Stop through the socket, or through `http_addr` when the socket does not
/// serve.
///
/// What: the module's stop. On `NotRunning` or `RunningUnserved`, the
/// `http_addr` lookup: a pid goes to `stop_pid_with`, which signals only this
/// console. No file, or nothing listening, keeps the module's answer. A
/// lookup error is an error. The module's scans read `table`.
/// Test: `stop_finds_the_console_through_http_addr_when_its_socket_bind_failed`,
/// `stop_with_an_unserved_console_and_no_http_addr_signals_nothing`,
/// `stop_on_not_running_with_a_foreign_listener_in_http_addr_is_an_error`.
pub(crate) async fn stop_with(
    target: &Target,
    delivery: &dyn SignalDelivery,
    // #11085: injectable so a test never scans the host's processes.
    table: &ProcessTable,
) -> Result<(StopOutcome, Route)> {
    let unserved = match lc::stop_with_table(&target.desc, delivery, table).await {
        Ok(StopOutcome::NotRunning) => None,
        Err(e @ LifecycleError::RunningUnserved { .. }) => Some(e),
        other => return Ok((other?, Route::Socket)),
    };
    // #11085 AC10: a console whose socket bind failed is reachable only here.
    match fallback_pid(target, unserved.as_ref())? {
        Some(pid) => Ok((
            lc::stop_pid_with_table(&target.desc, pid, delivery, table).await?,
            Route::HttpAddr(pid),
        )),
        None => match unserved {
            Some(e) => Err(e.into()),
            None => Ok((StopOutcome::NotRunning, Route::Socket)),
        },
    }
}

/// Restart through the socket, or through `http_addr` on `RunningUnserved`.
///
/// What: the module's restart, which starts a console that is not running
/// (P-b). On `RunningUnserved`, a pid behind `http_addr` goes to
/// `restart_pid_with`; with none, the error stands. Every stop error returns
/// before a spawn. The module's scans read `table`.
/// Test: `restart_through_http_addr_reports_a_survivor_and_starts_nothing`,
/// `stop_on_nothing_running_exits_1_and_restart_reaches_start`.
pub(crate) async fn restart_with(
    target: &Target,
    delivery: &dyn SignalDelivery,
    // #11085: injectable so a test never scans the host's processes.
    table: &ProcessTable,
) -> Result<(RestartOutcome, Route)> {
    match lc::restart_with_table(&target.desc, delivery, table).await {
        Err(e @ LifecycleError::RunningUnserved { .. }) => match fallback_pid(target, Some(&e))? {
            Some(pid) => Ok((
                lc::restart_pid_with_table(&target.desc, pid, delivery, table).await?,
                Route::HttpAddr(pid),
            )),
            None => Err(e.into()),
        },
        other => Ok((other?, Route::Socket)),
    }
}

/// The pid behind `http_addr`, `None` when there is no file or no listener.
fn fallback_pid(target: &Target, unserved: Option<&LifecycleError>) -> Result<Option<u32>> {
    let found = addr_pid::lookup(&target.addr_file).with_context(|| match unserved {
        Some(e) => format!("{e}; the http_addr fallback failed"),
        None => "the console socket is not served; the http_addr fallback failed".to_string(),
    })?;
    Ok(match found {
        AddrLookup::Pid(pid, _) => Some(pid),
        AddrLookup::NoAddrFile | AddrLookup::NoListener(_) => None,
    })
}

/// The exit status of a stop: `Ok(true)` when a console was stopped.
///
/// Why: P-b keeps `stop` exiting 1 when nothing runs; every error, named or
/// not, is a failure carrying the module's message.
/// Test: `stop_report_maps_each_outcome_to_its_exit_status`.
pub(crate) fn stop_report(result: Result<(StopOutcome, Route)>) -> Result<bool> {
    match result.context("trusty-console stop")? {
        (StopOutcome::Stopped { pid, forced }, route) => {
            let how = if forced { " (SIGKILL)" } else { "" };
            let via = route_note(route);
            println!("trusty-console stopped (pid {pid}){how}{via}");
            Ok(true)
        }
        (StopOutcome::NotRunning, _) => bail!("No trusty-console running"),
        (other, _) => bail!("trusty-console stop: unexpected outcome {other:?}"),
    }
}

/// The exit status of a start.
///
/// Why: a second `start` while the console serves is a no-op; this is the
/// only arm here that turns a module error into success.
/// Test: `start_report_treats_only_already_running_as_success`.
pub(crate) fn start_report(result: Result<StartOutcome, LifecycleError>) -> Result<()> {
    match result {
        Ok(started) => {
            eprintln!("trusty-console ready (pid {})", started.pid);
            Ok(())
        }
        Err(LifecycleError::AlreadyRunning { socket, .. }) => {
            eprintln!("trusty-console is already running on {}", socket.display());
            Ok(())
        }
        Err(e) => Err(anyhow::Error::new(e).context("trusty-console start")),
    }
}

/// The exit status of a restart: success only when the new pid serves.
///
/// Test: `restart_through_http_addr_reports_a_survivor_and_starts_nothing`.
pub(crate) fn restart_report(result: Result<(RestartOutcome, Route)>) -> Result<()> {
    let (restarted, route) = result.context("trusty-console restart")?;
    let was = match restarted.stop {
        StopOutcome::Stopped { pid, .. } => format!("pid {pid} stopped"),
        _ => "was not running".to_string(),
    };
    println!(
        "trusty-console restarted ({was}{}; pid {} serving)",
        route_note(route),
        restarted.start.pid
    );
    Ok(())
}

/// ` via http_addr` for a console found through the fallback.
fn route_note(route: Route) -> &'static str {
    match route {
        Route::Socket => "",
        Route::HttpAddr(_) => " via http_addr; its socket was not served",
    }
}

#[cfg(test)]
mod tests;
