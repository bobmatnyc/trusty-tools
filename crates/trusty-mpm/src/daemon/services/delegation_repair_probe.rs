//! Does the OS say a delegation's agent is still running? (#8257)
//!
//! Why: `tm repair delegation` ends a record on the daemon's own bookkeeping —
//! the owner session's liveness, a stop matched by agent type, the record's age.
//! Bookkeeping is exactly what went wrong when a record is stuck, so before the
//! repair writes it asks the machine. A repair that trusted the registry alone
//! would release a tree a live agent still holds, which is the ADR-0048 harm.
//! What: [`probe_live_evidence`] checks the record's OWN tree two ways — a
//! process whose cwd is inside it (`worktree_liveness::process_holding`), and a
//! Claude Code harness lock whose pid is running with the start time the lock
//! recorded. [`lock_holder_evidence`] is the pure half of the second.
//!
//! # Fail direction
//!
//! Closed. Every step that cannot answer — `lsof` missing or blind, git unable
//! to list the worktrees of a tree that has a `.git` at or above it, a lock pid
//! whose liveness or start time cannot be read — is
//! [`LiveEvidence::Undeterminable`], and the repair refuses on it with the
//! step's own words (ADR-0045). A tree with no `.git` anywhere above it holds
//! no harness lock, so git failing there is not an unanswered step.
//!
//! # Stated gap: a record with no tree of its own
//!
//! An unisolated subagent runs inside its dispatching session's `claude`
//! process, in the shared checkout. Every process standing there — that PM, the
//! operator's shell, the `tm` that asked for the repair — would match a cwd
//! probe, so the probe carries no information about this agent and is not run.
//! For that record the live-pid evidence is the owning session's own tracked
//! pid, which `delegation_repair::owner_liveness` reads before this module is
//! reached, plus any held harness lock that names the agent.
//! Test: `delegation_repair_tests`.

use std::path::Path;

use crate::core::agent::Delegation;
use crate::session_manager::worktree_registry::{
    RegisteredWorktree, harness_lock_pid_in_reason, is_harness_agent_lock_reason,
    list_registered_worktrees, pid_liveness,
};

/// What the OS says about one record's agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LiveEvidence {
    /// Every probe ran and none found the agent.
    Clear,
    /// The tree could not be shown free: a live process holds it, or the cwd
    /// probe could not complete. The text says which (#8257 critic R6).
    Held(String),
    /// A probe could not answer; the text names the step.
    Undeterminable(String),
}

/// How far a lock's recorded start may sit from the process's own (#8257).
///
/// Why: the lock's `start` is a `ctime` string at one-second resolution, and
/// the kernel's start time is truncated to the second as well, so an exact
/// equality would refuse on rounding alone.
const START_TOLERANCE_SECS: i64 = 2;

/// Probe the record's own tree for a live agent (#8257).
///
/// Why: see the module doc — the repair's last gate before it writes.
/// What: `Clear` when the record has no tree of its own (see the stated gap),
/// when that tree no longer exists, or when both probes ran and found nothing;
/// `Held` for a process standing in the tree or a running lock pid whose start
/// time matches; `Undeterminable` for any probe step that could not answer.
/// Test: `repair_refuses_while_a_live_process_holds_the_tree_8257`,
/// `repair_refuses_when_the_live_agent_probe_cannot_answer_8257`.
pub(crate) fn probe_live_evidence(d: &Delegation) -> LiveEvidence {
    probe_live_evidence_with(
        d,
        crate::session_manager::worktree_liveness::process_holding,
    )
}

/// [`probe_live_evidence`] with the cwd probe injected (#8257 critic R6).
///
/// Why: the probe-failure arm needs a test, and a missing `lsof` cannot be
/// staged on a machine that has one.
/// Test: `a_cwd_probe_that_cannot_run_refuses_without_claiming_a_holder_8257`.
pub(crate) fn probe_live_evidence_with(
    d: &Delegation,
    process_holding: impl Fn(&Path) -> Option<String>,
) -> LiveEvidence {
    let Some(tree) = own_tree(d) else {
        // #8257 owner ruling: no cwd probe here (the stated gap), but a held
        // harness lock naming this agent anywhere in the repo still refuses.
        return agent_lock_evidence(d);
    };
    match std::fs::symlink_metadata(tree) {
        // A tree that is gone has nobody standing in it.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return LiveEvidence::Clear,
        Err(e) => {
            return LiveEvidence::Undeterminable(format!(
                "could not stat the agent's tree {}: {e}",
                tree.display()
            ));
        }
        Ok(_) => {}
    }
    // `process_holding` folds "found one" and "could not look" into one
    // `Some(reason)`; both refuse, and its reason says which it was.
    if let Some(reason) = process_holding(tree) {
        return LiveEvidence::Held(reason);
    }
    let canonical = match std::fs::canonicalize(tree) {
        Ok(p) => p,
        Err(e) => {
            return LiveEvidence::Undeterminable(format!(
                "could not canonicalize {}: {e}",
                tree.display()
            ));
        }
    };
    as_evidence(harness_lock_in(tree, |w| {
        std::fs::canonicalize(&w.path).unwrap_or_else(|_| w.path.clone()) == canonical
    }))
}

