//! The pause's orphan prune is bounded to the paused project (#8782).
//!
//! Every repository is a `GitWorktreeFixture` in its own temp dir; the second
//! project reaches the scan as an ADOPTED checkout. The repos root and the
//! adopted anchors are injected, so no test reads the operator's workspace.

use super::prune_for_pause;
use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;
use crate::session_manager::worktree_scope::WorktreeScope;
use crate::session_manager::{DirtyWorktreePolicy, SessionManager};

/// 🔴 #8782: a pause in project A removes nothing in project B.
///
/// Fails when `prune_for_pause` sweeps under `WorktreeScope::all()`: B's
/// orphan — which the CONTROL shows the global prune would remove — is then
/// deleted by a pause typed in A.
#[tokio::test]
async fn a_pause_prunes_only_its_own_projects_orphans() {
    let (a, b) = (GitWorktreeFixture::new(), GitWorktreeFixture::new());
    let wt_a = a.add_worktree("pause-a-8782");
    let wt_b = b.add_worktree("pause-b-8782");
    for wt in [&wt_a, &wt_b] {
        GitWorktreeFixture::stamp_reclaimable_sentinel(wt);
    }
    let store = tempfile::tempdir().expect("store dir");
    let mgr = SessionManager::new(
        store.path(),
        crate::session_manager::tests::FakeTmuxDriver::new(),
    )
    .await
    .expect("manager");
    let adopted = vec![b.repo.clone()];

    // CONTROL (dry run): the unscoped prune reaches both projects' orphans.
    let global = mgr
        .prune_orphaned_worktrees_in(
            &a.repos_root,
            &[],
            true,
            DirtyWorktreePolicy::Skip,
            &adopted,
            &WorktreeScope::all(),
        )
        .await
        .expect("control sweep");
    assert!(
        global.removed.contains(&wt_a) && global.removed.contains(&wt_b),
        "CONTROL: both orphans must be reclaimable, or this test proves nothing: {global:?}"
    );

    let (removed, skipped) = prune_for_pause(&mgr, &a.repos_root, &adopted, &a.repo).await;
    assert!(skipped.is_empty(), "{skipped:?}");
    assert_eq!(removed, vec![wt_a.to_string_lossy().into_owned()]);
    assert!(!wt_a.exists(), "the paused project's orphan is reclaimed");
    assert!(wt_b.exists(), "a pause in A removed B's orphan");
}

/// #8782: a pause outside any git checkout prunes nothing rather than widening.
#[tokio::test]
async fn a_pause_outside_a_checkout_prunes_nothing() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("pause-nowhere-8782");
    GitWorktreeFixture::stamp_reclaimable_sentinel(&wt);
    let store = tempfile::tempdir().expect("store dir");
    let mgr = SessionManager::new(
        store.path(),
        crate::session_manager::tests::FakeTmuxDriver::new(),
    )
    .await
    .expect("manager");
    let elsewhere = tempfile::tempdir().expect("non-repo dir");
    let (removed, _) = prune_for_pause(&mgr, &fx.repos_root, &[], elsewhere.path()).await;
    assert!(removed.is_empty(), "{removed:?}");
    assert!(wt.exists());
}
