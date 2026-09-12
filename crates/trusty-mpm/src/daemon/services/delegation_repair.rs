//! Ending a delegation record that no signal can ever end (#7602).
//!
//! Why: a dispatched agent's record leaves `Running` only on a `SubagentStop`
//! the daemon actually receives. When the parent session dies without one — the
//! harness is killed, the `tm hook` POST loses its 2 s budget, the daemon
//! restarts mid-flight — the record stays non-terminal forever, and
//! [`crate::session_manager::worktree_reclaim::agent_ownership_blocks`] reads
//! non-terminal as `Live` and refuses the agent's worktree for good. #6497 and
//! #6556 closed the cases where the daemon SEES the owner die; this one is the
//! case where it never sees anything, and before this module no `tm` verb could
//! end such a record at all. The live specimen is
//! `.claude/worktrees/agent-af20cc838b2b30a55`, whose tree both
//! `tm session prune-worktrees --merged-prs` and `tm session adopt-worktree`
//! refused with "a delegation naming it has not ended".
//!
//! # Why the status written is terminal, and why `Stale` would not do
//!
//! [`Stale`](crate::core::agent::DelegationStatus::Stale) is deliberately
//! neither live nor terminal, and
//! the two consumers read that differently: the idle nudge treats it as not
//! live, but
//! [`crate::daemon::services::agent_worktree_reap::delegation_state_for_agent`]
//! — the reclaim gate's only informant — asks `is_terminal()` and so still
//! answers [`crate::session_manager::worktree_ownership::AgentDelegationState::Live`].
//! Staling therefore does not release the worktree, which is exactly why the
//! specimen survived #6497's session-end sweep. The repair writes
//! [`Cancelled`](crate::core::agent::DelegationStatus::Cancelled): an explicit
//! terminal state meaning "this
//! delegation was ended by an operator because nothing else could end it",
//! never `Completed`, which would claim the agent finished its work.
//!
//! # Fail direction
//!
//! Closed, in both of the two ways that matter. A record whose owning session is
//! still live is REFUSED whatever the caller passes — ending it would readmit a
//! second writer onto a HEAD a live agent holds, the ADR-0048 harm. A record
//! whose owner the registry has never heard of is UNDETERMINABLE, not absent
//! (ADR-0045): the delegation map is built empty at every boot, so silence after
//! a restart looks identical to "the session is gone". That arm refuses too, and
//! names `--force` as the operator's explicit assertion — which is the whole
//! point of a sanctioned verb rather than a timer that guesses.
//!
//! One residual remains, and it is bounded rather than closed. [`owner_liveness`]
//! keys on the exact [`SessionId`] recorded at dispatch, so a session whose
//! record was REMOVED (rather than marked `Stopped`) while its pane runs on
//! under a reissued id reads as `Unknown`, and `--force` would then cancel a
//! delegation whose agent may still be writing: `--force` trusts the daemon's
//! own session bookkeeping, not the OS and not git. Nothing is deleted on that
//! evidence, though — every destructive path downstream re-checks a live OS
//! process before removing anything (`agent_worktree_reap`'s gate 6,
//! `worktree_reclaim_sweep`, #7504) — so the worst outcome is a status flipped
//! early, not a live agent's tree removed.
//! Test: `delegation_repair_tests`.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::core::session::{SessionId, SessionStatus};
use crate::daemon::state::DaemonState;

/// What the daemon can say about the session that owns a stuck record.
///
/// Why: "the owner is gone" and "the registry cannot say" are the same empty
/// answer read two ways, and only the first is evidence — the same split
/// [`crate::session_manager::worktree_ownership::AgentDelegationState`] makes
/// one level up, for the same ADR-0045 reason.
/// What: `Live` — a session record exists, is Active, and its tracked process
/// (when one is tracked) is alive. `Gone` — a record exists and says otherwise.
/// `Unknown` — no record at all.
/// Test: `owner_liveness_reads_active_stopped_and_absent_7602`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OwnerLiveness {
    /// The owning session is still running.
    Live,
    /// The daemon holds positive evidence the owner is gone.
    Gone,
    /// The daemon holds no record of the owner, which proves nothing.
    Unknown,
}

/// What one `tm repair delegation` call did, or refused to do.
///
/// Why: the caller's exit status turns on this, and a refusal has to carry the
/// gate's own words — a repair that reports "nothing to do" for a record it
/// declined to touch is the silence this verb exists to remove.
/// What: the four outcomes, tagged for the wire.
/// Test: `repair_ends_a_stuck_record_of_a_dead_owner_7602`,
/// `repair_refuses_while_the_owner_is_live_7602`,
/// `repair_refuses_an_undeterminable_owner_without_force_7602`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum RepairOutcome {
    /// No delegation names this agent at all.
    NoRecord,
    /// Every delegation naming this agent is already terminal.
    AlreadyEnded {
        /// How many terminal records name the agent.
        records: usize,
    },
    /// A gate refused; nothing was written.
    Refused {
        /// The gate's own wording.
        reason: String,
    },
    /// Non-terminal records were ended.
    Ended {
        /// How many records this call terminalized.
        records: usize,
    },
}

/// The request body of the repair route.
///
/// Why: `force` is an operator assertion, so it travels explicitly rather than
/// being inferred from anything the daemon can see.
/// What: one flag, defaulting to the refusing behaviour.
/// Test: `repair_refuses_an_undeterminable_owner_without_force_7602`.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct RepairDelegationRequest {
    /// End the record even when the owning session is undeterminable.
    #[serde(default)]
    pub force: bool,
}

