//! Wired-in coverage for the #3764 item-1 cross-session worktree-deletion guard.
//!
//! Why: `workspace_guard::foreign_active_owner`'s unit tests pin the pure
//! decision matrix, but the bug being fixed is a WIRING bug — the guard has to
//! actually fire inside `decommission`, on the `caller: None` path that every
//! daemon-routed remove site uses (`daemon/mcp_session.rs`,
//! `daemon/sm_stdio/control.rs`, the HTTP routes, `daemon/idle_reaper.rs`,
//! `session_manager/dedup.rs`). A guard that exists but is never consulted is
//! exactly the "shipped guard that detects nothing" shape #3764 was filed over,
//! so these tests drive the real `decommission_with_root` entry point against a
//! real on-disk sentinel and assert on OBSERVABLE FILESYSTEM STATE, not just on
//! the returned error.
//! What: the refusal case (record points at a live peer's worktree), the
//! must-not-regress case (a session removing its own worktree), and the
//! must-not-weaken case (a peer in a non-Active state is still reclaimable).
//! Test: this file IS the test.

use super::decommission::WORKTREE_SENTINEL_FILE;
use super::manager::{ManagedError, SessionManager};
use super::record::{ManagedSessionId, ManagedSessionState, SessionRecord};
use super::worktree_ownership::WorktreeSentinel;

/// Build an Active record whose `workspace_path` is `ws`.
///
/// Why: the incident shape needs TWO records pointing at ONE path, so the
/// workspace path is a parameter rather than derived from the id.
fn record_at(
    id: ManagedSessionId,
    state: ManagedSessionState,
    ws: Option<std::path::PathBuf>,
) -> SessionRecord {
    SessionRecord {
        id,
        tmux_name: format!("tm-3764-{id}"),
        cwd: std::path::PathBuf::from("/tmp"),
        task: "task".into(),
        state,
        created_at: chrono::Utc::now(),
        last_activity_at: None,
        workspace_path: ws,
        repo_url: None,
        branch: None,
        pending_decision: None,
        proposed_default: None,
        correlation: Default::default(),
        runtime: Default::default(),
        ephemeral: false,
        // `true` deliberately: the OWNED branch of `decommission` is the one
        // that calls `remove_dir_all` directly, so this is the strictest
        // possible setting for a test that must prove nothing was deleted.
        workspace_owned: true,
        source_id: None,
        claude_session_id: None,
        scrollback_path: None,
        last_cwd: None,
        deliverable_id: None,
        pane_id: None,
        injection_status: Default::default(),
        // Deliberately `None`: the impostor record does NOT name an owner, so
        // the #3649 gate cannot fire and only the #3764 guard is under test.
        worktree_owner: None,
    }
}

/// Create a worktree directory whose on-disk ownership sentinel names `owner`.
fn worktree_owned_by(root: &std::path::Path, owner: ManagedSessionId) -> std::path::PathBuf {
    let ws = root
        .join("owner")
        .join("repo")
        .join(".worktrees")
        .join("wt");
    std::fs::create_dir_all(&ws).expect("create worktree dir");
    std::fs::write(
        ws.join(WORKTREE_SENTINEL_FILE),
        serde_json::to_vec(&WorktreeSentinel::new(owner)).expect("serialize sentinel"),
    )
    .expect("write sentinel");
    // A real file inside the tree, so "the tree survived" is a positive
    // assertion about CONTENT and not merely about the directory entry.
    std::fs::write(ws.join("live-work.txt"), b"peer's uncommitted work").expect("write work file");
    ws
}

