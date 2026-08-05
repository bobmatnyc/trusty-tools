//! Handler for `trusty-search stop`.

use super::daemon_utils::daemon_port_path;
use anyhow::{bail, Result};
use colored::Colorize;
use std::time::{Duration, Instant};

/// Why: extracted from `main()`. Stopping involves PID-file lookup, SIGTERM,
/// and a poll loop — clearer in its own function.
/// What: reads `~/.local/share/trusty-search/daemon.lock` for the PID, sends
/// SIGTERM, then waits up to
/// [`trusty_common::shutdown::termination_grace`] (#4393) for the daemon's port
/// file to disappear.
/// Additionally scans the process table for ANY other live `trusty-search`
/// daemon processes (closes #81 — orphans left running when the lockfile
/// went stale could consume unbounded RAM) and terminates them too.
///
/// #4395 scope note: `stop` deliberately keeps its stop-everything semantics —
/// it is an explicit operator command whose documented contract since #81 is
/// "nothing named trusty-search is left running". The IMPLICIT reaper inside
/// `start` is the one that had to become ownership-aware, because nobody asked
/// it to kill anything; see `commands::start::reap_orphans`.
///
/// Exits 1 only if NOTHING is killed (no lockfile + no orphans).
/// Test: with a running daemon → "Daemon stopped" within the grace window. Spawn
/// two `trusty-search start` instances; stop must reap both.
/// `stop_window_covers_the_flush_floor` pins the window against the flush floor.
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

    // #4113: under launchd this stop is temporary — say so, and name the
    // command that isn't.
    #[cfg(target_os = "macos")]
    if let Some(notice) = launchd_restart_notice(crate::commands::service::launchd_agent_loaded()) {
        println!("{} {notice}", "·".dimmed());
    }

    // Phase 1: SIGTERM all targets.
    for pid in &targets {
        let _ = send_signal(*pid, "TERM");
    }

    // Phase 2: poll for the lockfile-owning daemon to release the port file AND
    // for every targeted PID to exit.
    //
    // #4393: this window was 5 s, against a shutdown flush that floors at 30 s
    // per index — so `trusty-search stop` SIGKILLed a daemon that was mid-flush
    // on every stop that had real work to do. It now uses the same termination
    // grace launchd's `ExitTimeOut` and the orphan reaper use, so the three
    // paths cannot disagree about how long a daemon has to finish.
    let grace = trusty_common::shutdown::termination_grace();
    let deadline = Instant::now() + grace;
    let mut last_notice = Instant::now();
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
        // A 60 s silent wait reads as a hang. Say what is being waited on.
        if last_notice.elapsed() >= Duration::from_secs(5) {
            last_notice = Instant::now();
            println!(
                "{} still flushing index snapshots… (up to {}s, #4393)",
                "·".dimmed(),
                grace.as_secs()
            );
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

/// Build the operator notice explaining that a launchd-supervised stop is
/// temporary, or `None` when the agent is not loaded.
///
/// Why: #4113 gave the LaunchAgent `KeepAlive=true` so a clean exit no longer
/// leaves search down forever. The cost is that `trusty-search stop` alone no
/// longer means "stay stopped" on a supervised host — launchd re-launches the
/// daemon after its throttle interval. Announcing that at the moment of the
/// stop, with the `launchctl bootout` command that does keep it stopped, means
/// the new policy never reads as "stop is broken".
/// What: pure — returns `None` when `agent_loaded` is false, otherwise the
/// message naming both the reason and the bootout target. Split out from the
/// print so the wording is unit-testable without touching `launchctl`.
/// Test: `launchd_restart_notice_absent_when_agent_not_loaded`,
/// `launchd_restart_notice_names_bootout_command`.
#[cfg(target_os = "macos")]
fn launchd_restart_notice(agent_loaded: bool) -> Option<String> {
    if !agent_loaded {
        return None;
    }
    let label = crate::commands::service::LAUNCHD_LABEL;
    Some(format!(
        "launchd will restart the daemon shortly ({label} sets KeepAlive=true, \
         issue #4113).\n  To stop it and keep it stopped: launchctl bootout \
         gui/$(id -u)/{label}"
    ))
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

#[cfg(test)]
mod window_tests {
    /// Why (#4393 — the mismatch, on the path the original filing missed
    /// entirely): `stop` allowed 5 s before SIGKILL while the daemon's own
    /// shutdown flush floors at 30 s per index, so a stop issued while an index
    /// had real work to flush killed it mid-write. Every termination path has to
    /// clear that floor or the flush is decorative.
    /// What: asserts the grace window `handle_stop` waits for covers
    /// `shutdown_flush::MIN_FLUSH_TIMEOUT_SECS`.
    /// Test: this IS the test.
    #[test]
    fn stop_window_covers_the_flush_floor() {
        let window = trusty_common::shutdown::termination_grace();
        let floor = std::time::Duration::from_secs(
            crate::service::shutdown_flush::MIN_FLUSH_TIMEOUT_SECS,
        );
        assert!(
            window >= floor,
            "`stop`'s SIGKILL window ({window:?}) must cover the daemon's per-index \
             flush floor ({floor:?}) — 5 s against 30 s is the #4393 defect"
        );
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    /// Why: an unsupervised daemon really does stay stopped, so printing a
    /// launchd notice there would be wrong information, not extra help.
    /// What: asserts the notice is `None` when the agent is not loaded.
    /// Test: itself.
    #[test]
    fn launchd_restart_notice_absent_when_agent_not_loaded() {
        assert_eq!(launchd_restart_notice(false), None);
    }

    /// Why: the notice only earns its place if it hands the operator the
    /// command that actually keeps the daemon stopped — the whole tradeoff
    /// #4113 accepts is that `bootout`, not `stop`, is now the durable stop.
    /// What: asserts the message names the LaunchAgent label, the #4113
    /// rationale, and a `launchctl bootout` invocation.
    /// Test: itself.
    #[test]
    fn launchd_restart_notice_names_bootout_command() {
        let notice = launchd_restart_notice(true).expect("loaded agent must produce a notice");
        assert!(
            notice.contains(crate::commands::service::LAUNCHD_LABEL),
            "notice must name the LaunchAgent label, got: {notice}"
        );
        assert!(
            notice.contains("launchctl bootout"),
            "notice must give the command that keeps the daemon stopped, got: {notice}"
        );
        assert!(
            notice.contains("#4113"),
            "notice must cite the issue that changed the policy, got: {notice}"
        );
    }
}
