//! Coverage for the slot half of `prune_managed`'s compaction branch (#5897).
//!
//! Why: compaction removed the record and left the slot registry still holding
//! its number, so `numbered_snapshot` — which walks the slots the registry
//! HOLDS and tombstones any whose record has vanished — kept rendering the
//! pruned session as a `-- deleted --` row and `NUM` climbed with every prune.
//! The release is gated on `retention::workspace_needs_protection`, so both of
//! the two paths that free a slot agree on the condition. That gate is the
//! whole design, which is why the arm where it HOLDS gets a test of its own
//! rather than being inferred from the arm where it does not.
//! What: the released case (no workspace on disk → slot gone from the listing
//! and reusable), the protected case (a session-worktree-shaped workspace with
//! its ownership sentinel → record compacted, slot still held, directory
//! untouched), and the dry-run case (nothing released).
//! Test: this file IS the test module; run with `cargo test -p trusty-mpm`.

use std::path::{Path, PathBuf};

use super::super::record::{ManagedSessionId, ManagedSessionState};
use super::super::tests::{make_manager, seed_record};
use super::PruneFilter;

/// A directory shaped like a real SM-provisioned session worktree — the exact
/// fixture `retention_tests::session_worktree` builds, for the same reason: a
/// leaf under `.worktrees` carrying the `.trusty-mpm-worktree` sentinel both
/// provisioners write immediately after their `git worktree add`. `.worktrees`
/// is always in the detection set (#5204), so this is recognised whatever
/// `worktrees_dirname` the host machine has configured.
fn session_worktree(root: &Path, leaf: &str) -> PathBuf {
    let wt = root.join(".worktrees").join(leaf);
    std::fs::create_dir_all(&wt).expect("create worktree dir");
    std::fs::write(
        wt.join(super::super::decommission::WORKTREE_SENTINEL_FILE),
        b"",
    )
    .expect("write sentinel");
    wt
}

/// Every slot the registry currently holds, after observing `mgr`'s records.
async fn slots_held(mgr: &super::SessionManager) -> Vec<u32> {
    mgr.numbered_snapshot(&mgr.list().await)
        .await
        .into_iter()
        .map(|s| s.slot)
        .collect()
}

/// Compacting a tombstone frees its slot, and the number becomes reusable
/// (#5897).
///
/// Why: this is the reported bug. Pre-fix the record left the store but the
/// registry kept the number, so the listing still carried a `-- deleted --` row
/// for it and the next session was assigned a higher `NUM` — prune made the
/// symptom it advertises fixing strictly worse. Asserting on the reuse as well
/// as the disappearance is what distinguishes a released slot from one that is
/// merely hidden; a display filter would pass the first assertion and fail the
/// second.
/// What: seeds a Decommissioned tombstone (which carries no workspace, so the
/// protection guard does not fire) beside a live session, observes both into the
/// registry, prunes `Decommissioned`, then asserts the tombstone's slot is
/// absent from the listing, the survivor's `NUM` did not shift, and a freshly
/// created session is handed the freed number.
/// Test: this function IS the test.
#[tokio::test]
async fn prune_compaction_releases_the_slot() {
    let dir = crate::test_support::hermetic_temp_dir();
    let (mgr, _fake) = make_manager(&dir).await;

    let tombstone = ManagedSessionId::new();
    seed_record(
        &mgr,
        &dir,
        tombstone,
        ManagedSessionState::Decommissioned,
        false,
    )
    .await;
    let keeper = mgr
        .create("keeper".into(), None, None, None, None, None)
        .await
        .expect("create keeper");

    let before = mgr.numbered_snapshot(&mgr.list().await).await;
    let find = |rows: &[super::super::slots::NumberedSlot], id: ManagedSessionId| {
        rows.iter()
            .find(|s| s.record.as_ref().map(|r| r.id) == Some(id))
            .map(|s| s.slot)
    };
    let tombstone_slot = find(&before, tombstone).expect("tombstone observed");
    let keeper_slot = find(&before, keeper.id).expect("keeper observed");

    let outcome = mgr
        .prune_managed(PruneFilter::Decommissioned, false, false, None)
        .await
        .expect("prune decommissioned");
    assert_eq!(outcome.count(), 1, "the tombstone is the only target");

    let after = mgr.numbered_snapshot(&mgr.list().await).await;
    assert!(
        after.iter().all(|s| s.slot != tombstone_slot),
        "the compacted record's slot must leave the listing entirely, not linger \
         as a `-- deleted --` row; still held: {:?}",
        after.iter().map(|s| s.slot).collect::<Vec<_>>()
    );
    assert_eq!(
        find(&after, keeper.id),
        Some(keeper_slot),
        "a surviving session's NUM never shifts"
    );

    // The freed number is handed to the next NEW session — the property the
    // reported `NUM` climb is the absence of.
    let fresh = mgr
        .create("fresh".into(), None, None, None, None, None)
        .await
        .expect("create fresh");
    let reused = mgr.numbered_snapshot(&mgr.list().await).await;
    assert_eq!(
        find(&reused, fresh.id),
        Some(tombstone_slot),
        "the released slot is reusable"
    );
    let mut slots: Vec<u32> = reused.iter().map(|s| s.slot).collect();
    let total = slots.len();
    slots.sort_unstable();
    slots.dedup();
    assert_eq!(slots.len(), total, "no duplicate NUM after reallocation");
}

