//! Handler for `trusty-search stop`.

use super::daemon_utils::daemon_port_path;
use anyhow::{bail, Result};
use colored::Colorize;
use std::time::{Duration, Instant};

/// Why: extracted from `main()`. Stopping involves PID-file lookup, SIGTERM,
/// and a poll loop — clearer in its own function.
/// What: reads `~/.local/share/trusty-search/daemon.lock` for the PID, sends
/// SIGTERM, then waits up to 5 s for the daemon's port file to disappear.
/// Additionally scans the process table for ANY other live `trusty-search`
/// daemon processes (closes #81 — orphans left running when the lockfile
/// went stale could consume unbounded RAM) and terminates them too.
/// Exits 1 only if NOTHING is killed (no lockfile + no orphans).
/// Test: with a running daemon → "Daemon stopped" within 5 s. Spawn two
/// `trusty-search start` instances; stop must reap both.
pub async fn handle_stop() -> Result<()> {
    // The daemon writes its PID into the fs4 lockfile at startup
    // (see trusty-search-service/src/daemon.rs). Read the PID, send
    // SIGTERM, then poll for the port file to disappear as a signal
    // that shutdown completed cleanly.
    // Resolve lock path via the same override logic as daemon_dir() so that
    // `stop` targets the correct daemon when TRUSTY_DATA_DIR is set (issue #281).
    let lock_path = if let Ok(dir) = std::env::var("TRUSTY_DATA_DIR") {
        Some(std::path::PathBuf::from(dir).join("daemon.lock"))
    } else {
        dirs::data_local_dir().map(|d| d.join("trusty-search").join("daemon.lock"))
    };
    let port_path = daemon_port_path();

    let primary_pid = lock_path
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| s.trim().parse::<u32>().ok());

    // Collect every live trusty-search daemon process, regardless of whether
    // it matches the lockfile. The historical bug: `stop` only knew about
    // the PID in the lockfile, so if `start` was invoked twice (or the
    // lockfile went stale while a daemon kept running with PPID=1), orphan
    // daemons stayed alive forever and consumed gigabytes of RAM.
    let mut targets: Vec<u32> = find_daemon_pids();
    if let Some(p) = primary_pid {
        if !targets.contains(&p) {
            targets.push(p);
        }
    }
    // Never kill our own process (defensive: find_daemon_pids filters this
    // already, but a future caller could share the binary name).
    let me = std::process::id();
    targets.retain(|&pid| pid != me);

    if targets.is_empty() {
        bail!("No daemon running");
    }

    if let Some(p) = primary_pid {
        println!("{} Stopping daemon (PID {})…", "⟳".cyan(), p);
    }
    let orphans: Vec<u32> = targets
        .iter()
        .copied()
        .filter(|p| Some(*p) != primary_pid)
        .collect();
    if !orphans.is_empty() {
        println!(
            "{} Found {} orphan trusty-search process(es): {:?} — terminating",
            "⚠".yellow(),
            orphans.len(),
            orphans
        );
    }

    // Phase 1: SIGTERM all targets.
    for pid in &targets {
        let _ = send_signal(*pid, "TERM");
    }

    // Phase 2: poll up to 5 s for the lockfile-owning daemon to release the
    // port file AND for every targeted PID to exit.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        std::thread::sleep(Duration::from_millis(100));
        let any_alive = targets.iter().any(|p| pid_alive(*p));
        let port_gone = port_path.as_ref().map(|p| !p.exists()).unwrap_or(true);
        if !any_alive && port_gone {
            println!("{} Daemon stopped", "✓".green());
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

    // Final cleanup: stale port file from the SIGKILL'd daemon.
    if let Some(p) = port_path.as_ref() {
        if p.exists() && !targets.iter().any(|pid| pid_alive(*pid)) {
            let _ = std::fs::remove_file(p);
        }
    }

    if targets.iter().any(|p| pid_alive(*p)) {
        println!("{} Daemon may still be shutting down", "⚠".yellow());
    } else {
        println!("{} Daemon stopped", "✓".green());
    }
    Ok(())
}

/// Why: `pgrep -x trusty-search` would work on macOS/Linux but we already
/// depend on `sysinfo` and it's portable.
/// What: returns the PIDs of every process whose executable name is
/// `trusty-search`, excluding the current process. Filters by full process
/// name (not cmdline) to avoid matching `cargo run --bin trusty-search`
/// or grep'ing scripts that mention the string.
/// Test: in a process tree with two `trusty-search` daemons, returns both;
/// in a tree with only the calling CLI, returns empty.
pub(crate) fn find_daemon_pids() -> Vec<u32> {
    use sysinfo::{ProcessRefreshKind, RefreshKind, System};
    let mut sys = System::new_with_specifics(
        RefreshKind::nothing().with_processes(ProcessRefreshKind::nothing()),
    );
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    let me = std::process::id();
    let mut out = Vec::new();
    for (pid, proc_) in sys.processes() {
        let raw = pid.as_u32();
        if raw == me {
            continue;
        }
        // `name()` is the executable basename. We deliberately do NOT match
        // against `cmd()` so that `cargo`, shells, and editors that mention
        // "trusty-search" in their argv don't get killed.
        if proc_.name().to_string_lossy() == "trusty-search" {
            // Exclude short-lived CLI invocations (`trusty-search status`,
            // `query`, etc.) by checking for a long-running daemon: only
            // daemons listen on the HTTP port, so we identify them by the
            // presence of the `start` subcommand in their argv.
            let is_daemon = proc_.cmd().iter().any(|a| a.to_string_lossy() == "start");
            if is_daemon {
                out.push(raw);
            }
        }
    }
    out
}

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

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    match nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None) {
        Ok(()) => true,
        // EPERM means the process exists but we cannot signal it.
        Err(nix::errno::Errno::EPERM) => true,
        Err(_) => false,
    }
}

#[cfg(not(unix))]
fn pid_alive(_pid: u32) -> bool {
    true
}
