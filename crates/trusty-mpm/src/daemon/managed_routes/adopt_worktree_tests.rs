//! Tests for the #6497 adopt-worktree route.
//!
//! Why: the route is where the two liveness registries meet the pure gate, so
//! its tests must drive REAL registry state — a hand-built verdict would prove
//! only that the gate was called.
//! What: the refusal a live owner earns, and the transfer a dead one permits.
//! Test: this file IS the test module.

use super::*;

use crate::core::agent::{Delegation, DelegationStatus, ModelTier};
use crate::core::session::SessionId;
use crate::session_manager::decommission::WORKTREE_SENTINEL_FILE;
use crate::session_manager::worktree_ownership::{AgentWorktreeOwner, WorktreeSentinel};

/// A directory carrying an AGENT ownership sentinel naming `agent_id`.
fn agent_owned_tree(agent_id: &str) -> (tempfile::TempDir, SessionId) {
    let dir = tempfile::tempdir().expect("tempdir");
    let parent = SessionId::new();
    let payload = WorktreeSentinel::for_agent(AgentWorktreeOwner {
        agent_id: agent_id.to_string(),
        delegation_id: crate::core::agent::DelegationId::new(),
        parent_session_id: parent,
    });
    std::fs::write(
        dir.path().join(WORKTREE_SENTINEL_FILE),
        serde_json::to_vec(&payload).expect("serialize sentinel"),
    )
    .expect("write sentinel");
    (dir, parent)
}

/// A delegation naming `agent_id`, in `status`.
fn delegation_for(session: SessionId, agent_id: &str, status: DelegationStatus) -> Delegation {
    let mut d = Delegation::new(session, None, "rust-engineer", ModelTier::Sonnet, "work");
    d.agent_id = Some(agent_id.to_string());
    d.status = status;
    d
}

/// A live owner keeps its tree, and the sentinel is left exactly as it was.
#[tokio::test]
async fn adopt_worktree_route_refuses_a_live_owner() {
    let (tree, parent) = agent_owned_tree("agent-alive");
    let state = Arc::new(DaemonState::new());
    state.upsert_delegation(delegation_for(
        parent,
        "agent-alive",
        DelegationStatus::Running,
    ));
    let before = std::fs::read(tree.path().join(WORKTREE_SENTINEL_FILE)).expect("read sentinel");

    let outcome = adopt_worktree_core(
        &state,
        AdoptWorktreeRequest {
            path: tree.path().to_path_buf(),
            as_session: ManagedSessionId::new(),
        },
    )
    .await;

    assert_eq!(outcome.status, 409, "a live owner's tree must be refused");
    assert_eq!(
        std::fs::read(tree.path().join(WORKTREE_SENTINEL_FILE)).expect("read sentinel"),
        before,
        "a refusal must write nothing"
    );
}

/// The #6497 case: every delegation naming the owning agent has ended, nothing
/// else claims the tree, and the sentinel is rewritten to the adopting session.
#[tokio::test]
async fn adopt_worktree_route_transfers_a_dead_owners_tree() {
    let (tree, parent) = agent_owned_tree("agent-ended");
    let state = Arc::new(DaemonState::new());
    state.upsert_delegation(delegation_for(
        parent,
        "agent-ended",
        DelegationStatus::Completed,
    ));
    let successor = ManagedSessionId::new();

    let outcome = adopt_worktree_core(
        &state,
        AdoptWorktreeRequest {
            path: tree.path().to_path_buf(),
            as_session: successor,
        },
    )
    .await;

    assert_eq!(outcome.status, 200, "a dead owner's tree is adoptable");
    match read_sentinel_owner(tree.path()) {
        SentinelOwner::Known(id, _) => assert_eq!(id, successor),
        other => panic!("the sentinel must name the adopting session; got {other:?}"),
    }
}
