//! Integration test: the auto orphan-worktree reaper removes orphaned worktree
//! dirs but PRESERVES live ones (#1838).
//!
//! Why: #1838 — the managed-clone `.worktrees/<id>` tree grew to 94 dirs for one
//! project because nothing ran the orphan sweep automatically.
//! [`SessionManager::reap_orphaned_worktrees`] is the convenience entry point the
//! daemon's orphan-GC loop now calls each tick; this test locks in the
//! safety-critical invariant that it removes a dir with NO matching session record
//! while NEVER touching one backed by a live record.
//! What: builds a repos root with two `<owner>/<repo>/.worktrees/<id>` dirs — one
//! backed by a live `SessionManager` record, one orphaned — runs
//! `reap_orphaned_worktrees`, and asserts the orphan is gone and the live dir
//! survives (both on disk and in the returned removed-set).
//! Test: this file IS the test module; run with `cargo test -p trusty-mpm`.

use tempfile::TempDir;

use super::manager::SessionManager;
use super::record::ManagedSessionId;
use super::tests::FakeTmuxDriver;

/// `reap_orphaned_worktrees` removes a `.worktrees/<id>` dir with no session
/// record but PRESERVES one whose path is still a live record (#1838).
///
/// Why: this is the exact mirror-of-safety the task requires — the reaper must
/// identify and remove an orphaned dir with no matching session record while
/// preserving a dir that still has a live/valid session record. It exercises the
/// full convenience path (`reap_orphaned_worktrees` → active-set snapshot →
/// `prune_orphaned_worktrees`) rather than the low-level helper, so it covers the
/// new #1838 auto-reap entry point end-to-end.
/// What: creates two real worktree dirs, registers a live record pointing at one,
/// runs the reaper, and asserts the orphan is deleted, the live dir remains, and
/// the returned removed-set names only the orphan.
/// Test: this function IS the test.
#[tokio::test]
async fn reap_orphaned_worktrees_removes_orphan_preserves_live() {
    let store_dir = TempDir::new().unwrap();
    let repos = TempDir::new().unwrap();
    let mgr = SessionManager::new(store_dir.path(), FakeTmuxDriver::new())
        .await
        .expect("manager");

    let wt_root = repos.path().join("owner").join("repo").join(".worktrees");
    let live_wt = wt_root.join("live-session");
    let orphan_wt = wt_root.join("orphan-session");
    std::fs::create_dir_all(&live_wt).unwrap();
    std::fs::create_dir_all(&orphan_wt).unwrap();

    // Register a live session whose workspace_path is the live worktree. Marked
    // unowned (in-project worktree, not a full clone), mirroring real sessions.
    let _record = mgr
        .create_with_id(
            ManagedSessionId::new(),
            "task".into(),
            Some(live_wt.clone()),
            None,
            Some(live_wt.clone()),
            None,
            None,
            crate::runtime::RuntimeKind::default(),
            false,
            false,
        )
        .await
        .expect("create live record");

    let removed = mgr
        .reap_orphaned_worktrees(repos.path())
        .await
        .expect("reap must not error");

    // The orphan (no matching record) must be removed from disk.
    assert!(
        !orphan_wt.exists(),
        "orphaned worktree dir must be removed by the auto-reaper (#1838)"
    );
    // The live worktree (backed by a record) must survive on disk.
    assert!(
        live_wt.exists(),
        "live session worktree must be preserved by the auto-reaper (#1838)"
    );
    // The removed-set must name the orphan and NOT the live dir.
    assert!(
        removed.iter().any(|p| p.ends_with("orphan-session")),
        "removed set must include the orphan; got {removed:?}"
    );
    assert!(
        !removed.iter().any(|p| p.ends_with("live-session")),
        "removed set must NOT include the live worktree; got {removed:?}"
    );
}