/// Decommissioning a record whose `workspace_path` points at a LIVE peer's
/// worktree is refused, and the peer's files survive untouched.
///
/// Why (#3764): this is the observed incident shape — a #1744 cwd collision
/// left three Active records pointing at ONE worktree path. Before this guard,
/// `is_safe_to_remove` passed (the path IS inside the managed root), the
/// `caller: None` path skipped the #3649 gate entirely, and `remove_dir_all`
/// destroyed the live owner's tree while that session kept running.
/// Test: this function IS the test.
#[tokio::test]
async fn decommission_refuses_to_delete_live_peer_worktree() {
    let dir = crate::test_support::hermetic_temp_dir();
    let mgr = SessionManager::new(dir.path(), super::tests::FakeTmuxDriver::new())
        .await
        .expect("manager");

    let managed_root = crate::test_support::hermetic_temp_dir();
    let peer = ManagedSessionId::new();
    let ws = worktree_owned_by(managed_root.path(), peer);

    // The real owner: Active, and it is the session the sentinel names.
    mgr.store
        .write()
        .await
        .upsert(record_at(
            peer,
            ManagedSessionState::Active,
            Some(ws.clone()),
        ))
        .await
        .expect("upsert peer");

    // The impostor: a second record pointing at the SAME path (the collision).
    let impostor = ManagedSessionId::new();
    mgr.store
        .write()
        .await
        .upsert(record_at(
            impostor,
            ManagedSessionState::Active,
            Some(ws.clone()),
        ))
        .await
        .expect("upsert impostor");

    // `caller: None` — operator/daemon-internal authority, the path taken by
    // every real daemon remove site and the one #3649's gate does not cover.
    let err = mgr
        .decommission_with_root(&impostor, managed_root.path(), None)
        .await
        .expect_err("decommission must refuse to delete a live peer's worktree");

    match err {
        ManagedError::ForeignActiveWorktree(target, owner, path) => {
            assert_eq!(target, impostor, "target must be the impostor record");
            assert_eq!(
                owner, peer,
                "owner must be the live peer named by the sentinel"
            );
            assert_eq!(path, ws.display().to_string());
        }
        other => panic!("expected ForeignActiveWorktree, got {other:?}"),
    }

    // The peer's work must still be on disk, byte for byte.
    assert!(
        ws.join("live-work.txt").exists(),
        "the live peer's worktree contents must survive a refused decommission"
    );
    assert_eq!(
        std::fs::read(ws.join("live-work.txt")).expect("read work file"),
        b"peer's uncommitted work",
    );

    // Neither record may be tombstoned — the impostor is still Active, so the
    // operator can see and fix the collision instead of it being erased.
    assert_eq!(
        mgr.get(&impostor).await.expect("get impostor").state,
        ManagedSessionState::Active,
    );
    assert_eq!(
        mgr.get(&peer).await.expect("get peer").state,
        ManagedSessionState::Active,
    );
}

/// A session decommissioning its OWN worktree is unaffected by the guard.
///
/// Why: the guard must not break the normal teardown path. This is the
/// regression half — without it, a guard that refused everything would also
/// pass the refusal test above.
/// Test: this function IS the test.
#[tokio::test]
async fn decommission_removes_own_worktree_despite_guard() {
    let dir = crate::test_support::hermetic_temp_dir();
    let mgr = SessionManager::new(dir.path(), super::tests::FakeTmuxDriver::new())
        .await
        .expect("manager");

    let managed_root = crate::test_support::hermetic_temp_dir();
    let id = ManagedSessionId::new();
    let ws = worktree_owned_by(managed_root.path(), id);

    mgr.store
        .write()
        .await
        .upsert(record_at(id, ManagedSessionState::Active, Some(ws.clone())))
        .await
        .expect("upsert");

    let (record, removed) = mgr
        .decommission_with_root(&id, managed_root.path(), None)
        .await
        .expect("a session must be able to decommission its own worktree");

    assert!(
        removed,
        "the owned workspace must actually have been removed"
    );
    assert!(!ws.exists(), "the session's own worktree must be gone");
    assert_eq!(record.state, ManagedSessionState::Decommissioned);
}

