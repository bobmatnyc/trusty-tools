//! What guided autostart does before it polls `/health`, and what a timeout
//! means afterwards (#9034).
//!
//! Why: "the launchd service is loaded" was read as "the daemon is running".
//! `launchctl print` exits 0 for a loaded job whose `state = not running` — the
//! #4230 state after `tm stop` on a KeepAlive{SuccessfulExit:false} unit — so
//! bare `tm` waited on a daemon that nothing would start. A lock pid was also
//! trusted on `kill(pid, 0)` alone, so a reused pid blocked autostart forever.
//! What: [`prepare_autostart`] probes launchd through an injected `launchctl`
//! runner and the lock through an injected pid-identity check, then waits on a
//! running daemon, asks launchd to start a stopped one (`bootstrap`, then
//! `kickstart`), or spawns. [`timeout_evidence`] and [`autostart_timeout_error`]
//! decide whether a poll timeout is a slow daemon or a down one.
//! Test: `guided_autostart_plan_tests.rs`.

use super::daemon_pid_identity::PidIdentity;
use super::guided_liveness::{DaemonAliveUnresponsive, DaemonLockUnverified};

/// One finished `launchctl` call: whether it exited 0, and its stdout.
pub(crate) struct LaunchctlReply {
    /// `true` when `launchctl` exited 0.
    pub success: bool,
    /// Captured stdout.
    pub stdout: String,
}

/// Runs `launchctl <args>`; `None` when it could not be spawned.
pub(crate) type LaunchctlRunner<'a> = &'a dyn Fn(&[String]) -> Option<LaunchctlReply>;

/// The launchd job autostart may drive: `gui/<uid>/<label>` and its plist.
pub(crate) struct LaunchdTarget {
    /// `gui/<uid>`.
    pub domain: String,
    /// The daemon's launchd label.
    pub label: String,
    /// The installed plist, for `launchctl bootstrap`.
    pub plist: std::path::PathBuf,
}

impl LaunchdTarget {
    fn service(&self) -> String {
        format!("{}/{}", self.domain, self.label)
    }
}

/// launchd's answer about the daemon job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LaunchdJob {
    /// `launchctl print` failed: the job is not loaded in this domain.
    NotLoaded,
    /// Loaded, `state = running`.
    Running,
    /// Loaded, any other state (`not running`, `spawn scheduled`, …).
    Stopped,
}

/// Parse the job's `state = …` line from `launchctl print` output.
///
/// Why: #9034 — exit 0 from `launchctl print` proves the job is loaded, not
/// that it runs. The `state` line is the only field that says which.
/// What: the first line whose trimmed form is `state = <value>`; `running` →
/// [`LaunchdJob::Running`], any other value → [`LaunchdJob::Stopped`]. `None`
/// when no such line exists. Nested `state = active` lines of sub-sections
/// (endpoints, event triggers) are indented deeper and come after the job's
/// own line, so the first match is the job's.
/// Test: `parse_state_running`, `parse_state_not_running`,
/// `parse_state_absent_is_none`.
pub(crate) fn parse_launchd_state(print_output: &str) -> Option<LaunchdJob> {
    print_output.lines().find_map(|line| {
        let value = line.trim().strip_prefix("state = ")?;
        Some(if value.trim() == "running" {
            LaunchdJob::Running
        } else {
            LaunchdJob::Stopped
        })
    })
}

/// Ask launchd for the job's state through `run`.
///
/// What: `launchctl print <domain>/<label>`; a failed or unspawnable call is
/// `NotLoaded`; exit 0 with no `state` line is `Stopped`, so it gets started.
/// Test: `prepare_kickstarts_a_loaded_but_stopped_job` and siblings.
pub(crate) fn probe_job(run: LaunchctlRunner<'_>, target: &LaunchdTarget) -> LaunchdJob {
    match run(&["print".to_string(), target.service()]) {
        Some(reply) if reply.success => {
            parse_launchd_state(&reply.stdout).unwrap_or(LaunchdJob::Stopped)
        }
        _ => LaunchdJob::NotLoaded,
    }
}

/// What autostart does, decided by [`prepare_autostart`].
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum AutostartPlan {
    /// A daemon is running; poll it. The string names the evidence.
    AwaitExisting(String),
    /// launchd was asked to start the job; poll, then re-probe on timeout.
    AwaitLaunchd,
    /// No daemon and launchd cannot help: spawn one.
    Spawn,
    /// A live lock pid is not confirmed a daemon: stop, keep the lock, and
    /// report. The string names the evidence and the recovery.
    Blocked(String),
}

/// The production pid-identity check, injectable for tests.
pub(crate) type IdentifyPid<'a> = &'a dyn Fn(u32) -> PidIdentity;

