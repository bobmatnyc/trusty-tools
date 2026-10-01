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
//! peer process; it is the owner only when that `claude` — pid AND start time
//! — IS the process the owning session was bound to when it announced itself
//! over the socket (`daemon::state::session_claudes`). Every lookup that
//! cannot answer yields [`RepairCaller::Unestablished`], which the repair
//! gate refuses.
//! Test: `delegation_repair_caller_tests.rs`.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use trusty_common::uds::server::RequestPeer;

use super::delegation_repair::RepairCaller;
use crate::core::agent::Delegation;
use crate::core::session::SessionId;
use crate::core::twin_identity::ClaudeProcess;
use crate::daemon::state::DaemonState;

/// What the transport can prove about the process that sent a repair (#8531).
///
/// Why: only the socket carries a kernel-reported peer; HTTP carries nothing
/// a caller cannot write. The reason travels with the missing pid, so the
/// refusal says which transport or lookup failed.
/// What: the peer pid with the Unix second the request was seen, or why no
/// pid is available.
/// Test: `an_unproven_peer_is_unestablished_8531`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepairPeer {
    /// The kernel-reported pid of the connected process.
    Kernel {
        /// The peer's pid.
        pid: u32,
        /// #8531: when the request was seen, Unix seconds — a process under
        /// this pid that started later holds a reused pid, not the peer's.
        seen_at: u64,
    },
    /// No kernel-verified pid; the text says why.
    Unproven(String),
}

