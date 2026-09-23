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
use crate::daemon::rpc::managed::outcome::RouteBody;
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

/// A REAL git worktree, harness-locked for `agent_id` at `pid`, carrying an
/// agent ownership sentinel and NO delegation record (#7974).
///
/// Why a real worktree rather than a bare tempdir: the fallback's whole input
/// is `git worktree list --porcelain`'s lock reason, so a directory git has
/// never heard of exercises only the silent arm.
/// Why no delegation record: that IS the reported state. A delegation map is
/// rebuilt empty at every daemon boot, so after a restart the registry answers
/// `Unknown` for an agent that may have died days ago.
fn harness_locked_tree(
    agent_id: &str,
    pid: u32,
) -> (
    crate::session_manager::worktree_git_fixture::GitWorktreeFixture,
    std::path::PathBuf,
) {
    let fixture = crate::session_manager::worktree_git_fixture::GitWorktreeFixture::new();
    let wt = fixture.add_worktree(agent_id);
    write_agent_sentinel(&wt, agent_id, SessionId::new());
    fixture.harness_lock_worktree_with_pid(&wt, agent_id, pid);
    (fixture, wt)
}

/// #7974, the reported case: a dispatched agent's tree is adoptable once the
/// agent is provably gone, even though the daemon restart left the delegation
/// registry with no record of it.
///
/// Fails before #7974: the registry answers `Unknown`, which ADR-0045 refuses,
/// so the 409 named the dead agent and the operator's uncommitted work stayed
/// unreachable by the documented verb.
/// Test: this function IS the test.
#[tokio::test]
async fn adopt_worktree_route_takes_a_dead_agents_tree_after_a_daemon_restart() {
    // A reaped child's pid is provably free, unlike any constant.
    let mut child = std::process::Command::new("true")
        .spawn()
        .expect("spawn a process that exits immediately");
    let dead_pid = child.id();
    child.wait().expect("reap the child");
    assert_eq!(
        crate::session_manager::worktree_registry::pid_liveness(dead_pid),
        Some(false),
        "premise broken: the fixture pid must be genuinely gone"
    );

    let (_fixture, tree) = harness_locked_tree("agent-a92efd8de8a9e6960", dead_pid);
    // A daemon that has just restarted: no delegation record either way.
    let state = Arc::new(DaemonState::new());
    let successor = ManagedSessionId::new();

    let outcome = adopt_worktree_core(
        &state,
        AdoptWorktreeRequest {
            path: tree.clone(),
            as_session: successor,
        },
    )
    .await;

    assert_eq!(
        outcome.status, 200,
        "a dead dispatched agent's tree must be adoptable; body was: {:?}",
        outcome.body
    );
    match read_sentinel_owner(&tree) {
        SentinelOwner::Known(id, _) => assert_eq!(id, successor),
        other => panic!("the sentinel must name the adopting session; got {other:?}"),
    }
}

/// #8318, the reported case: after adoption the dead agent's harness lock is
/// gone and the tree's branch is free for the next agent's own worktree.
///
/// Fails before #8318: adoption rewrote only the sentinel, so HEAD stayed on
/// the branch, git refused to check it out anywhere else, and the lock stayed.
/// Test: this function IS the test.
#[tokio::test]
async fn adopt_worktree_route_frees_the_branch_and_clears_a_dead_agents_lock() {
    let mut child = std::process::Command::new("true")
        .spawn()
        .expect("spawn a process that exits immediately");
    let dead_pid = child.id();
    child.wait().expect("reap the child");
    let (fixture, tree) = harness_locked_tree("agent-8318-dead", dead_pid);
    let state = Arc::new(DaemonState::new());

    let outcome = adopt_worktree_core(
        &state,
        AdoptWorktreeRequest {
            path: tree.clone(),
            as_session: ManagedSessionId::new(),
        },
    )
    .await;

    assert_eq!(outcome.status, 200, "body: {:?}", outcome.body);
    let RouteBody::Json(body) = &outcome.body else {
        panic!("a 200 must carry JSON: {:?}", outcome.body);
    };
    assert_eq!(body["branch_release"]["outcome"], "released", "{body}");
    assert_eq!(body["harness_lock"]["outcome"], "cleared", "{body}");
    assert_eq!(
        harness_agent_lock_pid(&tree),
        None,
        "git must report no lock"
    );
    let successor = fixture.repo.join("successor-8318");
    let added = std::process::Command::new("git")
        .arg("-C")
        .arg(&fixture.repo)
        .args(["worktree", "add"])
        .arg(&successor)
        .arg("session/agent-8318-dead")
        .status()
        .expect("run git");
    assert!(
        added.success(),
        "the next agent must be able to check the branch out"
    );
}

/// 🔴 #8318 critic MEDIUM-1: the registry says the owning agent ended, but the
/// harness lock names a pid that is still running. Adoption proceeds on the
/// registry's word, yet the branch stays checked out and the response says
/// which pid holds it. Fails before the fix, which detached HEAD anyway.
#[tokio::test]
async fn adopt_worktree_route_keeps_the_branch_while_the_lock_pid_runs() {
    let running = std::process::id();
    let agent = "agent-8318-lock-running";
    let (_fixture, tree) = harness_locked_tree(agent, running);
    let state = Arc::new(DaemonState::new());
    // The registry's answer: every delegation naming the agent has ended.
    state.upsert_delegation(delegation_for(
        SessionId::new(),
        agent,
        DelegationStatus::Completed,
    ));

    let outcome = adopt_worktree_core(
        &state,
        AdoptWorktreeRequest {
            path: tree.clone(),
            as_session: ManagedSessionId::new(),
        },
    )
    .await;

    assert_eq!(outcome.status, 200, "body: {:?}", outcome.body);
    let RouteBody::Json(body) = &outcome.body else {
        panic!("a 200 must carry JSON: {:?}", outcome.body);
    };
    assert_eq!(body["harness_lock"]["outcome"], "left_running", "{body}");
    assert_eq!(body["branch_release"]["outcome"], "kept", "{body}");
    let reason = body["branch_release"]["reason"].as_str().unwrap_or("");
    assert!(reason.contains(&running.to_string()), "{reason}");
    let head = std::process::Command::new("git")
        .arg("-C")
        .arg(&tree)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .expect("run git");
    assert_eq!(
        String::from_utf8_lossy(&head.stdout).trim(),
        format!("session/{agent}"),
        "a running pid's branch must stay checked out"
    );
}

/// #7974, the arm that must NOT move: the same daemon-restart state, but the
/// lock names a pid that is running. The fallback supplies no death evidence,
/// so ADR-0045's refusal stands and the sentinel is untouched.
/// Test: this function IS the test.
#[tokio::test]
async fn adopt_worktree_route_still_refuses_a_live_owner_after_a_daemon_restart() {
    // This test process is unambiguously running.
    let (_fixture, tree) = harness_locked_tree("agent-still-working", std::process::id());
    let state = Arc::new(DaemonState::new());
    let before = std::fs::read(tree.join(WORKTREE_SENTINEL_FILE)).expect("read sentinel");

    let outcome = adopt_worktree_core(
        &state,
        AdoptWorktreeRequest {
            path: tree.clone(),
            as_session: ManagedSessionId::new(),
        },
    )
    .await;

    assert_eq!(
        outcome.status, 409,
        "a lock naming a RUNNING pid must not permit adoption"
    );
    assert_eq!(
        std::fs::read(tree.join(WORKTREE_SENTINEL_FILE)).expect("read sentinel"),
        before,
        "a refusal must write nothing"
    );
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