/// What `daemon.lock` proves about a daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LockVerdict {
    /// No lock of ours.
    Absent,
    /// Ours, and its pid is dead: the ONLY state whose lock may be deleted.
    Stale(u32),
    /// Its pid is alive and is a tm/trusty-mpm daemon.
    LiveDaemon(u32),
    /// Its pid is alive but is not confirmed a daemon (another program, or an
    /// unreadable argv). Never deleted; the operator decides.
    Unverified(u32, PidIdentity),
}

/// Read `daemon.lock` against the process table.
///
/// Why: #9034 — the lock-removal rule. A lock is deleted ONLY when its pid is
/// not alive. A live pid whose identity is a daemon is awaited; a live pid
/// that is not confirmed a daemon — another program on a reused pid, or an
/// argv that cannot be read — is never deleted by `tm`: bare `tm` stops and
/// tells the operator to remove the lock if no tm daemon is running. An
/// identity check that fails must fail closed, because the wrong answer
/// deletes a live daemon's lock and starts a second daemon.
/// What: parses without mutating; dead pid → `Stale`; live → `identify(pid)`:
/// `Daemon` → `LiveDaemon`, otherwise `Unverified`.
/// Test: `lock_verdict_follows_the_removal_rule`,
/// `prepare_never_deletes_an_unverified_lock`.
pub(crate) fn lock_verdict(lock_path: &std::path::Path, identify: IdentifyPid<'_>) -> LockVerdict {
    use trusty_mpm::core::daemon_identity::{parse_lock, pid_alive};
    let Some(lock) = std::fs::read_to_string(lock_path)
        .ok()
        .as_deref()
        .and_then(parse_lock)
    else {
        return LockVerdict::Absent;
    };
    if !pid_alive(lock.pid) {
        return LockVerdict::Stale(lock.pid);
    }
    match identify(lock.pid) {
        PidIdentity::Daemon => LockVerdict::LiveDaemon(lock.pid),
        other => LockVerdict::Unverified(lock.pid, other),
    }
}

/// Operator-facing evidence for a lock that keeps `tm` from starting a
/// daemon, with its recovery; `None` for `Absent` and `Stale`.
///
/// Test: `prepare_awaits_a_live_daemon_lock_pid`,
/// `prepare_never_deletes_an_unverified_lock`.
pub(crate) fn lock_evidence(verdict: LockVerdict) -> Option<String> {
    match verdict {
        LockVerdict::LiveDaemon(pid) => Some(format!(
            "daemon.lock names live daemon pid {pid}; if that process is not serving, \
             stop it or remove ~/.trusty-mpm/daemon.lock, then run `tm start`"
        )),
        LockVerdict::Unverified(pid, identity) => {
            let what = if identity == PidIdentity::NotDaemon {
                "it is another program"
            } else {
                "its identity could not be read"
            };
            Some(format!(
                "daemon.lock names live pid {pid}, not confirmed as a tm daemon ({what}); \
                 remove ~/.trusty-mpm/daemon.lock if no tm daemon is running, then run `tm start`"
            ))
        }
        LockVerdict::Absent | LockVerdict::Stale(_) => None,
    }
}