/// A held harness lock naming `d`'s agent, for a record with no tree of its
/// own (#8257).
///
/// What: `Clear` without an agent id or a `cwd`, when `cwd` no longer exists,
/// or when `cwd` sits outside any repository; otherwise the lock evidence over
/// every worktree `cwd`'s repository registers whose harness lock names
/// `agent-<agent_id>`.
/// Test: `the_owner_is_refused_while_a_harness_lock_names_its_agent_8257`,
/// `a_record_whose_cwd_is_gone_is_repairable_8257`.
fn agent_lock_evidence(d: &Delegation) -> LiveEvidence {
    let (Some(agent_id), Some(cwd)) = (d.agent_id.as_deref(), d.cwd.as_deref()) else {
        return LiveEvidence::Clear;
    };
    if let Err(e) = std::fs::symlink_metadata(cwd) {
        return match e.kind() {
            std::io::ErrorKind::NotFound => LiveEvidence::Clear,
            _ => LiveEvidence::Undeterminable(format!("could not stat {}: {e}", cwd.display())),
        };
    }
    let token = format!("agent-{agent_id}");
    as_evidence(harness_lock_in(cwd, |w| {
        w.lock_reason
            .as_deref()
            .is_some_and(|r| r.split_whitespace().any(|t| t == token))
    }))
}

/// Fold a lock read into the evidence the repair acts on.
fn as_evidence(read: Result<Option<String>, String>) -> LiveEvidence {
    match read {
        Ok(None) => LiveEvidence::Clear,
        Ok(Some(reason)) => LiveEvidence::Held(reason),
        Err(reason) => LiveEvidence::Undeterminable(reason),
    }
}

/// The tree the agent reported as its own, when it differs from where it was
/// dispatched from.
fn own_tree(d: &Delegation) -> Option<&Path> {
    let tree = d.worktree_path.as_deref()?;
    (d.cwd.as_deref() != Some(tree)).then_some(tree)
}

/// Read the harness lock of the first worktree `anchor`'s repository registers
/// that `pick` selects, if any.
///
/// What: `Ok(None)` when git lists the worktrees and the picked one is absent,
/// unlocked, or locked by an operator rather than the harness, or when git
/// cannot list them and [`outside_any_repository`] finds no `.git` at or above
/// `anchor`. `Err` for every other git failure.
/// Test: `a_record_whose_cwd_is_outside_any_repository_is_repairable_8257`,
/// `a_record_whose_repository_git_cannot_read_still_refuses_8257`.
fn harness_lock_in(
    anchor: &Path,
    pick: impl Fn(&RegisteredWorktree) -> bool,
) -> Result<Option<String>, String> {
    let Some(registered) = list_registered_worktrees(anchor) else {
        // #8257 critic R2: no repository, no harness lock — nothing to read.
        if outside_any_repository(anchor)? {
            return Ok(None);
        }
        return Err(format!(
            "git could not list the worktrees registered for {}, so no harness lock was read",
            anchor.display()
        ));
    };
    let reason = registered
        .into_iter()
        .filter(|w| w.locked && pick(w))
        .find_map(|w| w.lock_reason.filter(|r| is_harness_agent_lock_reason(r)));
    match reason {
        Some(reason) => lock_holder_evidence(&reason, pid_liveness, process_start_epoch),
        None => Ok(None),
    }
}

/// Is there positively no git repository at or above `anchor`? (#8257)
///
/// Why: git's exit 128 "not a git repository" is also what it prints for a
/// real repository whose `.git` it cannot read, so git's answer alone cannot
/// tell "no repository" from "a repository git failed on". A `.git` entry is
/// what git discovery looks for, and its absence on every ancestor settles it.
/// What: `Ok(true)` when neither the canonical `anchor` nor any ancestor has a
/// `.git` entry (directory or gitfile); `Ok(false)` when one does; `Err` when
/// `anchor` cannot be canonicalized or an ancestor's `.git` cannot be stat'd
/// for any reason other than its absence.
fn outside_any_repository(anchor: &Path) -> Result<bool, String> {
    let canonical = std::fs::canonicalize(anchor)
        .map_err(|e| format!("could not canonicalize {}: {e}", anchor.display()))?;
    for dir in canonical.ancestors() {
        let dot_git = dir.join(".git");
        match std::fs::symlink_metadata(&dot_git) {
            Ok(_) => return Ok(false),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("could not stat {}: {e}", dot_git.display())),
        }
    }
    Ok(true)
}

