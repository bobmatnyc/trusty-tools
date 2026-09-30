//! Who asked for a delegation repair, as the kernel reports it (#8531).
//!
//! Why: the owning session may clear its own live record (#8257 owner ruling).
//! Until #8531 the daemon took "I am the owner" from the `x-tm-caller-session`
//! header, which the CLI filled from `CLAUDE_CODE_SESSION_ID` — any local
//! process could set both, so a denied sibling could impersonate the owner and
//! put a second writer onto a shared HEAD (ADR-0048). Owner ruling 2026-09-28
//! (Option 2): the caller is established only over the daemon socket, from the
//! kernel-verified peer pid plus a parent-process walk. Nothing the caller
//! writes is ever a fallback.
//! What: [`establish_caller`] turns the socket's [`RepairPeer`] into a
//! [`RepairCaller`]. The caller's session is the nearest `claude` above the
//! peer process; it is the owner only when that `claude` IS the owning
//! session's process. Every lookup that cannot answer yields
//! [`RepairCaller::Unestablished`], which the repair gate refuses.
//! Test: `delegation_repair_caller_tests.rs`.

use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use super::delegation_repair::RepairCaller;
use crate::core::agent::Delegation;
use crate::core::session::{SessionId, SessionStatus};
use crate::core::twin_arming::{ProcessFacts, STATUS_MAX_ANCESTOR_HOPS};
use crate::core::twin_identity::ClaudeProcess;
use crate::daemon::state::DaemonState;

/// What the transport can prove about the process that sent a repair (#8531).
///
/// Why: only the socket carries a kernel-reported peer; HTTP carries nothing
/// a caller cannot write. The reason travels with the missing pid, so the
/// refusal says which transport or lookup failed.
/// What: the peer pid, or why none is available.
/// Test: `an_unproven_peer_is_unestablished_8531`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepairPeer {
    /// The kernel-reported pid of the connected process.
    Kernel(u32),
    /// No kernel-verified pid; the text says why.
    Unproven(String),
}

impl RepairPeer {
    /// The peer of a socket request: the pid the socket server read off the
    /// connection, or `Unproven` when the kernel reported none.
    pub fn from_socket(pid: Option<u32>) -> Self {
        pid.map_or_else(
            || {
                Self::Unproven(
                    "the daemon socket could not read the calling process's pid from the kernel"
                        .to_string(),
                )
            },
            Self::Kernel,
        )
    }

    /// The peer of an HTTP request, which never proves a caller (#8531).
    pub fn http() -> Self {
        Self::Unproven(
            "the request came over HTTP, which carries no kernel-verified caller — \
             `tm repair delegation` sends it over the daemon socket (#8531)"
                .to_string(),
        )
    }
}

/// The caller of a repair over the records `matches` selects (#8531).
///
/// Why: the one production entry point, run on the blocking pool — the walk
/// reads the process table and the owner lookup may run `tmux`.
/// What: [`establish_caller_with`] over the owners of the matched open
/// records, the live process table, and [`owner_claude_pid`].
/// Test: `establish_caller_with`'s cases; `a_socket_caller_without_a_claude_owner_process_is_refused_8531`.
pub fn establish_caller(
    state: &Arc<DaemonState>,
    peer: RepairPeer,
    matches: impl Fn(&Delegation) -> bool,
) -> RepairCaller {
    let mut owners: Vec<SessionId> = Vec::new();
    for d in state.all_delegations() {
        if matches(&d) && !d.status.is_terminal() && !owners.contains(&d.session) {
            owners.push(d.session);
        }
    }
    establish_caller_with(
        peer,
        &owners,
        crate::core::twin_arming::nearest_claude_for_status,
        |s| owner_claude_pid(state, s, crate::core::twin_arming::process_facts),
    )
}

