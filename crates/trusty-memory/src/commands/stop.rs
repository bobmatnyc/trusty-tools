//! Handler for `trusty-memory stop` — terminates the running daemon.
//!
//! Why: with `start` now self-spawning a background daemon, operators need a
//! matching `stop` that does not depend on launchd / systemd. The historical
//! `service stop` only worked on macOS launchd-managed installations; a
//! `start`-spawned daemon is just a detached child process whose only public
//! handle is its name on the process table and its address file at
//! `~/.trusty-memory/http_addr`.
//! What: walks the process table via `sysinfo`, collects every daemon-mode
//! `trusty-memory serve` process (not the per-session stdio bridges, not CLI
//! calls, not `cargo run`), sends SIGTERM, polls up to five seconds for them
//! to exit, and SIGKILLs stragglers. Mirrors the `trusty-search stop` flow so
//! the two daemons share a stop UX.
//! Test: `daemon_pids_in_returns_only_daemon_mode_serve_processes`,
//! `find_daemon_pids_finds_a_live_serve_foreground_process`.

use anyhow::{bail, Result};
use colored::Colorize;
use std::time::{Duration, Instant};

/// Stop every live `trusty-memory` daemon owned by this user.
///
/// Why: the daemon writes no PID file (only an `http_addr` record), so the
/// process table is the source of truth. [`find_daemon_pids`] matches only
/// daemon-mode `serve` processes, so CLI calls and the stdio bridges each MCP
/// client session runs cannot be hit (#277). Exits non-zero ("No daemon
/// running") when nothing matches so shell-scripted callers can distinguish
/// "I stopped it" from "nothing to stop".
/// What: SIGTERM phase → 5 s poll → SIGKILL phase; finally removes the stale
/// address file when every targeted process has exited.
/// Test: target selection in `daemon_pids_in_returns_only_daemon_mode_serve_processes`.
pub async fn handle_stop() -> Result<()> {
    let targets = find_daemon_pids();
    if targets.is_empty() {
        bail!("No daemon running");
    }

    println!(
        "{} Stopping trusty-memory daemon ({} process(es): {:?})…",
        "⟳".cyan(),
        targets.len(),
        targets
    );

    // Phase 1: SIGTERM all targets.
    for pid in &targets {
        let _ = send_signal(*pid, "TERM");
    }

    // Phase 2: poll up to 5 s for every targeted PID to exit.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        std::thread::sleep(Duration::from_millis(100));
        let any_alive = targets.iter().any(|p| pid_alive(*p));
        if !any_alive {
            println!("{} Daemon stopped", "✓".green());
            cleanup_addr_file();
            return Ok(());
        }
        if Instant::now() >= deadline {
            break;
        }
    }

    // Phase 3: SIGKILL anything still alive.
    let stragglers: Vec<u32> = targets.iter().copied().filter(|p| pid_alive(*p)).collect();
    if !stragglers.is_empty() {
        println!(
            "{} {} process(es) ignored SIGTERM — sending SIGKILL: {:?}",
            "⚠".yellow(),
            stragglers.len(),
            stragglers
        );
        for pid in &stragglers {
            let _ = send_signal(*pid, "KILL");
        }
        std::thread::sleep(Duration::from_millis(500));
    }

    if targets.iter().any(|p| pid_alive(*p)) {
        println!("{} Daemon may still be shutting down", "⚠".yellow());
    } else {
        println!("{} Daemon stopped", "✓".green());
        cleanup_addr_file();
    }
    Ok(())
}

/// Remove the stale `~/.trusty-memory/http_addr` after a successful stop.
///
/// Why: the daemon writes its address on bind but does not clean it up on
/// SIGKILL, so a CLI client reading the file next would chase a dead port
/// for the discovery timeout. Best-effort: an I/O error here just gets
/// silently swallowed because the daemon is already down — the next `start`
/// will overwrite the file with a fresh address anyway.
/// What: locates the file via `trusty_common::resolve_data_dir` and
/// `fs::remove_file`s it.
/// Test: covered indirectly by the stop integration path.
fn cleanup_addr_file() {
    if let Ok(dir) = trusty_common::resolve_data_dir("trusty-memory") {
        let _ = std::fs::remove_file(dir.join("http_addr"));
    }
}

/// One process-table row: pid, executable basename, argv.
#[derive(Debug, Clone)]
pub(crate) struct ProcInfo {
    pub pid: u32,
    pub name: String,
    pub argv: Vec<String>,
}

/// What a `trusty-memory` process is, read from its argv.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProcessRole {
    /// `serve --foreground` / `serve --http[=ADDR]`: the resident daemon, the
    /// one long-lived process that opens palaces.
    Daemon,
    /// Bare `serve` / `serve --stdio`: a per-session MCP bridge that never
    /// opens redb (#1078) and is never re-spawned by its client (#8351).
    StdioBridge,
    /// Any other subcommand.
    Other,
}