/// Is the pid a harness lock names still the process that wrote it? (#8257)
///
/// Why: a lock pid alone is not evidence — pids are reused, and a running
/// process that started after the lock was written is someone else. The lock
/// records its holder's start time, so the match is checked, not assumed.
/// What: [`lock_holder_evidence_in`] with this machine's local zone.
/// Test: `lock_holder_evidence_reads_each_arm_8257`.
pub(crate) fn lock_holder_evidence(
    reason: &str,
    pid_alive: impl Fn(u32) -> Option<bool>,
    start_of: impl Fn(u32) -> Option<i64>,
) -> Result<Option<String>, String> {
    lock_holder_evidence_in(reason, pid_alive, start_of, &chrono::Local)
}

/// [`lock_holder_evidence`] with the machine's zone injected (#8257).
///
/// Why: the harness writes the lock's `start <ctime>` in UTC (measured: lock
/// `10:45:39`, `ps -o lstart` `06:45:39` EDT). Read as local time, every live
/// lock on a non-UTC machine looked like a reused pid, so the repair cleared a
/// record a running agent held. The zone the stamp is written in is not
/// recorded, so a mismatch shaped like a zone offset is ambiguous, and an
/// ambiguous reading must never read as dead.
/// What: `Ok(None)` when the pid is gone, or runs with a start that matches no
/// reading of the stamp and sits no zone offset away from its UTC reading (a
/// reused pid). `Ok(Some(reason))` when it runs and its start matches the stamp
/// read as UTC or in `local`, within [`START_TOLERANCE_SECS`]. `Err` when the
/// reason names no pid or no start time, when either probe cannot answer, or
/// when the start sits a whole zone offset from the UTC reading.
/// Test: `a_live_lock_reads_live_in_any_time_zone_8257`,
/// `lock_holder_evidence_reads_each_arm_8257`.
pub(crate) fn lock_holder_evidence_in<Tz: chrono::TimeZone>(
    reason: &str,
    pid_alive: impl Fn(u32) -> Option<bool>,
    start_of: impl Fn(u32) -> Option<i64>,
    local: &Tz,
) -> Result<Option<String>, String> {
    let pid = harness_lock_pid_in_reason(reason)
        .ok_or_else(|| format!("the harness lock `{reason}` names no pid"))?;
    match pid_alive(pid) {
        Some(false) => return Ok(None),
        None => {
            return Err(format!(
                "could not tell whether harness lock pid {pid} runs"
            ));
        }
        Some(true) => {}
    }
    let stamp = lock_start_stamp(reason).ok_or_else(|| {
        format!("harness lock pid {pid} runs, but the lock carries no readable start time")
    })?;
    let actual = start_of(pid)
        .ok_or_else(|| format!("could not read the start time of running pid {pid}"))?;
    let as_utc = stamp.and_utc().timestamp();
    let as_local = local.from_local_datetime(&stamp);
    let near = |at: i64| (actual - at).abs() <= START_TOLERANCE_SECS;
    let matches = near(as_utc)
        || [as_local.clone().earliest(), as_local.latest()]
            .into_iter()
            .flatten()
            .any(|t| near(t.timestamp()));
    if matches {
        return Ok(Some(format!(
            "harness lock pid {pid} is running and is the process that took the lock"
        )));
    }
    let skew = actual - as_utc;
    if is_zone_offset(skew) {
        return Err(format!(
            "harness lock pid {pid} runs, and its start time sits {skew} s from the lock's — a \
             whole time-zone offset, so a reused pid cannot be told from the lock's own holder \
             read in another zone"
        ));
    }
    Ok(None)
}

/// Is `skew` seconds one whole time-zone offset, within the start tolerance?
///
/// What: offsets run from UTC-12 to UTC+14 in quarter-hour steps.
fn is_zone_offset(skew: i64) -> bool {
    const QUARTER_HOUR: i64 = 15 * 60;
    let off_grid = skew.rem_euclid(QUARTER_HOUR);
    skew.abs() <= 14 * 3600 + START_TOLERANCE_SECS
        && off_grid.min(QUARTER_HOUR - off_grid) <= START_TOLERANCE_SECS
}

/// A lock reason's `start <ctime>`, zone-less; `None` when missing or unparsable.
fn lock_start_stamp(reason: &str) -> Option<chrono::NaiveDateTime> {
    let rest = reason.split_once(" start ")?.1;
    let stamp = rest.split(')').next()?;
    let stamp = stamp.split_whitespace().collect::<Vec<_>>().join(" ");
    chrono::NaiveDateTime::parse_from_str(&stamp, "%a %b %d %H:%M:%S %Y").ok()
}

/// The kernel's start time of `pid`, in epoch seconds.
fn process_start_epoch(pid: u32) -> Option<i64> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
    let pid = Pid::from_u32(pid);
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing(),
    );
    sys.process(pid)
        .and_then(|p| i64::try_from(p.start_time()).ok())
}
