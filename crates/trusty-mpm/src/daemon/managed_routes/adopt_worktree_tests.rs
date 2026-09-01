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

/// Write an AGENT ownership sentinel naming `agent_id` into `dir`.
fn write_agent_sentinel(dir: &std::path::Path, agent_id: &str, parent: SessionId) {
    let payload = WorktreeSentinel::for_agent(AgentWorktreeOwner {
        agent_id: agent_id.to_string(),
        delegation_id: crate::core::agent::DelegationId::new(),
        parent_session_id: parent,
    });
    std::fs::write(
        dir.join(WORKTREE_SENTINEL_FILE),
        serde_json::to_vec(&payload).expect("serialize sentinel"),
    )
    .expect("write sentinel");
}

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

/// A tree that exists under two names — `<tmp>/real/tree` and, through a
/// symlinked parent, `<tmp>/link/tree` — with an ENDED owner, so only the
/// claimant gate can refuse.
///
/// Returns the tempdir, the real spelling, and the symlinked spelling.
fn symlinked_tree(
    state: &Arc<DaemonState>,
    agent_id: &str,
) -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = std::fs::canonicalize(tmp.path()).expect("canonicalize tempdir");
    let real = root.join("real");
    std::fs::create_dir_all(real.join("tree")).expect("mkdir real/tree");
    std::os::unix::fs::symlink(&real, root.join("link")).expect("symlink");

    let parent = SessionId::new();
    write_agent_sentinel(&real.join("tree"), agent_id, parent);
    state.upsert_delegation(delegation_for(
        parent,
        agent_id,
        DelegationStatus::Completed,
    ));
    (tmp, real.join("tree"), root.join("link").join("tree"))
}

/// A live delegation working in the tree under a DIFFERENT-but-equivalent
/// spelling still refuses adoption.
///
/// Why this is the gate's real failure mode rather than an exotic one: a raw
/// `starts_with` is lexical, and the delegation's `cwd` and the request's path
/// routinely reach the daemon through different symlinks. Missing the match
/// hands away a tree an agent is writing in.
#[tokio::test]
async fn adopt_worktree_route_refuses_a_claimant_under_a_symlinked_spelling() {
    let state = Arc::new(DaemonState::new());
    let (_tmp, real, linked) = symlinked_tree(&state, "agent-ended-symlink");

    // The claimant records the REAL spelling; the request asks under the
    // SYMLINKED one. Lexically these share no prefix.
    let mut claimant = delegation_for(SessionId::new(), "other-agent", DelegationStatus::Running);
    claimant.agent = "rust-engineer".to_string();
    claimant.cwd = Some(real.clone());
    state.upsert_delegation(claimant);
    assert!(
        !linked.starts_with(&real),
        "the fixture must present two spellings a lexical prefix test cannot relate"
    );

    let outcome = adopt_worktree_core(
        &state,
        AdoptWorktreeRequest {
            path: linked.clone(),
            as_session: ManagedSessionId::new(),
        },
    )
    .await;

    assert_eq!(
        outcome.status, 409,
        "a live claimant under an equivalent spelling must refuse; body was: {:?}",
        outcome.body
    );
    // And the reverse direction: request the real path, claim the linked one.
    let state = Arc::new(DaemonState::new());
    let (_tmp2, real2, linked2) = symlinked_tree(&state, "agent-ended-symlink-2");
    let mut claimant = delegation_for(SessionId::new(), "other-agent", DelegationStatus::Running);
    claimant.agent = "rust-engineer".to_string();
    claimant.cwd = Some(linked2);
    state.upsert_delegation(claimant);
    let outcome = adopt_worktree_core(
        &state,
        AdoptWorktreeRequest {
            path: real2,
            as_session: ManagedSessionId::new(),
        },
    )
    .await;
    assert_eq!(outcome.status, 409, "the mirror spelling must refuse too");
}

/// The same tree spelled with a trailing slash, and with a `..` hop through it,
/// still refuses. Both are the ordinary shapes a recorded `cwd` arrives in.
#[tokio::test]
async fn adopt_worktree_route_refuses_a_claimant_under_a_trailing_slash_spelling() {
    for spelling in ["trailing-slash", "dot-dot"] {
        let state = Arc::new(DaemonState::new());
        let (_tmp, real, _linked) = symlinked_tree(&state, "agent-ended-slash");
        let cwd = match spelling {
            "trailing-slash" => std::path::PathBuf::from(format!("{}/", real.display())),
            _ => real.join("..").join("tree"),
        };

        let mut claimant =
            delegation_for(SessionId::new(), "other-agent", DelegationStatus::Running);
        claimant.agent = "rust-engineer".to_string();
        claimant.cwd = Some(cwd);
        state.upsert_delegation(claimant);

        let outcome = adopt_worktree_core(
            &state,
            AdoptWorktreeRequest {
                path: real,
                as_session: ManagedSessionId::new(),
            },
        )
        .await;
        assert_eq!(
            outcome.status, 409,
            "the {spelling} spelling must refuse; body was: {:?}",
            outcome.body
        );
    }
}
