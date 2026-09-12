//! Tests for the #7602 operator repair of a stuck delegation record.
//!
//! Why: every arm of this verb is a refusal, and a refusal that silently
//! becomes a write is how a live agent's working tree gets handed to a second
//! writer. The gate is therefore covered arm by arm, both as the pure decision
//! and through the state-mutating entry point.
//! What: [`decide`]'s five outcomes, [`owner_liveness`]'s three, and
//! [`repair_delegation`]'s write-or-refuse behaviour over a real
//! [`DaemonState`].
//! Test: this file IS the test module.

use super::*;
use crate::core::agent::{Delegation, DelegationStatus, ModelTier};
use crate::core::session::{ControlModel, Session};
use crate::daemon::state::DaemonState;

/// A daemon holding one session in `status`, or holding none at all.
fn state_with(status: Option<SessionStatus>) -> (Arc<DaemonState>, SessionId) {
    let state = Arc::new(DaemonState::new());
    let id = SessionId::new();
    if let Some(status) = status {
        let mut s = Session::new(id, "/tmp/p", ControlModel::Tmux, None);
        s.status = status;
        state.register_session(s);
    }
    (state, id)
}

/// One delegation naming `agent_id`, in `status`.
fn delegation(session: SessionId, agent_id: &str, status: DelegationStatus) -> Delegation {
    let mut d = Delegation::new(session, None, "rust-engineer", ModelTier::Sonnet, "work");
    d.agent_id = Some(agent_id.to_string());
    d.status = status;
    d
}

// #7602: the reported shape — a record left non-terminal by a session that
// ended without a stop hook, whose owner the daemon can see is gone.
#[test]
fn repair_ends_a_stuck_record_of_a_dead_owner_7602() {
    let (state, session) = state_with(Some(SessionStatus::Stopped));
    state.upsert_delegation(delegation(
        session,
        "af20cc838b2b30a55",
        DelegationStatus::Running,
    ));

    let outcome = repair_delegation(&state, "af20cc838b2b30a55", false);

    assert_eq!(outcome, RepairOutcome::Ended { records: 1 });
    // The status must be TERMINAL: `Stale` is what #6497 already wrote and is
    // exactly what left the worktree gate refusing.
    let d = &state.all_delegations()[0];
    assert_eq!(d.status, DelegationStatus::Cancelled);
    assert!(d.status.is_terminal(), "the repair must release the gate");
}

// #7602: the closure condition, end to end — the worktree gate that refused the
// specimen ("a delegation naming it has not ended") answers `Ended` afterwards.
#[test]
fn repair_releases_the_worktree_reclaim_gate_7602() {
    use crate::daemon::services::agent_worktree_reap::delegation_state_for_agent;
    use crate::session_manager::worktree_ownership::AgentDelegationState;

    let (state, session) = state_with(Some(SessionStatus::Stopped));
    state.upsert_delegation(delegation(
        session,
        "af20cc838b2b30a55",
        DelegationStatus::Running,
    ));
    assert_eq!(
        delegation_state_for_agent(&state, "af20cc838b2b30a55"),
        AgentDelegationState::Live,
        "precondition: the gate refuses the tree"
    );

    repair_delegation(&state, "af20cc838b2b30a55", false);

    assert_eq!(
        delegation_state_for_agent(&state, "af20cc838b2b30a55"),
        AgentDelegationState::Ended
    );
}

