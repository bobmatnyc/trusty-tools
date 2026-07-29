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
    let mgr = SessionManager::new(store_dir.path(), FakeTmuxDriver::new())
        .await
        .expect("manager");

    // #4207: both worktrees are REAL `git worktree add`s. Discovery is now
    // derived from git's registry, so a `mkdir` is not a candidate and the
    // orphan half of this test would pass vacuously without them.
    let fx = super::worktree_git_fixture::GitWorktreeFixture::new();
    let live_wt = fx.add_worktree("live-session");
    let orphan_wt = fx.add_worktree("orphan-session");

    // #3649: the auto-reaper now NEVER deletes an owner-unknown worktree (a
    // dir with no sentinel, or a legacy zero-byte one). Write a valid
    // ownership sentinel naming an id that resolves to NO session record at
    // all, so `resolve_ownerless_with_grace` treats it as provably ownerless
    // and the reaper still reclaims it — preserving this test's original
    // "orphan with no matching record is removed" intent under the new
    // ownership model. #3649 review fix: the sentinel's `created_at` must be
    // OLDER than `OWNERLESS_GRACE`, or a never-registered-but-freshly-stamped
    // owner is (correctly) treated as a creation race and spared, not reclaimed.
    let aged = chrono::Utc::now()
        - super::worktree_ownership::OWNERLESS_GRACE
        - chrono::Duration::minutes(1);
    std::fs::write(
        orphan_wt.join(super::decommission::WORKTREE_SENTINEL_FILE),
        serde_json::to_vec(&super::worktree_ownership::WorktreeSentinel {
            owner_session_id: ManagedSessionId::new(),
            created_at: aged,
        })
        .expect("serialize aged sentinel"),
    )
    .expect("write ownerless sentinel");

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

    let outcome = mgr
        .reap_orphaned_worktrees(&fx.repos_root)
        .await
        .expect("reap must not error");
    let removed = outcome.removed;

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
    assert!(
        outcome.owner_unknown.is_empty(),
        "no owner-unknown candidates expected in this fixture; got {:?}",
        outcome.owner_unknown
    );
}

/// #4288: the AUTOMATIC GC path spares a `Stopped` record's workspace — its
/// active-set construction must stay UNFILTERED by record state.
///
/// Why: this is the sibling of `prune_spares_a_stopped_records_workspace`
/// (which pins the manual HTTP route). `reap_orphaned_worktrees` is what the
/// daemon's orphan-GC loop calls on a timer; it hardcodes `dry_run: false` and
/// has no preview mode, so it is the path that would destroy a live worktree
/// unattended. Session state is not a liveness signal: a session measured
/// still running in tmux was recorded `stopped` while holding 12 modified
/// files, 31 untracked files, and an unpushed commit.
///
/// WHAT THIS TEST DOES AND DOES NOT CATCH — measured, not assumed. There are
/// TWO deliberately-unfiltered reads on this path: the caller-supplied set
/// built in `reap_orphaned_worktrees`, and the Phase 2 `fresh_active` snapshot
/// that `prune_orphaned_worktrees` re-reads from the store immediately before
/// deleting. They are defense-in-depth, and NEITHER is load-bearing alone:
/// narrowing only the reap-side set leaves the worktree a candidate that
/// Phase 2 still spares; narrowing only Phase 2 leaves the worktree out of the
/// candidate list entirely. This test therefore stays GREEN under either
/// single narrowing and goes RED when BOTH are narrowed — verified in both
/// directions. It pins the PAIR, which is the property that actually keeps a
/// live worktree on disk; it is not a tripwire on either read in isolation.
///
/// Non-vacuity: the fixture carries an aged, well-formed ownership sentinel
/// naming a NEVER-REGISTERED owner, so every downstream gate already votes
/// "reclaim". That detail is load-bearing and not incidental — a sentinel
/// naming the stopped session ITSELF would be spared at the #3649 gate
/// (`resolve_ownerless_with_grace` treats `Stopped` as live/resumable, NOT
/// terminal), so such a fixture would survive the mutation too and pin
/// nothing. The CONTROL below asserts reclaimability against an EMPTY active
/// set, so if a future change makes this fixture unreclaimable for an
/// unrelated reason the control fails loudly instead of letting the real
/// assertions pass for the wrong reason. Active-set membership is therefore
/// the ONLY thing under test.
/// Test: this function IS the test.
#[tokio::test]
async fn reap_spares_a_stopped_records_workspace() {
    use super::record::{ManagedSessionState, SessionRecord};
    use super::worktree_git_fixture::GitWorktreeFixture;

    let store_dir = TempDir::new().unwrap();
    let mgr = SessionManager::new(store_dir.path(), FakeTmuxDriver::new())
        .await
        .expect("manager");

    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("stopped-but-live");
    GitWorktreeFixture::stamp_reclaimable_sentinel(&wt);

    // CONTROL: with an EMPTY active set this fixture IS reclaimable. Dry-run,
    // so nothing is deleted before the real assertions run.
    let control = mgr
        .prune_orphaned_worktrees(&fx.repos_root, &[], true, super::DirtyWorktreePolicy::Skip)
        .await
        .expect("control sweep must not error");
    assert!(
        control.removed.contains(&wt),
        "CONTROL: {} must be reclaimable with an empty active set, otherwise \
         this test proves nothing; removed={:?} owner_unknown={:?} skipped_dirty={:?}",
        wt.display(),
        control.removed,
        control.owner_unknown,
        control.skipped_dirty
    );

    // Register the worktree to a record and drive it to `Stopped` — the exact
    // shape observed on a session that was in fact still running.
    let record = mgr
        .create_with_id(
            ManagedSessionId::new(),
            "pinned by #4288".into(),
            Some(wt.clone()),
            None,
            Some(wt.clone()),
            None,
            None,
            crate::runtime::RuntimeKind::default(),
            false,
            false,
        )
        .await
        .expect("seed session record");
    let stopped = SessionRecord {
        state: ManagedSessionState::Stopped,
        ..record
    };
    mgr.store
        .write()
        .await
        .upsert(stopped)
        .await
        .expect("persist the Stopped record");

    // The automatic sweep always really deletes (`dry_run: false` is hardcoded),
    // so this is the destructive path, not a preview.
    let outcome = mgr
        .reap_orphaned_worktrees(&fx.repos_root)
        .await
        .expect("reap must not error");

    assert!(
        !outcome.removed.contains(&wt),
        "a Stopped record's workspace must NOT be removed by the automatic \
         orphan-GC sweep; {} appeared in the removed set {:?}",
        wt.display(),
        outcome.removed
    );
    assert!(
        !outcome.owner_unknown.contains(&wt),
        "a spared workspace is never even a candidate, so it must not be \
         reported as owner-unknown either; got {:?}",
        outcome.owner_unknown
    );
    assert!(
        wt.exists(),
        "{} must still exist on disk after the automatic sweep",
        wt.display()
    );
}