/// A provably-ownerless (terminal) peer does not block reclamation.
///
/// Why: #3649's orphan-GC and every operator-driven reclaim path depend on
/// being able to tear down a worktree whose owner is terminal or gone. The
/// guard must narrow to peers that are NOT provably ownerless — this test pins
/// that it does not freeze the existing GC.
///
/// This replaces an earlier `..._when_peer_is_stopped` test that asserted the
/// OPPOSITE for a Stopped peer. That assertion encoded the HIGH-1 bug: a
/// Stopped session is resumable, so its worktree must be protected, not
/// reclaimed. Terminal is the correct boundary, and it is also the operator's
/// escape hatch — `tm sessions delete <stale-id>` marks a record `Deleted`,
/// which is terminal, which releases the guard.
/// Test: this function IS the test.
#[tokio::test]
async fn decommission_allows_reclaim_when_peer_is_terminal() {
    let dir = crate::test_support::hermetic_temp_dir();
    let mgr = SessionManager::new(dir.path(), super::tests::FakeTmuxDriver::new())
        .await
        .expect("manager");

    let managed_root = crate::test_support::hermetic_temp_dir();
    let peer = ManagedSessionId::new();
    let ws = worktree_owned_by(managed_root.path(), peer);

    mgr.store
        .write()
        .await
        .upsert(record_at(peer, ManagedSessionState::Decommissioned, None))
        .await
        .expect("upsert terminal peer");

    let reclaimer = ManagedSessionId::new();
    mgr.store
        .write()
        .await
        .upsert(record_at(
            reclaimer,
            ManagedSessionState::Active,
            Some(ws.clone()),
        ))
        .await
        .expect("upsert reclaimer");

    let (_record, removed) = mgr
        .decommission_with_root(&reclaimer, managed_root.path(), None)
        .await
        .expect("a terminal peer must not block reclamation");
    assert!(removed, "reclamation must still remove the workspace");
    assert!(!ws.exists(), "the reclaimed worktree must be gone");
}

/// A STOPPED peer's worktree is refused — the HIGH-1 regression.
///
/// Why (code-critic HIGH-1): the first draft refused only for an `Active`
/// owner, which made this `caller: None` guard strictly WEAKER than the #3649
/// gate twenty lines earlier in the same function — that gate uses
/// `resolve_ownerless`, which returns `false` for `Stopped`. The result was
/// backwards: `caller: Some` protected a Stopped peer while `caller: None` —
/// every daemon path this guard exists to cover — deleted it.
///
/// The reachable sequence in shipped code: `idle_reaper` stops peer P at the
/// idle threshold; later it reaps colliding record I at the done threshold via
/// `decommission(&I, None)`; the sentinel names P; P is Stopped; the guard
/// passed; `remove_session_worktree` destroyed P's RESUMABLE tree. A stopped
/// session's worktree is exactly the one that must survive — it is going to be
/// resumed.
/// Test: this function IS the test.
#[tokio::test]
async fn decommission_refuses_to_delete_stopped_peer_worktree() {
    let dir = crate::test_support::hermetic_temp_dir();
    let mgr = SessionManager::new(dir.path(), super::tests::FakeTmuxDriver::new())
        .await
        .expect("manager");

    let managed_root = crate::test_support::hermetic_temp_dir();
    let peer = ManagedSessionId::new();
    let ws = worktree_owned_by(managed_root.path(), peer);

    // The peer is STOPPED — runtime not running, but workspace INTACT and
    // RESUMABLE (see `ManagedSessionState::Stopped`).
    mgr.store
        .write()
        .await
        .upsert(record_at(
            peer,
            ManagedSessionState::Stopped,
            Some(ws.clone()),
        ))
        .await
        .expect("upsert stopped peer");

    let impostor = ManagedSessionId::new();
    mgr.store
        .write()
        .await
        .upsert(record_at(
            impostor,
            ManagedSessionState::Active,
            Some(ws.clone()),
        ))
        .await
        .expect("upsert impostor");

    let err = mgr
        .decommission_with_root(&impostor, managed_root.path(), None)
        .await
        .expect_err("a STOPPED peer's resumable worktree must not be deleted");
    match err {
        ManagedError::ForeignActiveWorktree(target, owner, _path) => {
            assert_eq!(target, impostor);
            assert_eq!(owner, peer);
        }
        other => panic!("expected ForeignActiveWorktree, got {other:?}"),
    }

    assert!(
        ws.join("live-work.txt").exists(),
        "the stopped peer's work must survive — it is going to be resumed"
    );
    assert_eq!(
        mgr.get(&peer).await.expect("get peer").state,
        ManagedSessionState::Stopped,
    );
}