/// [`establish_caller`] over an injected process walk and owner lookup.
///
/// Why: every refusal arm — no pid, an unreadable ancestry, no `claude`
/// above the caller, an owner whose process cannot be found — must be
/// testable without a live session.
/// What: `Session(owner)` only when the nearest `claude` above `peer` is the
/// process `owner_claude` names for one of `owners`. Otherwise
/// `Unestablished`, naming the failed step. An owner lookup error is never
/// skipped silently: when no owner matches, its text is in the reason.
/// Test: `the_owner_process_establishes_the_owner_8531`,
/// `a_sibling_session_process_is_not_the_owner_8531`,
/// `an_unproven_peer_is_unestablished_8531`,
/// `an_unreadable_ancestry_is_unestablished_8531`,
/// `a_caller_with_no_claude_above_it_is_unestablished_8531`,
/// `an_owner_process_that_cannot_be_found_is_unestablished_8531`.
pub(crate) fn establish_caller_with(
    peer: RepairPeer,
    owners: &[SessionId],
    nearest_claude: impl Fn(u32) -> Result<Option<ClaudeProcess>, String>,
    owner_claude: impl Fn(SessionId) -> Result<u32, String>,
) -> RepairCaller {
    let pid = match peer {
        RepairPeer::Kernel(pid) => pid,
        RepairPeer::Unproven(why) => return RepairCaller::Unestablished(why),
    };
    // #8531: the kernel's pid, walked up to the session process that runs it.
    let caller = match nearest_claude(pid) {
        Ok(Some(claude)) => claude,
        Ok(None) => {
            return RepairCaller::Unestablished(format!(
                "no claude session process runs within {STATUS_MAX_ANCESTOR_HOPS} parent hops \
                 of the calling process (pid {pid})"
            ));
        }
        Err(e) => {
            return RepairCaller::Unestablished(format!(
                "the calling process's ancestry (pid {pid}) could not be read: {e}"
            ));
        }
    };
    let mut failures: Vec<String> = Vec::new();
    for owner in owners {
        match owner_claude(*owner) {
            Ok(owner_pid) if owner_pid == caller.pid => return RepairCaller::Session(*owner),
            Ok(_) => {}
            Err(e) => failures.push(e),
        }
    }
    let mut why = format!(
        "the calling process runs under claude pid {}, which is not the process of the session \
         that owns the record",
        caller.pid
    );
    if !failures.is_empty() {
        why.push_str(&format!(
            "; the owner's process could not be found: {}",
            failures.join("; ")
        ));
    }
    RepairCaller::Unestablished(why)
}

/// The pid of the `claude` process that runs session `session` (#8531).
///
/// Why: the owner check compares processes, not ids — an id is something a
/// caller can write, a live process tree is not.
/// What: a native session's recorded pid, accepted only while that pid's
/// process started no later than the record was registered (a reused pid
/// started after). A tmux session's `claude`, found under its pane now. A
/// missing or stopped record, a process-table miss, or no `claude` in the
/// pane is `Err`.
/// Test: `a_native_owner_pid_that_predates_its_record_is_its_process_8531`,
/// `a_native_owner_pid_started_after_its_record_is_refused_8531`,
/// `a_missing_or_stopped_owner_record_is_an_error_8531`.
pub(crate) fn owner_claude_pid(
    state: &DaemonState,
    session: SessionId,
    facts: impl Fn(u32) -> Result<ProcessFacts, String>,
) -> Result<u32, String> {
    let record = state
        .session(session)
        .ok_or_else(|| "the daemon holds no record of the owning session".to_string())?;
    if record.status == SessionStatus::Stopped {
        return Err("the owning session's record is stopped".to_string());
    }
    if let Some(pid) = record.pid {
        let registered = record
            .created_at
            .duration_since(UNIX_EPOCH)
            .map_err(|e| format!("the owning session's registration time is unreadable: {e}"))?
            .as_secs();
        let started = facts(pid)?.start_time;
        if started > registered {
            return Err(format!(
                "pid {pid} now names a process started after the owning session registered"
            ));
        }
        return Ok(pid);
    }
    crate::core::process::find_claude_pid_in_tmux(&record.tmux_name, 1, Duration::ZERO)
        .ok_or_else(|| "no claude process could be found in the owning session's pane".to_string())
}

#[cfg(test)]
#[path = "delegation_repair_caller_tests.rs"]
mod delegation_repair_caller_tests;