/// The whole gate, as a pure function of the counts and the owner's liveness.
///
/// Why: every refusal arm is testable without a daemon, a session store, or a
/// hook payload — and the arms are the point of the verb, not the write.
/// What: nothing named → `NoRecord`; nothing non-terminal → `AlreadyEnded`; a
/// live owner → `Refused` unconditionally, `force` included; an unknown owner →
/// `Refused` unless `force`; otherwise `Ended`.
/// Test: `decide_refuses_a_live_owner_even_with_force_7602`,
/// `decide_needs_force_for_an_unknown_owner_7602`,
/// `decide_ends_a_gone_owners_records_7602`, `decide_reports_no_record`,
/// `decide_reports_already_ended`.
pub(crate) fn decide(
    open: usize,
    terminal: usize,
    owner: OwnerLiveness,
    force: bool,
) -> RepairOutcome {
    if open == 0 && terminal == 0 {
        return RepairOutcome::NoRecord;
    }
    if open == 0 {
        return RepairOutcome::AlreadyEnded { records: terminal };
    }
    match owner {
        // Never, at any force. A live owner's agent may still be writing, and
        // releasing its tree is the ADR-0048 harm this refusal exists for.
        OwnerLiveness::Live => RepairOutcome::Refused {
            reason: "the session that dispatched this agent is still Active — ending its \
                     delegation would readmit a second writer onto a working tree the agent may \
                     still hold. Stop the session first (#7602)"
                .to_string(),
        },
        OwnerLiveness::Unknown if !force => RepairOutcome::Refused {
            reason: "the daemon holds no record of the session that dispatched this agent, so it \
                     cannot confirm the owner is gone — the delegation map is rebuilt empty at \
                     every daemon start, and silence after a restart is undeterminable, not \
                     absent (ADR-0045). Re-run with --force to assert the owner is gone (#7602)"
                .to_string(),
        },
        OwnerLiveness::Unknown | OwnerLiveness::Gone => RepairOutcome::Ended { records: open },
    }
}

/// What the daemon can say about the session owning `session`.
///
/// Why: the one reading of the session registry this verb makes, so the three
/// arms cannot drift from the `reap_against` rules they mirror.
/// What: absent → `Unknown`; `Stopped`, or a tracked pid that is not alive →
/// `Gone`; anything else → `Live`.
/// Test: `owner_liveness_reads_active_stopped_and_absent_7602`.
pub(crate) fn owner_liveness(state: &Arc<DaemonState>, session: SessionId) -> OwnerLiveness {
    let Some(record) = state.session(session) else {
        return OwnerLiveness::Unknown;
    };
    if record.status == SessionStatus::Stopped {
        return OwnerLiveness::Gone;
    }
    match record.pid {
        Some(pid) if !crate::core::process::is_process_alive(pid) => OwnerLiveness::Gone,
        _ => OwnerLiveness::Live,
    }
}

/// End every non-terminal delegation naming `agent_id`, or say why not (#7602).
///
/// Why: the operator-facing half — `tm repair delegation <agent-id>` calls the
/// route that calls this. It is the only sanctioned way a stuck record ends, and
/// it writes a terminal status precisely so the reclaim gate, which reads
/// `is_terminal()`, stops refusing the agent's worktree.
/// What: tallies the records naming the agent, resolves the strictest owner
/// liveness across their sessions (any live owner wins), runs [`decide`], and on
/// `Ended` writes [`Cancelled`](crate::core::agent::DelegationStatus::Cancelled)
/// to each non-terminal record.
/// Nothing is written on any other outcome.
/// Test: `repair_ends_a_stuck_record_of_a_dead_owner_7602`,
/// `repair_refuses_while_the_owner_is_live_7602`,
/// `repair_refuses_an_undeterminable_owner_without_force_7602`,
/// `repair_force_ends_a_record_whose_owner_is_unknown_7602`,
/// `repair_reports_no_record_for_an_unknown_agent_7602`.
pub fn repair_delegation(state: &Arc<DaemonState>, agent_id: &str, force: bool) -> RepairOutcome {
    let mut open = 0usize;
    let mut terminal = 0usize;
    let mut owner = None;
    for d in state.all_delegations() {
        if d.agent_id.as_deref() != Some(agent_id) {
            continue;
        }
        if d.status.is_terminal() {
            terminal += 1;
            continue;
        }
        open += 1;
        // Any live owner decides the whole call: the strictest answer wins.
        let liveness = owner_liveness(state, d.session);
        owner = Some(match (owner, liveness) {
            (Some(OwnerLiveness::Live), _) | (_, OwnerLiveness::Live) => OwnerLiveness::Live,
            (Some(OwnerLiveness::Gone), _) | (_, OwnerLiveness::Gone) => OwnerLiveness::Gone,
            _ => OwnerLiveness::Unknown,
        });
    }

    let outcome = decide(
        open,
        terminal,
        owner.unwrap_or(OwnerLiveness::Unknown),
        force,
    );
    if let RepairOutcome::Ended { .. } = outcome {
        // #7602: Cancelled, never Completed — the agent did not report finishing,
        // an operator ended the record because nothing else could.
        state.cancel_stuck_delegations_of_agent(agent_id);
        tracing::warn!(
            agent_id,
            ended = open,
            forced = force,
            "delegation: operator-repaired a stuck record — status Cancelled (#7602)"
        );
    }
    outcome
}

#[cfg(test)]
#[path = "delegation_repair_tests.rs"]
mod delegation_repair_tests;
