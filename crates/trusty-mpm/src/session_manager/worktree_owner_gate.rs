//! The session-safe reclaim rule for a harness agent-store tree (#7771, #8301).
//!
//! Why: a tree under `.claude/worktrees/` was refused forever when it carried
//! no owner file, which is every tree made by hand with `git worktree add` —
//! and `tm pr merge` removed ANOTHER session's idle, resumable agent tree with
//! no ownership check at all. Both paths now ask one question, answered here:
//! is any live party still entitled to this tree?
//! What: [`owner_refusal`] applies conditions (a)–(d) of the #7771 rule. (a) no
//! harness lock names a running pid whose start time matches the lock; (b) no
//! non-terminal delegation names the owner file's agent; (c) no process stands
//! in the tree; (d) the owner file's session is the caller or provably ended.
//! An owner file that names nobody makes (b) and (d) vacuous.
//!
//! # Fail direction
//!
//! Every probe that cannot answer keeps the tree with a named reason
//! (ADR-0045): an unparsable lock reason, a process table that cannot be read,
//! an owner file that cannot be read or parsed (#8511), a cwd probe that fails,
//! and a session nothing proves ended.
//! Test: `worktree_owner_gate_tests`.

use std::path::Path;

use super::worktree_ownership::{
    AgentDelegationState, AgentWorktreeOwner, OwnerReadError, SentinelOwner,
};
use super::worktree_registry::{
    harness_lock_pid_in_reason, is_harness_agent_lock_reason, list_registered_worktrees,
    pid_liveness,
};

/// How far a lock's recorded start may sit from the process's own start.
///
/// Why: both sides are whole seconds, taken by different clocks at different
/// moments, so exact equality would call a live holder "reused".
const START_TOLERANCE_SECS: i64 = 2;

/// What git's lock on a tree says about who holds it (#7771 condition a).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LockLiveness {
    /// Git lists the tree and reports no lock.
    Unlocked,
    /// A harness lock whose pid is gone or reused — released, carrying why.
    Stale(String),
    /// A harness lock whose pid runs with the recorded start time.
    Held(String),
    /// An operator lock, or a lock nothing could judge — keep, carrying why.
    Refused(String),
}

impl LockLiveness {
    /// The refusal this answer carries, or `None` for a released tree.
    pub(crate) fn refusal(&self) -> Option<String> {
        match self {
            Self::Unlocked | Self::Stale(_) => None,
            Self::Held(why) | Self::Refused(why) => Some(why.clone()),
        }
    }
}

/// The UTC start time a harness lock reason records, as Unix seconds (#7771).
///
/// Why: a pid alone cannot tell the holder from a later process that reused
/// its number; the start time the harness writes beside it can.
/// What: the text between `start ` and the closing `)`, read as
/// `Thu Sep 24 02:08:36 2026` in UTC (whitespace-normalised, so a space-padded
/// day parses too). `None` when the marker is absent or does not parse.
/// Test: `lock_start_reads_the_measured_reason_shape`,
/// `lock_start_is_none_for_a_reason_without_one`.
pub(crate) fn parse_lock_start(reason: &str) -> Option<i64> {
    let rest = reason.split_once(" start ")?.1;
    let stamp = rest.split_once(')').map_or(rest, |(s, _)| s);
    // The weekday is dropped: it restates the date, and a writer that got it
    // wrong must not turn a readable start into "no start".
    let normal = stamp
        .split_whitespace()
        .skip(1)
        .collect::<Vec<_>>()
        .join(" ");
    chrono::NaiveDateTime::parse_from_str(&normal, "%b %d %H:%M:%S %Y")
        .ok()
        .map(|t| t.and_utc().timestamp())
}