/// Classify a `trusty-memory` argv the way `main::serve_mode` dispatches it.
///
/// Why (#277): since #5267 bare `serve` is the stdio bridge, so "argv contains
/// `serve`" no longer means "the daemon". Every Claude session runs a bridge.
/// What: the subcommand is the first argument after argv\[0\] that is not a
/// flag (the only global flag, `-v`, takes no value). Under `serve`,
/// `--stdio` wins; otherwise `--foreground` or `--http[=ADDR]` selects the
/// daemon and no transport flag selects the bridge.
/// Test: `classify_argv_separates_the_daemon_from_stdio_bridges`.
pub(crate) fn classify_argv(argv: &[String]) -> ProcessRole {
    let args = argv.get(1..).unwrap_or_default();
    let Some(sub) = args.iter().position(|a| !a.starts_with('-')) else {
        return ProcessRole::Other;
    };
    if args[sub] != "serve" {
        return ProcessRole::Other;
    }
    let flags = &args[sub + 1..];
    let has = |f: &str| flags.iter().any(|a| a == f);
    if has("--stdio") {
        ProcessRole::StdioBridge
    } else if has("--foreground")
        || flags
            .iter()
            .any(|a| a == "--http" || a.starts_with("--http="))
    {
        ProcessRole::Daemon
    } else {
        ProcessRole::StdioBridge
    }
}

/// The daemon PIDs in `procs`, excluding `me`.
///
/// Why: the decision, separated from the process-table read so a test can
/// inject the table.
/// What: keeps rows whose executable basename is `trusty-memory` (so `cargo
/// run -p trusty-memory -- serve` is never matched) and whose argv classifies
/// as [`ProcessRole::Daemon`]. Stdio bridges are excluded: they hold no
/// palace, and killing one strands its client session without memory tools.
/// Test: `daemon_pids_in_returns_only_daemon_mode_serve_processes`,
/// `daemon_pids_in_is_empty_when_only_bridges_run`.
pub(crate) fn daemon_pids_in(procs: &[ProcInfo], me: u32) -> Vec<u32> {
    let mut out: Vec<u32> = procs
        .iter()
        .filter(|p| p.pid != me && p.name == "trusty-memory")
        .filter(|p| classify_argv(&p.argv) == ProcessRole::Daemon)
        .map(|p| p.pid)
        .collect();
    out.sort_unstable();
    out
}

/// Read the process table with each process' argv loaded.
///
/// Why (#277): `ProcessRefreshKind::nothing()` and `refresh_processes` never
/// load argv, so `cmd()` was always empty and the scan matched nothing.
/// What: one refresh with `with_cmd(UpdateKind::Always)`.
/// Test: `find_daemon_pids_finds_a_live_serve_foreground_process`.
pub(crate) fn list_processes() -> Vec<ProcInfo> {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always),
    );
    sys.processes()
        .iter()
        .map(|(pid, p)| ProcInfo {
            pid: pid.as_u32(),
            name: p.name().to_string_lossy().into_owned(),
            argv: p
                .cmd()
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect(),
        })
        .collect()
}

/// Every live `trusty-memory` daemon PID, excluding this process.
///
/// Why: the daemon writes no PID file, so the process table is the source of
/// truth for `stop` and for `import kuzu`'s live-daemon refusal.
/// What: [`daemon_pids_in`] over [`list_processes`].
/// Test: `find_daemon_pids_finds_a_live_serve_foreground_process`.
pub(crate) fn find_daemon_pids() -> Vec<u32> {
    daemon_pids_in(&list_processes(), std::process::id())
}

/// Send a POSIX signal to a PID by shelling out to `/bin/kill`.
///
/// Why: avoid pulling in `nix` just for this — the crate already runs
/// `colored`-friendly user output, and `kill -SIGNAL pid` is universally
/// available on every Unix `trusty-memory` is supported on (macOS, Linux).
/// What: spawns `kill -<sig> <pid>` and returns an error if the exit status
/// is non-zero.
/// Test: covered indirectly by the stop integration path.
#[cfg(unix)]
fn send_signal(pid: u32, sig: &str) -> std::io::Result<()> {
    let status = std::process::Command::new("kill")
        .arg(format!("-{sig}"))
        .arg(pid.to_string())
        .status()?;
    if !status.success() {
        return Err(std::io::Error::other(format!(
            "kill -{sig} {pid} exited {status}"
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
fn send_signal(_pid: u32, _sig: &str) -> std::io::Result<()> {
    Err(std::io::Error::other(
        "signals unsupported on this platform",
    ))
}

/// Check whether a PID is still alive (Unix only).
///
/// Why: the SIGTERM-then-SIGKILL poll loop needs a portable "is this PID
/// alive?" probe. `kill(pid, 0)` returns success when the process exists
/// and EPERM when it exists but we cannot signal it — both count as "alive"
/// for the purposes of the poll.
/// What: invokes `kill -0 <pid>` via `Command` so we do not pull in `nix`.
/// Test: covered indirectly by the stop integration path.
#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn pid_alive(_pid: u32) -> bool {
    true
}

#[cfg(test)]
#[path = "stop_tests.rs"]
mod tests;
