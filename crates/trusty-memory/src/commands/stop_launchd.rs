//! `trusty-memory stop` against a daemon launchd supervises (#8750).
//!
//! Why: `stop` found its target only in the process table. A launchd-spawned
//! `serve --foreground` daemon that the scan did not classify — argv the table
//! could not read, or a unit between respawns — was reported as "No daemon
//! running" while launchd kept it alive. The supported way to stop a launchd
//! job is through its label.
//! What: [`stop_with_unit`] asks the `com.trusty.memory` unit for its pid
//! first. A running unit gets `launchctl kill SIGTERM gui/<uid>/<label>` and up
//! to the termination grace to drain; then the table scan stops any daemon the
//! unit does not own. The unit is never booted out: the plist's
//! `KeepAlive = { SuccessfulExit = false }` leaves a cleanly exited daemon
//! down, and the unit stays loaded for `service start` and the next login.
//! That keeps the per-crate stop policy #4113 and #4230 rely on; `service stop`
//! remains the command that unloads the unit.
//! Test: `stop_terminates_a_launchd_daemon_the_process_table_misses`,
//! `stop_propagates_a_failed_launchd_kill`,
//! `stop_fails_when_the_launchd_daemon_outlives_the_grace`,
//! `stop_does_not_report_no_daemon_when_launchd_cannot_be_queried`.

use anyhow::{bail, Context, Result};
use colored::Colorize;
use std::time::{Duration, Instant};

use super::{daemon_pids_in, pid_alive, stop_daemons_in, ProcInfo};

/// launchd's view of the daemon's LaunchAgent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnitState {
    /// No unit is loaded under the label, or the platform has no launchd.
    NotLoaded,
    /// The unit is loaded. `pid` is its running process; `None` between spawns.
    Loaded { pid: Option<u32> },
}

/// The route `stop` takes to launchd.
///
/// Why: the seam keeps every test away from the real user launchd domain —
/// a test must never signal the live installed daemon.
/// What: the unit's label, its state, and a SIGTERM delivered by label.
/// Test: `stop_terminates_a_launchd_daemon_the_process_table_misses`.
pub(crate) trait LaunchdUnit {
    /// The unit's launchd label.
    fn label(&self) -> &str;
    /// Whether the unit is loaded, and its pid.
    fn state(&self) -> Result<UnitState>;
    /// `launchctl kill SIGTERM` the unit's process, addressed by label.
    fn terminate(&self) -> Result<()>;
}

/// The real `com.trusty.memory` LaunchAgent in the user's GUI domain.
#[cfg(target_os = "macos")]
pub(crate) struct UserLaunchAgent;

#[cfg(target_os = "macos")]
impl UserLaunchAgent {
    fn target(&self) -> String {
        format!(
            "gui/{}/{}",
            trusty_common::launchd::current_uid(),
            self.label()
        )
    }
}

#[cfg(target_os = "macos")]
impl LaunchdUnit for UserLaunchAgent {
    fn label(&self) -> &str {
        super::super::service::LAUNCHD_LABEL
    }

    fn state(&self) -> Result<UnitState> {
        let out = std::process::Command::new("launchctl")
            .arg("print")
            .arg(self.target())
            .output()
            .context("run launchctl print")?;
        // launchctl print exits non-zero for a label that is not loaded.
        if !out.status.success() {
            return Ok(UnitState::NotLoaded);
        }
        let printed = String::from_utf8_lossy(&out.stdout);
        Ok(UnitState::Loaded {
            pid: trusty_common::launchd_grace::parse_launchctl_pid(&printed),
        })
    }

    fn terminate(&self) -> Result<()> {
        let target = self.target();
        let out = std::process::Command::new("launchctl")
            .args(["kill", "SIGTERM"])
            .arg(&target)
            .output()
            .context("run launchctl kill")?;
        if !out.status.success() {
            bail!(
                "launchctl kill SIGTERM {target} exited {}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(())
    }
}

/// No launchd on this platform: the unit is never loaded.
#[cfg(not(target_os = "macos"))]
pub(crate) struct UserLaunchAgent;

#[cfg(not(target_os = "macos"))]
impl LaunchdUnit for UserLaunchAgent {
    fn label(&self) -> &str {
        "launchd"
    }

    fn state(&self) -> Result<UnitState> {
        Ok(UnitState::NotLoaded)
    }

    fn terminate(&self) -> Result<()> {
        Ok(())
    }
}

/// Stop the launchd unit's daemon, then every other daemon in `procs`.
///
/// Why: see the module doc.
/// What: when `unit` reports a live pid (not `me`), terminates it by label and
/// waits up to `unit_grace`. It is never SIGKILLed: a signal death is a
/// non-zero exit, which `KeepAlive` answers with a respawn. The remaining
/// table daemons go through [`stop_daemons_in`] with `grace`. A unit whose
/// state cannot be read does not count as "no daemon": with nothing in the
/// table either, the launchd error is the result.
///
/// # Errors
///
/// A failed `launchctl kill`, naming the label; a launchd daemon still alive
/// after `unit_grace`; an unreadable unit with an empty table; and every
/// [`stop_daemons_in`] error. "No daemon running" only when launchd answered
/// and neither it nor the table holds a daemon.
///
/// Test: `stop_terminates_a_launchd_daemon_the_process_table_misses`,
/// `stop_propagates_a_failed_launchd_kill`,
/// `stop_fails_when_the_launchd_daemon_outlives_the_grace`,
/// `stop_does_not_report_no_daemon_when_launchd_cannot_be_queried`.
pub(crate) fn stop_with_unit(
    procs: &[ProcInfo],
    me: u32,
    grace: Duration,
    unit: &dyn LaunchdUnit,
    unit_grace: Duration,
) -> Result<()> {
    let label = unit.label().to_string();
    let unit_pid = match unit.state() {
        Ok(UnitState::Loaded { pid }) => pid.filter(|p| *p != me && pid_alive(*p)),
        Ok(UnitState::NotLoaded) => None,
        Err(e) => {
            // #8750: unknown is not "nothing running" — keep the error.
            if daemon_pids_in(procs, me).is_empty() {
                return Err(e.context(format!(
                    "could not query launchd unit {label}, and the process table \
                     holds no trusty-memory daemon"
                )));
            }
            eprintln!(
                "{} could not query launchd unit {label}: {e:#}",
                "⚠".yellow()
            );
            None
        }
    };
    let Some(pid) = unit_pid else {
        return stop_daemons_in(procs, me, grace);
    };
    println!(
        "{} Stopping launchd unit {label} (pid {pid}; up to {}s to drain)…",
        "⟳".cyan(),
        unit_grace.as_secs()
    );
    unit.terminate()
        .with_context(|| format!("stop launchd unit {label} (#8750)"))?;
    if !wait_for_exit(pid, unit_grace) {
        bail!(
            "launchd unit {label}: daemon pid {pid} still running {}s after SIGTERM",
            unit_grace.as_secs()
        );
    }
    println!(
        "{} Daemon stopped; {label} stays loaded and restarts on `trusty-memory \
         service start` or the next login",
        "✓".green()
    );
    let rest: Vec<ProcInfo> = procs.iter().filter(|p| p.pid != pid).cloned().collect();
    if daemon_pids_in(&rest, me).is_empty() {
        return Ok(());
    }
    stop_daemons_in(&rest, me, grace)
}

/// Poll until `pid` is gone, or `grace` runs out. True when it exited.
fn wait_for_exit(pid: u32, grace: Duration) -> bool {
    let deadline = Instant::now() + grace;
    loop {
        if !pid_alive(pid) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