/// Judge one lock reason against the process table (#7771 condition a).
///
/// Why: pure over its two probes, so every failure arm is testable — no live
/// process can be made to fail a table read on demand.
/// What: an operator-shaped reason refuses. A harness reason with no pid
/// refuses. `pid_alive` answering `Some(false)` is stale; `None` refuses. A
/// running pid is held only when `start_of` matches the recorded start within
/// [`START_TOLERANCE_SECS`]; a mismatch is stale (reused), and an unreadable
/// start on either side refuses.
/// Test: `judge_lock_releases_a_dead_pid`, `judge_lock_releases_a_reused_pid`,
/// `judge_lock_holds_a_live_matching_pid`, `judge_lock_refuses_every_probe_failure`.
pub(crate) fn judge_lock(
    reason: &str,
    pid_alive: &dyn Fn(u32) -> Option<bool>,
    start_of: &dyn Fn(u32) -> Result<i64, String>,
) -> LockLiveness {
    if !is_harness_agent_lock_reason(reason) {
        return LockLiveness::Refused(format!(
            "git-locked by the operator (`{reason}`) — a standing veto no reclaim overrides"
        ));
    }
    let Some(pid) = harness_lock_pid_in_reason(reason) else {
        return LockLiveness::Refused(format!(
            "the harness lock `{reason}` names no pid this can read, so its holder cannot be \
             judged (#7771, ADR-0045)"
        ));
    };
    match pid_alive(pid) {
        Some(false) => return LockLiveness::Stale(format!("lock pid {pid} is gone")),
        None => {
            return LockLiveness::Refused(format!(
                "whether lock pid {pid} runs could not be read from the process table \
                 (#7771, ADR-0045)"
            ));
        }
        Some(true) => {}
    }
    let Some(recorded) = parse_lock_start(reason) else {
        return LockLiveness::Refused(format!(
            "lock pid {pid} runs and the lock records no readable start time, so a reused pid \
             cannot be told from the holder (#7771, ADR-0045)"
        ));
    };
    match start_of(pid) {
        Ok(actual) if (actual - recorded).abs() <= START_TOLERANCE_SECS => {
            LockLiveness::Held(format!(
                "git still reports the harness's agent-lifetime lock (`{reason}`), and pid {pid} \
                 runs with the start time it recorded (#6561, #7771)"
            ))
        }
        Ok(actual) => LockLiveness::Stale(format!(
            "lock pid {pid} was reused (lock start {recorded}, process start {actual})"
        )),
        Err(e) => LockLiveness::Refused(format!(
            "lock pid {pid} runs and its start time could not be read: {e} (#7771, ADR-0045)"
        )),
    }
}

/// `secs` in the harness lock's own start format, UTC (#7771).
///
/// Test: `lock_start_round_trips_its_own_format`.
#[cfg(test)]
pub(crate) fn format_lock_start(secs: i64) -> String {
    chrono::DateTime::from_timestamp(secs, 0)
        .map(|t| t.format("%a %b %e %H:%M:%S %Y").to_string())
        .unwrap_or_default()
}

/// The start time of `pid`, as Unix seconds, from the process table.
pub(crate) fn process_start_secs(pid: u32) -> Result<i64, String> {
    use sysinfo::{Pid, ProcessesToUpdate, System};
    let pid = Pid::from_u32(pid);
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
    let start = sys
        .process(pid)
        .ok_or_else(|| "the process table holds no entry for it".to_string())?
        .start_time();
    i64::try_from(start).map_err(|e| format!("start time out of range: {e}"))
}

/// Ask git, then the process table, who holds `path` (#7771 condition a).
///
/// Test: `lock_liveness_of_an_unlocked_tree_is_unlocked`,
/// `lock_liveness_releases_a_real_lock_whose_pid_is_gone`,
/// `lock_liveness_holds_a_real_lock_naming_this_process`,
/// `lock_liveness_refuses_a_path_git_cannot_place`.
pub(crate) fn lock_liveness(path: &Path) -> LockLiveness {
    let Some(registered) = list_registered_worktrees(path) else {
        return LockLiveness::Refused(
            "git could not be asked whether this tree is locked (#7771, ADR-0045)".into(),
        );
    };
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let found = registered
        .into_iter()
        .find(|w| std::fs::canonicalize(&w.path).unwrap_or_else(|_| w.path.clone()) == canonical);
    let Some(record) = found else {
        return LockLiveness::Refused("git does not list this path as a worktree".into());
    };
    if !record.locked {
        return LockLiveness::Unlocked;
    }
    let reason = record.lock_reason.unwrap_or_default();
    judge_lock(&reason, &pid_liveness, &process_start_secs)
}