/// The guard holds with NO sentinel at all, purely from store state.
///
/// Why (code-critic MEDIUM): the sentinel is a plain file inside the worktree
/// that any agent in any session can write or delete, and pre-#3649 worktrees
/// carry a zero-byte one naming nobody. On exactly those legacy paths the
/// sentinel half is silently inert. The store-side check is unforgeable — two
/// live records bound to one canonical path IS the #1744 collision shape — and
/// this test removes the sentinel entirely to prove the guard does not depend
/// on it.
/// Test: this function IS the test.
#[tokio::test]
async fn decommission_refuses_when_store_shows_live_peer_without_sentinel() {
    let dir = crate::test_support::hermetic_temp_dir();
    let mgr = SessionManager::new(dir.path(), super::tests::FakeTmuxDriver::new())
        .await
        .expect("manager");

    let managed_root = crate::test_support::hermetic_temp_dir();
    let peer = ManagedSessionId::new();
    let ws = worktree_owned_by(managed_root.path(), peer);
    // Legacy / tampered worktree: no ownership sentinel whatsoever.
    std::fs::remove_file(ws.join(WORKTREE_SENTINEL_FILE)).expect("remove sentinel");

    mgr.store
        .write()
        .await
        .upsert(record_at(
            peer,
            ManagedSessionState::Active,
            Some(ws.clone()),
        ))
        .await
        .expect("upsert peer");

    let impostor = ManagedSessionId::new();
    mgr.store
        .write()
        .await
        .upsert(record_at(
            impostor,
            ManagedSessionState::Active,
            Some(ws.clone()),
        ))
        .await
        .expect("upsert impostor");

    let err = mgr
        .decommission_with_root(&impostor, managed_root.path(), None)
        .await
        .expect_err("the store-side check must refuse even with no sentinel on disk");
    match err {
        ManagedError::ForeignActiveWorktree(_, owner, _) => assert_eq!(owner, peer),
        other => panic!("expected ForeignActiveWorktree, got {other:?}"),
    }
    assert!(
        ws.join("live-work.txt").exists(),
        "the peer's work must survive a sentinel-less refusal"
    );
}

/// A record-only collapse — no filesystem removal — is NOT blocked, even when
/// a live peer shares the path.
///
/// Why: `decommission` only deletes a workspace when it is SM-owned or an
/// in-project `.worktrees/` slice; otherwise it warns and merely tombstones the
/// record. Guarding that case is pure over-refusal, and it breaks `dedup`, whose
/// job is collapsing a dead duplicate that BY CONSTRUCTION shares a
/// `workspace_path` with the record it deduplicates against — the reconciliation
/// this guard actually wants an operator to perform. Scoping the guard to
/// destructive removals keeps it exactly coextensive with the harm it prevents.
///
/// Caught by `reconcile_dedup_collapses_exact_workspace_duplicate_of_live_record`
/// and `..._dead_duplicate_of_owned_record` when the guard was first wired
/// unscoped; this test pins the boundary directly so the reason survives.
/// Test: this function IS the test.
#[tokio::test]
async fn decommission_allows_record_only_collapse_sharing_a_path() {
    let dir = crate::test_support::hermetic_temp_dir();
    let mgr = SessionManager::new(dir.path(), super::tests::FakeTmuxDriver::new())
        .await
        .expect("manager");

    // A PLAIN directory: not SM-owned, not under `.worktrees/` — so
    // `decommission` will not touch the filesystem at all.
    let plain = crate::test_support::hermetic_temp_dir();
    let ws = plain.path().join("proj").join("checkout");
    std::fs::create_dir_all(&ws).expect("create plain workspace");
    std::fs::write(ws.join("work.txt"), b"user work").expect("write");

    let live = ManagedSessionId::new();
    let mut live_rec = record_at(live, ManagedSessionState::Active, Some(ws.clone()));
    live_rec.workspace_owned = false;
    mgr.store
        .write()
        .await
        .upsert(live_rec)
        .await
        .expect("upsert live");

    let stale = ManagedSessionId::new();
    let mut stale_rec = record_at(stale, ManagedSessionState::Stopped, Some(ws.clone()));
    stale_rec.workspace_owned = false;
    mgr.store
        .write()
        .await
        .upsert(stale_rec)
        .await
        .expect("upsert stale");

    let managed_root = crate::test_support::hermetic_temp_dir();
    let (record, removed) = mgr
        .decommission_with_root(&stale, managed_root.path(), None)
        .await
        .expect("a record-only collapse must not be blocked by the worktree guard");

    assert!(!removed, "no filesystem removal should have occurred");
    assert_eq!(record.state, ManagedSessionState::Decommissioned);
    assert!(
        ws.join("work.txt").exists(),
        "the shared directory and its contents must be untouched"
    );
    assert_eq!(
        mgr.get(&live).await.expect("get live").state,
        ManagedSessionState::Active,
        "the live record must survive the collapse"
    );
}