/// A tombstone whose session worktree is STILL on disk keeps its slot (#5897).
///
/// Why: the guard, not the release, is the design. `workspace_needs_protection`
/// refuses to free a number while the session's `.worktrees/<uuid>` directory
/// and its ownership sentinel are there, because something may still be standing
/// in that tree — handing the number to a newly-spawned session whose worktree
/// already exists is worse than a cosmetic tombstone row, and would weaken
/// #3034's no-silent-reuse property. Without this test the branch that fires is
/// the only one covered, and an "unconditional release" regression would pass
/// the suite.
/// What: seeds a Decommissioned tombstone, repoints its `workspace_path` at a
/// real session-worktree-shaped directory, prunes `Decommissioned`, then asserts
/// the record IS gone from the store, the slot is STILL held (so the row renders
/// as a tombstone with no record), and the directory and its sentinel are
/// untouched — prune's compaction is a store operation and must not become a
/// filesystem deletion by proxy.
/// Test: this function IS the test.
#[tokio::test]
async fn prune_compaction_keeps_the_slot_when_the_worktree_is_still_on_disk() {
    let dir = crate::test_support::hermetic_temp_dir();
    let (mgr, _fake) = make_manager(&dir).await;

    let tombstone = ManagedSessionId::new();
    seed_record(
        &mgr,
        &dir,
        tombstone,
        ManagedSessionState::Decommissioned,
        false,
    )
    .await;

    // `seed_record` gives a Decommissioned record no workspace; give it one that
    // still looks like a live session worktree.
    let wt = session_worktree(dir.path(), &tombstone.to_string());
    let mut record = mgr.get(&tombstone).await.expect("seeded record");
    record.workspace_path = Some(wt.clone());
    mgr.store
        .write()
        .await
        .upsert(record)
        .await
        .expect("repoint workspace_path");

    let before = mgr.numbered_snapshot(&mgr.list().await).await;
    let tombstone_slot = before
        .iter()
        .find(|s| s.record.as_ref().map(|r| r.id) == Some(tombstone))
        .expect("tombstone observed")
        .slot;

    let outcome = mgr
        .prune_managed(PruneFilter::Decommissioned, false, false, None)
        .await
        .expect("prune decommissioned");
    assert_eq!(outcome.count(), 1, "the record is still compacted");
    assert!(
        matches!(
            mgr.get(&tombstone).await,
            Err(super::super::manager::ManagedError::SessionNotFound(_))
        ),
        "compaction still removes the record from the store"
    );

    let after = mgr.numbered_snapshot(&mgr.list().await).await;
    let row = after
        .iter()
        .find(|s| s.slot == tombstone_slot)
        .expect("the protected slot is STILL held — no number is handed out");
    assert!(
        row.record.is_none(),
        "its record is gone, so the row renders as a `-- deleted --` tombstone"
    );

    // The store operation never reaches the filesystem.
    assert!(wt.is_dir(), "prune must not remove the worktree directory");
    assert!(
        wt.join(super::super::decommission::WORKTREE_SENTINEL_FILE)
            .is_file(),
        "prune must not remove the ownership sentinel"
    );
}

/// A dry-run prune releases nothing (#5897).
///
/// Why: `dry_run` promises that NOTHING is mutated, and the slot registry is
/// state a caller can observe through `tm session ls`. The release sits inside
/// the `if !dry_run` block; this pins that placement so a later refactor cannot
/// lift it out without a failing test.
/// What: seeds a tombstone, records the held slots, dry-run prunes
/// `Decommissioned`, asserts the held set is byte-identical and the record is
/// still in the store.
/// Test: this function IS the test.
#[tokio::test]
async fn prune_dry_run_releases_no_slot() {
    let dir = crate::test_support::hermetic_temp_dir();
    let (mgr, _fake) = make_manager(&dir).await;

    let tombstone = ManagedSessionId::new();
    seed_record(
        &mgr,
        &dir,
        tombstone,
        ManagedSessionState::Decommissioned,
        false,
    )
    .await;
    let before = slots_held(&mgr).await;

    mgr.prune_managed(PruneFilter::Decommissioned, true, false, None)
        .await
        .expect("dry-run prune");

    assert_eq!(slots_held(&mgr).await, before, "dry-run holds every slot");
    assert!(
        mgr.get(&tombstone).await.is_ok(),
        "dry-run leaves the record in the store"
    );
}