/// Unlock `path` if, judged right now, its lock is a stale harness lock
/// (#7771).
///
/// Why: `git worktree remove` refuses any locked tree, so a lock whose holder
/// is gone must be released before a removal the gates already approved.
/// What: re-judges the lock; unlocked is a no-op, stale runs
/// `git worktree unlock`, and every other answer refuses.
/// Test: `release_stale_lock_unlocks_a_dead_pids_lock`,
/// `release_stale_lock_refuses_a_live_holder`.
pub(crate) fn release_stale_lock(path: &Path) -> Result<(), String> {
    match lock_liveness(path) {
        LockLiveness::Unlocked => Ok(()),
        LockLiveness::Stale(why) => {
            let out = super::worktree_safety::git_command(path, &["worktree", "unlock", "."])
                .output()
                .map_err(|e| format!("`git worktree unlock` could not run ({why}): {e}"))?;
            if out.status.success() {
                tracing::info!(path = %path.display(), "released a stale harness lock — {why} (#7771)");
                return Ok(());
            }
            Err(format!(
                "`git worktree unlock` failed ({why}): {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
        LockLiveness::Held(why) | LockLiveness::Refused(why) => Err(why),
    }
}

/// Whether the session an owner file names may lose its tree (#7771 d).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SessionEnd {
    /// It is the session running the reclaim.
    Caller,
    /// The #7652 evidence proves it ended.
    Ended,
    /// It is live, or its liveness probe errored.
    Live,
    /// Nothing can show it ended — carrying why.
    Undeterminable(String),
}

/// The owner file, through the strict `worktree_ownership` reader (#7771,
/// #8511).
///
/// Why: a hand-made tree has no owner file and may be reclaimed, but an owner
/// file that exists and cannot be read may be a live party's truncated claim,
/// so the two must not fold together. Only `worktree_ownership` knows where the
/// marker lives (admin dir or legacy in-tree), so nothing here builds its path.
/// What: `Ok(None)` for no owner file, or one that names nobody (an empty
/// pre-#3649 marker): (b) and (d) are vacuous and (a), (c) and the later gates
/// decide. `Ok(Some(owner))` for an agent or session owner. `Err(why)` when the
/// file cannot be read, does not parse, or the tree's `.git` entry does not
/// resolve — the caller keeps the tree (ADR-0045).
/// Test: `owner_refusal_reclaims_a_tree_with_no_owner_file`,
/// `owner_refusal_honours_a_marker_only_in_the_admin_dir`,
/// `owner_refusal_honours_a_pre_migration_in_tree_marker`,
/// `owner_refusal_keeps_a_tree_whose_git_entry_does_not_resolve`,
/// `classify_blocks_an_agent_store_worktree_with_an_unreadable_sentinel`.
pub(crate) fn read_owner(path: &Path) -> Result<Option<SentinelOwner>, String> {
    match super::worktree_ownership::read_sentinel_owner_strict(path) {
        Ok(None | Some(SentinelOwner::Unknown)) => Ok(None),
        Ok(owner) => Ok(owner),
        Err(OwnerReadError::Unreadable { path, reason }) => Err(format!(
            "its owner file {} cannot be read ({reason}), so who owns the tree cannot be \
             determined (#7771, #8511, ADR-0045)",
            path.display()
        )),
        Err(OwnerReadError::Corrupt { path, reason }) => Err(format!(
            "its owner file {} does not parse ({reason}), so who owns the tree cannot be \
             determined (#7771, #8511, ADR-0045)",
            path.display()
        )),
    }
}

/// The inputs [`owner_refusal`] judges, each injectable for its tests.
pub(crate) struct OwnerGate<'a> {
    /// (b) The delegation registry's answer for the owner file's agent.
    pub agent_state: &'a dyn Fn(&AgentWorktreeOwner) -> AgentDelegationState,
    /// (d) Whether a named session is the caller or provably ended.
    pub session_end: &'a dyn Fn(&str) -> SessionEnd,
    /// (a) Git's lock, judged against the process table.
    pub lock: &'a dyn Fn(&Path) -> LockLiveness,
    /// (c) A process standing in the tree, or why that could not be asked.
    pub cwd_holder: &'a dyn Fn(&Path) -> Option<String>,
}

impl OwnerGate<'_> {
    /// The production probes, around the caller's (b) and (d) answers.
    pub(crate) fn host<'a>(
        agent_state: &'a dyn Fn(&AgentWorktreeOwner) -> AgentDelegationState,
        session_end: &'a dyn Fn(&str) -> SessionEnd,
    ) -> OwnerGate<'a> {
        OwnerGate {
            agent_state,
            session_end,
            lock: &lock_liveness,
            cwd_holder: &super::worktree_liveness::process_holding,
        }
    }
}