/// Decide, and perform the launchd side of, the autostart.
///
/// Why: #9034 — see the module doc; every input that touches the host is
/// injected so the decision runs against a fake `launchctl` and temp lock.
/// What: (1) a launchd job reporting `state = running` → `AwaitExisting`;
/// (2) [`lock_verdict`]: a live daemon pid → `AwaitExisting`; an unverified
/// live pid → `Blocked`, lock kept; a stale (dead-pid) lock is removed;
/// (3) with a launchd target, a not-loaded job is bootstrapped and re-probed;
/// running → `AwaitLaunchd`; stopped → `launchctl kickstart`, `AwaitLaunchd`
/// when it succeeds; (4) otherwise `Spawn`.
/// Test: `prepare_awaits_a_running_launchd_job`,
/// `prepare_kickstarts_a_loaded_but_stopped_job`,
/// `prepare_bootstraps_then_kickstarts_an_unloaded_job`,
/// `prepare_spawns_when_launchd_cannot_start_the_job`,
/// `prepare_awaits_a_live_daemon_lock_pid`,
/// `prepare_never_deletes_an_unverified_lock`,
/// `prepare_clears_dead_pid_lock_and_spawns`.
pub(crate) fn prepare_autostart(
    run: LaunchctlRunner<'_>,
    launchd: Option<&LaunchdTarget>,
    lock_path: &std::path::Path,
    identify: IdentifyPid<'_>,
) -> AutostartPlan {
    let mut job = launchd.map(|t| probe_job(run, t));
    if job == Some(LaunchdJob::Running) {
        return AutostartPlan::AwaitExisting(format!(
            "launchd reports {} running",
            launchd.map_or("the service", |t| t.label.as_str())
        ));
    }
    match lock_verdict(lock_path, identify) {
        verdict @ LockVerdict::LiveDaemon(_) => {
            return AutostartPlan::AwaitExisting(lock_evidence(verdict).unwrap_or_default());
        }
        // #9034: a live pid that is not confirmed a daemon fails closed.
        verdict @ LockVerdict::Unverified(..) => {
            return AutostartPlan::Blocked(lock_evidence(verdict).unwrap_or_default());
        }
        // #9034: only a dead pid's lock is deleted; the spawned daemon's
        // duplicate guard would otherwise refuse to start.
        LockVerdict::Stale(pid) => {
            trusty_mpm::core::daemon_identity::remove_lock_owned_by_at(lock_path, &[pid]);
        }
        LockVerdict::Absent => {}
    }
    let Some(target) = launchd else {
        return AutostartPlan::Spawn;
    };
    if job == Some(LaunchdJob::NotLoaded) {
        let plist = target.plist.to_string_lossy().into_owned();
        let _ = run(&["bootstrap".to_string(), target.domain.clone(), plist]);
        job = Some(probe_job(run, target));
    }
    match job {
        Some(LaunchdJob::Running) => AutostartPlan::AwaitLaunchd,
        // #9034: loaded but stopped is down and startable — the #4230 state.
        Some(LaunchdJob::Stopped) => match run(&["kickstart".to_string(), target.service()]) {
            Some(reply) if reply.success => AutostartPlan::AwaitLaunchd,
            _ => AutostartPlan::Spawn,
        },
        _ => AutostartPlan::Spawn,
    }
}

/// Liveness evidence at the end of an unsuccessful `/health` poll.
///
/// Why: #9034 — a timeout after a spawn or a launchd start is only "slow"
/// when something is still running; otherwise the daemon is down.
/// What: `AwaitExisting` → its evidence; `AwaitLaunchd` → re-probe, evidence
/// only when launchd now reports `running`; `Spawn` → evidence only when the
/// spawned child is still running (`spawned_running` holds its pid).
/// Test: `timeout_evidence_*`.
pub(crate) fn timeout_evidence(
    plan: &AutostartPlan,
    run: LaunchctlRunner<'_>,
    launchd: Option<&LaunchdTarget>,
    spawned_running: Option<u32>,
) -> Option<String> {
    match plan {
        AutostartPlan::AwaitExisting(evidence) | AutostartPlan::Blocked(evidence) => {
            Some(evidence.clone())
        }
        AutostartPlan::AwaitLaunchd => launchd
            .filter(|t| probe_job(run, t) == LaunchdJob::Running)
            .map(|t| format!("launchd reports {} running", t.label)),
        AutostartPlan::Spawn => {
            spawned_running.map(|pid| format!("spawned pid {pid} still starting"))
        }
    }
}

/// The pid of a spawned daemon child that has not exited yet.
///
/// Why: #9034 — the spawn path dropped its `Child`, so a 5 s timeout read a
/// daemon that was still starting as down and went offline.
/// What: `try_wait()`; `Some(pid)` while it runs, `None` once it exited or
/// the status cannot be read.
/// Test: `spawned_child_still_running_reports_its_pid`,
/// `spawned_child_that_exited_reports_none`.
pub(crate) fn spawned_still_running(child: &mut std::process::Child) -> Option<u32> {
    matches!(child.try_wait(), Ok(None)).then(|| child.id())
}

/// The error autostart returns, without polling, for a `Blocked` plan.
///
/// Test: `prepare_never_deletes_an_unverified_lock`.
pub(crate) fn blocked_error(evidence: &str) -> anyhow::Error {
    DaemonLockUnverified {
        evidence: evidence.to_string(),
    }
    .into()
}

/// The error a timed-out autostart returns.
///
/// Why: #9034 — the caller stops on [`DaemonAliveUnresponsive`] and goes
/// offline on anything else, so this one constructor decides that split.
/// What: `Some(evidence)` → [`DaemonAliveUnresponsive`]; `None` → a plain
/// "did not become healthy" error.
/// Test: `timeout_error_with_evidence_is_alive_unresponsive`,
/// `timeout_error_without_evidence_is_plain`.
pub(crate) fn autostart_timeout_error(evidence: Option<String>) -> anyhow::Error {
    match evidence {
        Some(evidence) => DaemonAliveUnresponsive { evidence }.into(),
        None => anyhow::anyhow!("daemon did not become healthy within 5 s after auto-start"),
    }
}

#[cfg(test)]
#[path = "guided_autostart_plan_tests.rs"]
mod tests;
