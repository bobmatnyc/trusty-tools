//! Which `claude` process registered each session id, from the kernel (#8531).
//!
//! Why: a delegation repair is granted to the session that owns the record,
//! and "owns" must be a fact about a process, not a name or a field a caller
//! can write. A session record's `pid` is PATCHable (`mpm.sessions.set_pid`)
//! and its `tmux_name` can be squatted with `tmux new-session`; the record
//! itself is dropped by the reaper and can then be re-created by anyone who
//! names the id. This map is none of those things.
//! What: [`SessionClaudes`] holds, per session id, the `claude` process
//! (pid + start time) that announced the session in a `SessionStart` hook
//! over the daemon socket — the kernel-reported peer, walked up to its
//! nearest `claude`. First writer wins for the daemon's lifetime; no route
//! writes or clears an entry, and the reaper never touches it.
//! Test: `session_claudes_tests.rs`.

use dashmap::DashMap;
use dashmap::mapref::entry::Entry;

use super::DaemonState;
use crate::core::session::SessionId;
use crate::core::twin_arming::{
    ProcessFacts, STATUS_MAX_ANCESTOR_HOPS, nearest_claude_for_status_in, process_facts,
};
use crate::core::twin_identity::ClaudeProcess;

/// The kernel-bound `claude` of every session announced over the socket.
///
/// Why: see the module doc.
/// What: session id → the announcing `claude`'s pid and start time.
/// Test: `a_session_is_bound_once_first_writer_wins_8531`.
#[derive(Debug, Default)]
pub struct SessionClaudes {
    bound: DashMap<SessionId, ClaudeProcess>,
}

impl SessionClaudes {
    /// The `claude` bound to `session`, if one announced it over the socket.
    pub fn get(&self, session: SessionId) -> Option<ClaudeProcess> {
        self.bound.get(&session).map(|e| *e.value())
    }

    /// The nearest `claude` above the kernel-reported peer `pid` (#8531).
    ///
    /// Why: both the binding and the repair check start from a pid the kernel
    /// reported when the request arrived, and the walk runs later; by then
    /// the peer may have exited and its pid been reused.
    /// What: [`peer_claude_with`] over the live process table and the same
    /// `claude` name rule the hook's walk uses.
    /// Test: `peer_claude_with`'s cases.
    pub fn peer_claude(&self, pid: u32, seen_at: u64) -> Result<ClaudeProcess, String> {
        peer_claude_with(
            pid,
            seen_at,
            process_facts,
            crate::core::twin_arming::is_claude,
        )
    }
}

/// [`SessionClaudes::peer_claude`] over an injected process table (#8531).
///
/// What: `Err` when the peer's process cannot be read, when it started after
/// `seen_at` (a reused pid), when the ancestry cannot be read, or when no
/// `claude` runs within [`STATUS_MAX_ANCESTOR_HOPS`] of it. Otherwise that
/// `claude`, with its start time.
/// Test: `a_peer_started_after_the_request_is_refused_8531`,
/// `an_unreadable_peer_is_refused_8531`,
/// `a_peer_with_no_claude_above_it_is_refused_8531`,
/// `the_peer_claude_is_the_nearest_claude_ancestor_8531`.
pub(crate) fn peer_claude_with(
    pid: u32,
    seen_at: u64,
    facts: impl Fn(u32) -> Result<ProcessFacts, String>,
    is_claude: impl Fn(u32) -> Result<bool, String>,
) -> Result<ClaudeProcess, String> {
    // #8531: a peer that started after the request arrived holds a reused pid.
    let started = facts(pid)
        .map_err(|e| format!("the calling process (pid {pid}) could not be read: {e}"))?
        .start_time;
    if started > seen_at {
        return Err(format!(
            "pid {pid} now names a process started after the request arrived"
        ));
    }
    match nearest_claude_for_status_in(pid, facts, is_claude) {
        Ok(Some(claude)) => Ok(claude),
        Ok(None) => Err(format!(
            "no claude session process runs within {STATUS_MAX_ANCESTOR_HOPS} parent hops \
             of the calling process (pid {pid})"
        )),
        Err(e) => Err(format!(
            "the calling process's ancestry (pid {pid}) could not be read: {e}"
        )),
    }
}

impl DaemonState {
    /// The kernel-bound `claude` registry (#8531).
    pub fn session_claudes(&self) -> &SessionClaudes {
        &self.session_claudes
    }

    /// Bind `session` to the `claude` that announced it (#8531).
    ///
    /// Why: the binding is what a later repair compares the caller against,
    /// so it can be written once only, and never for an id whose records
    /// predate it — an id that already owns delegations was announced to the
    /// daemon by someone this call cannot identify.
    /// What: `Ok` when the entry was vacant and no delegation names
    /// `session`; `Err` naming why otherwise. Never overwrites.
    /// Test: `a_session_is_bound_once_first_writer_wins_8531`,
    /// `a_session_that_already_owns_records_is_not_bound_8531`.
    pub fn bind_session_claude(
        &self,
        session: SessionId,
        claude: ClaudeProcess,
    ) -> Result<(), String> {
        match self.session_claudes.bound.entry(session) {
            Entry::Occupied(_) => Err("the session is already bound to its claude".to_string()),
            Entry::Vacant(slot) => {
                if self.delegations.iter().any(|d| d.session == session) {
                    return Err(
                        "the session already owns delegation records, so the process that \
                         announced it first is unknown"
                            .to_string(),
                    );
                }
                slot.insert(claude);
                Ok(())
            }
        }
    }
}

#[cfg(test)]
#[path = "session_claudes_tests.rs"]
mod session_claudes_tests;
