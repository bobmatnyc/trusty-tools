//! Whether a session's bound `claude` still runs, for the reaper (#9010).
//!
//! Why: the reaper removes a settled session only when its bound `claude` is
//! proven gone, so the probe separates proof from an unanswered lookup.
//! What: [`ClaudeLiveness`] and the probes that produce it; the reaper's
//! hold and sweep live in `session_claudes`.
//! Test: `session_claudes_tests.rs` (`_9010` cases).

use crate::core::session::SessionId;
use crate::core::twin_arming::{ProcessFacts, process_facts};
use crate::core::twin_identity::ClaudeProcess;

/// What a probe of a bound `claude` found (#9010).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ClaudeLiveness {
    /// A process with the bound pid AND start time runs.
    Alive,
    /// Proven gone: no process holds the pid, or a later process does.
    Gone(String),
    /// The process table could not answer; nothing may act on it.
    Unknown(String),
}

impl ClaudeLiveness {
    /// Whether this probe of `session`'s bound `claude` proves it gone, and
    /// so lets the reaper remove the session (#9010).
    ///
    /// What: `true` only for [`Self::Gone`], logged at INFO. An
    /// [`Self::Unknown`] is logged at WARN and keeps the session.
    /// Test: `an_unanswered_probe_keeps_a_settled_session_9010`.
    pub(crate) fn proves_gone(&self, session: SessionId) -> bool {
        match self {
            Self::Alive => false,
            Self::Gone(why) => {
                tracing::info!(session = ?session, "a settled session's claude is gone: {why} (#9010)");
                true
            }
            // #9010: fail closed — an unanswered probe reaps nothing.
            Self::Unknown(why) => {
                tracing::warn!(session = ?session, "kept a settled session, its claude's liveness is unknown: {why} (#9010)");
                false
            }
        }
    }
}

/// Whether the bound `claude` still runs, over the live process table (#9010).
///
/// What: [`claude_liveness_with`] over `kill(pid, 0)`
/// (`session_manager::worktree_registry::pid_liveness`), [`process_facts`]
/// and the hook walk's `claude` name rule.
/// Test: `a_settled_session_whose_claude_exited_is_reaped_9010`.
pub(crate) fn claude_liveness(claude: ClaudeProcess) -> ClaudeLiveness {
    let exists = |pid| {
        crate::session_manager::worktree_registry::pid_liveness(pid)
            .ok_or_else(|| format!("kill(0) could not tell whether pid {pid} runs"))
    };
    claude_liveness_with(
        claude,
        exists,
        process_facts,
        crate::core::twin_arming::is_claude,
    )
}

/// [`claude_liveness`] over an injected existence probe, process table and
/// `claude` name check.
///
/// Why: the reaper removes a session on [`ClaudeLiveness::Gone`], so only
/// proof may produce it; an unanswered probe must leave the session alone.
/// What: `Unknown` when `exists` errs, or the pid exists and `facts` cannot
/// read it. `Gone` when no process holds the pid, or the one holding it
/// started at another time AND `is_claude` says it is no `claude` (a reused
/// pid). Any other start-time mismatch is `Unknown`: on Linux the start time
/// is derived from the boot time, which a wall-clock step moves. `Alive`
/// otherwise.
/// Test: `claude_liveness_needs_proof_to_call_a_claude_gone_9010`.
pub(crate) fn claude_liveness_with(
    claude: ClaudeProcess,
    exists: impl Fn(u32) -> Result<bool, String>,
    facts: impl Fn(u32) -> Result<ProcessFacts, String>,
    is_claude: impl Fn(u32) -> Result<bool, String>,
) -> ClaudeLiveness {
    let pid = claude.pid;
    let started = match exists(pid) {
        Err(e) => return ClaudeLiveness::Unknown(e),
        Ok(false) => return ClaudeLiveness::Gone(format!("no process holds pid {pid}")),
        Ok(true) => match facts(pid) {
            Ok(f) if f.start_time == claude.start_time => return ClaudeLiveness::Alive,
            Ok(f) => f.start_time,
            Err(e) => return ClaudeLiveness::Unknown(format!("pid {pid} could not be read: {e}")),
        },
    };
    let shifted = format!(
        "pid {pid} names a process started at {started}, not {}",
        claude.start_time
    );
    // #9010: a mismatch proves a reused pid only when its holder is no claude.
    match is_claude(pid) {
        Ok(false) => ClaudeLiveness::Gone(format!("{shifted}, and it is not a claude")),
        Ok(true) => ClaudeLiveness::Unknown(format!("{shifted}, but it is a claude")),
        Err(e) => ClaudeLiveness::Unknown(format!("{shifted}, and it could not be named: {e}")),
    }
}