/// `at` in Unix seconds; `0` when the clock reads before the epoch, which
/// every real process postdates, so a broken clock refuses every peer.
fn unix_secs(at: SystemTime) -> u64 {
    at.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

impl RepairPeer {
    /// The peer of a socket request: the pid the socket server read off the
    /// connection, seen when the connection was accepted, or `Unproven` when
    /// the kernel reported none.
    // #8531: `seen_at` is the accept instant, stamped before the frame read.
    pub fn from_socket(peer: Option<RequestPeer>) -> Self {
        peer.map_or_else(
            || {
                Self::Unproven(
                    "the daemon socket could not read the calling process's pid from the kernel"
                        .to_string(),
                )
            },
            |peer| Self::Kernel {
                pid: peer.pid,
                seen_at: unix_secs(peer.accepted_at),
            },
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
/// reads the process table.
/// What: [`establish_caller_with`] over the owners of the matched open
/// records, the live process table, and [`owner_claude`].
/// Test: `establish_caller_with`'s cases;
/// `a_socket_caller_without_a_claude_owner_process_is_refused_8531`,
/// `the_bound_owner_ends_its_own_record_over_the_socket_8531`.
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
    let claudes = state.session_claudes();
    establish_caller_with(
        peer,
        &owners,
        |pid, seen_at| claudes.peer_claude(pid, seen_at),
        |s| owner_claude(state, s),
    )
}

/// [`establish_caller`] over an injected process walk and owner lookup.
///
/// Why: every refusal arm — no pid, a reused pid, an unreadable ancestry, no
/// `claude` above the caller, an owner with no bound process — must be
/// testable without a live session.
/// What: `Session(owner)` only when the nearest `claude` above `peer` equals
/// — pid and start time — the process `owner_claude` names for one of
/// `owners`. Otherwise `Unestablished`, naming the failed step. An owner
/// lookup error is never skipped silently: when no owner matches, its text
/// is in the reason.
/// Test: `the_owner_process_establishes_the_owner_8531`,
/// `a_sibling_session_process_is_not_the_owner_8531`,
/// `a_reused_owner_pid_with_another_start_time_is_not_the_owner_8531`,
/// `an_unproven_peer_is_unestablished_8531`,
/// `a_failed_peer_walk_is_unestablished_8531`,
/// `an_owner_process_that_cannot_be_found_is_unestablished_8531`.
pub(crate) fn establish_caller_with(
    peer: RepairPeer,
    owners: &[SessionId],
    peer_claude: impl Fn(u32, u64) -> Result<ClaudeProcess, String>,
    owner_claude: impl Fn(SessionId) -> Result<ClaudeProcess, String>,
) -> RepairCaller {
    let (pid, seen_at) = match peer {
        RepairPeer::Kernel { pid, seen_at } => (pid, seen_at),
        RepairPeer::Unproven(why) => return RepairCaller::Unestablished(why),
    };
    // #8531: the kernel's pid, walked up to the session process that runs it.
    let caller = match peer_claude(pid, seen_at) {
        Ok(claude) => claude,
        Err(e) => return RepairCaller::Unestablished(e),
    };
    let mut failures: Vec<String> = Vec::new();
    for owner in owners {
        match owner_claude(*owner) {
            // #8531: pid AND start time — a reused pid is another process.
            Ok(bound) if bound == caller => return RepairCaller::Session(*owner),
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

/// Bind `session` to the `claude` that announced it over the socket (#8531).
///
/// Why: the owner's process identity must come from the kernel at the moment
/// the session announced itself — `SessionStart`, sent by `tm hook` as a
/// child of that session's `claude` — never from a field a caller writes
/// later.
/// What: the nearest `claude` above the kernel peer `peer`
/// ([`SessionClaudes::peer_claude`](crate::daemon::state::session_claudes::SessionClaudes::peer_claude)),
/// then [`DaemonState::bind_session_claude`] (first announcement wins), both
/// on the blocking pool — the walk reads the process table and the bind
/// writes the registry file. `Ok` without a walk when the id's first
/// announcement is already recorded. `Err` naming the failed step when there
/// is no peer pid, the walk fails, or the bind is refused; `ingest_hook` then
/// records the id as unproven, and an unproven owner is refused.
/// Test: `the_bound_owner_ends_its_own_record_over_the_socket_8531`,
/// `a_session_start_without_a_peer_pid_binds_nothing_8531`,
/// `a_session_start_whose_walk_fails_binds_nothing_8531`,
/// `a_restart_does_not_hand_the_owner_id_to_a_sibling_8531`.
pub async fn bind_announcing_claude(
    state: &Arc<DaemonState>,
    session: SessionId,
    peer: RepairPeer,
) -> Result<(), String> {
    let (pid, seen_at) = match peer {
        RepairPeer::Kernel { pid, seen_at } => (pid, seen_at),
        RepairPeer::Unproven(why) => return Err(why),
    };
    // #8531: a recorded first announcement is final, so skip the walk.
    if state.session_claudes().is_settled(session) {
        return Ok(());
    }
    let held = Arc::clone(state);
    tokio::task::spawn_blocking(move || {
        let claude = held.session_claudes().peer_claude(pid, seen_at)?;
        held.bind_session_claude(session, claude)
    })
    .await
    .map_err(|e| format!("the process walk did not finish: {e}"))?
}

/// The `claude` process that owns session `session` (#8531).
///
/// Why: the owner check compares processes, not ids or names. A session
/// record's `pid` is PATCHable and its tmux name can be squatted, so neither
/// is read; only the kernel-derived binding made when the session announced
/// itself over the socket is.
/// What: [`owner_claude_with`] over the live process table.
/// Test: `an_unbound_owner_has_no_process_8531`,
/// `a_patched_pid_does_not_establish_the_owner_8531`,
/// `a_squatted_tmux_name_does_not_establish_the_owner_8531`.
pub(crate) fn owner_claude(
    state: &DaemonState,
    session: SessionId,
) -> Result<ClaudeProcess, String> {
    owner_claude_with(state, session, crate::core::twin_arming::process_facts)
}

/// [`owner_claude`] over an injected process table (#8531).
///
/// What: the [`ClaudeProcess`] bound to `session` while a process with that
/// pid AND start time still runs. `Err` when no binding exists — the session
/// announced itself over HTTP, before this binding existed, or never — and
/// when the bound process has exited or its pid now names a later process: a
/// persisted binding outlives its process, and must then grant nothing.
/// Test: `a_bound_owner_whose_claude_exited_has_no_process_8531`,
/// `an_unbound_owner_has_no_process_8531`.
pub(crate) fn owner_claude_with(
    state: &DaemonState,
    session: SessionId,
    facts: impl Fn(u32) -> Result<crate::core::twin_arming::ProcessFacts, String>,
) -> Result<ClaudeProcess, String> {
    let bound = state.session_claudes().get(session).ok_or_else(|| {
        "the owning session never announced its claude process over the daemon socket".to_string()
    })?;
    // #8531: the binding persists across restarts; its process may not.
    match facts(bound.pid) {
        Ok(f) if f.start_time == bound.start_time => Ok(bound),
        _ => Err(format!(
            "the owning session's claude (pid {}) is no longer running",
            bound.pid
        )),
    }
}

#[cfg(test)]
#[path = "delegation_repair_caller_tests.rs"]
mod delegation_repair_caller_tests;