/// Why no reclaim may take `path`, or `None` when (a)–(d) all hold (#7771).
///
/// Why: see the module doc — the one rule the prune survey, its pre-delete
/// re-check and `tm pr cleanup` all apply.
/// What: (a) the lock, then the owner file (one naming nobody skips (b) and
/// (d); one that cannot be read keeps the tree — [`read_owner`]), then (b) and
/// (d) against an agent or session owner, then (c) last,
/// because it is the one probe that costs a process-table walk.
/// Test: `owner_refusal_reclaims_a_tree_with_no_owner_file`,
/// `owner_refusal_keeps_a_tree_a_live_pid_locks`,
/// `owner_refusal_keeps_a_live_delegations_tree`,
/// `owner_refusal_keeps_another_live_sessions_tree`,
/// `owner_refusal_permits_the_callers_own_agent_tree`,
/// `owner_refusal_permits_an_ended_sessions_agent_tree`,
/// `owner_refusal_keeps_a_tree_a_process_stands_in`,
/// `owner_refusal_keeps_a_session_nothing_proves_ended`.
pub(crate) fn owner_refusal(path: &Path, gate: &OwnerGate<'_>) -> Option<String> {
    if let Some(why) = (gate.lock)(path).refusal() {
        return Some(why);
    }
    let session = match read_owner(path) {
        Err(why) => return Some(why),
        Ok(None | Some(SentinelOwner::Unknown)) => None,
        Ok(Some(SentinelOwner::Agent(owner, _))) => {
            if (gate.agent_state)(&owner) == AgentDelegationState::Live {
                return Some(format!(
                    "owned by dispatched agent {} — a delegation naming it has not ended \
                     (#5661, #7771)",
                    owner.agent_id
                ));
            }
            Some(owner.parent_session_id.0.to_string())
        }
        Ok(Some(SentinelOwner::Known(owner, _))) => Some(owner.to_string()),
    };
    if let Some(session) = session {
        match (gate.session_end)(&session) {
            SessionEnd::Caller | SessionEnd::Ended => {}
            SessionEnd::Live => {
                return Some(format!(
                    "its owner file names session {session}, which is live and is not the \
                     calling session — that session may still resume work here (#7771)"
                ));
            }
            SessionEnd::Undeterminable(why) => {
                return Some(format!(
                    "its owner file names session {session}, and nothing proves that session \
                     ended: {why} (#7771, ADR-0045)"
                ));
            }
        }
    }
    (gate.cwd_holder)(path)
}

#[cfg(test)]
#[path = "worktree_owner_gate_tests.rs"]
mod worktree_owner_gate_tests;