// #7602: the whole point of the reclaim gate — a live owner is never overridden,
// whatever the operator asserts.
#[test]
fn repair_refuses_while_the_owner_is_live_7602() {
    let (state, session) = state_with(Some(SessionStatus::Active));
    state.upsert_delegation(delegation(session, "agent-live", DelegationStatus::Running));

    let outcome = repair_delegation(&state, "agent-live", true);

    match outcome {
        RepairOutcome::Refused { reason } => assert!(reason.contains("still Active"), "{reason}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert_eq!(state.all_delegations()[0].status, DelegationStatus::Running);
}

// #7602 / ADR-0045: the delegation map is rebuilt empty at every daemon start,
// so an owner the registry cannot find is undeterminable, not gone.
#[test]
fn repair_refuses_an_undeterminable_owner_without_force_7602() {
    let (state, session) = state_with(None);
    state.upsert_delegation(delegation(
        session,
        "agent-unknown",
        DelegationStatus::Running,
    ));

    let outcome = repair_delegation(&state, "agent-unknown", false);

    match outcome {
        RepairOutcome::Refused { reason } => assert!(reason.contains("--force"), "{reason}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert_eq!(state.all_delegations()[0].status, DelegationStatus::Running);
}

#[test]
fn repair_force_ends_a_record_whose_owner_is_unknown_7602() {
    let (state, session) = state_with(None);
    state.upsert_delegation(delegation(
        session,
        "agent-unknown",
        DelegationStatus::Running,
    ));

    assert_eq!(
        repair_delegation(&state, "agent-unknown", true),
        RepairOutcome::Ended { records: 1 }
    );
    assert_eq!(
        state.all_delegations()[0].status,
        DelegationStatus::Cancelled
    );
}

// #7602: a `Stale` record is NOT terminal, so it is exactly the population this
// verb exists to end — the #6497 sweep leaves records in this state and the
// reclaim gate still reads them as live.
#[test]
fn repair_ends_a_stale_record_too_7602() {
    let (state, session) = state_with(Some(SessionStatus::Stopped));
    state.upsert_delegation(delegation(session, "agent-stale", DelegationStatus::Stale));

    assert_eq!(
        repair_delegation(&state, "agent-stale", false),
        RepairOutcome::Ended { records: 1 }
    );
}

#[test]
fn repair_reports_no_record_for_an_unknown_agent_7602() {
    let (state, _session) = state_with(Some(SessionStatus::Stopped));
    assert_eq!(
        repair_delegation(&state, "agent-nobody-knows", false),
        RepairOutcome::NoRecord
    );
}

#[test]
fn repair_reports_already_ended_and_writes_nothing_7602() {
    let (state, session) = state_with(Some(SessionStatus::Stopped));
    state.upsert_delegation(delegation(
        session,
        "agent-done",
        DelegationStatus::Completed,
    ));

    assert_eq!(
        repair_delegation(&state, "agent-done", false),
        RepairOutcome::AlreadyEnded { records: 1 }
    );
    assert_eq!(
        state.all_delegations()[0].status,
        DelegationStatus::Completed
    );
}

#[test]
fn owner_liveness_reads_active_stopped_and_absent_7602() {
    let (active, id) = state_with(Some(SessionStatus::Active));
    assert_eq!(owner_liveness(&active, id), OwnerLiveness::Live);

    let (stopped, id) = state_with(Some(SessionStatus::Stopped));
    assert_eq!(owner_liveness(&stopped, id), OwnerLiveness::Gone);

    let (empty, id) = state_with(None);
    assert_eq!(owner_liveness(&empty, id), OwnerLiveness::Unknown);
}

#[test]
fn decide_refuses_a_live_owner_even_with_force_7602() {
    assert!(matches!(
        decide(1, 0, OwnerLiveness::Live, true),
        RepairOutcome::Refused { .. }
    ));
}

#[test]
fn decide_needs_force_for_an_unknown_owner_7602() {
    assert!(matches!(
        decide(1, 0, OwnerLiveness::Unknown, false),
        RepairOutcome::Refused { .. }
    ));
    assert_eq!(
        decide(1, 0, OwnerLiveness::Unknown, true),
        RepairOutcome::Ended { records: 1 }
    );
}

#[test]
fn decide_ends_a_gone_owners_records_7602() {
    assert_eq!(
        decide(2, 1, OwnerLiveness::Gone, false),
        RepairOutcome::Ended { records: 2 }
    );
}

#[test]
fn decide_reports_no_record() {
    assert_eq!(
        decide(0, 0, OwnerLiveness::Unknown, true),
        RepairOutcome::NoRecord
    );
}

#[test]
fn decide_reports_already_ended() {
    assert_eq!(
        decide(0, 3, OwnerLiveness::Gone, false),
        RepairOutcome::AlreadyEnded { records: 3 }
    );
}
