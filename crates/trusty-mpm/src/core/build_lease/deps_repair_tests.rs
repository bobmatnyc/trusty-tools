//! #10212: the operator's `--rotate-deps` pass and the daemon reclaim.
//!
//! Every test works in a temp dir, with a temp lease store; none touches a
//! real cargo target or the operator's leases.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use super::super::deps_budget::DepsCount;
use super::super::deps_rotate::PROFILE;
use super::super::deps_rotate::tests::{
    DEAD_PID, aside_in, deps_len, hold_cargo_lock, target_with,
};
use super::*;

/// A shared root, a pool root and a lease store: one shared target with
/// `shared_n` deps entries and slot 0 with `slot_n`.
fn roots(shared_n: usize, slot_n: usize) -> (tempfile::TempDir, PathBuf, PathBuf, SlotDir) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let shared = tmp.path().join("cargo-target");
    let pool = tmp.path().join("pool");
    target_with(&shared.join("o/r"), shared_n);
    target_with(&pool.join("o/r/slot-0"), slot_n);
    let store = SlotDir::at(tmp.path().join("store")).expect("lease store");
    (tmp, shared, pool, store)
}

#[test]
fn the_repair_rotates_an_over_line_target() {
    let (_tmp, shared, pool, store) = roots(6, 6);
    let rows = rotate_all(&shared, &pool, Ok(&store), 5, true);
    let slot = pool.join("o/r/slot-0");
    for target in [shared.join("o/r"), slot.clone()] {
        assert!(
            rows.iter()
                .any(|(t, r)| *t == target && matches!(r, Rotation::Rotated { deleted: true, .. })),
            "{target:?}: {rows:?}"
        );
        assert!(!target.join(PROFILE).exists(), "{target:?}");
        let left: Vec<_> = std::fs::read_dir(&target)
            .expect("target")
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".evicting."))
            .collect();
        assert!(
            left.is_empty(),
            "the aside tree was deleted before the pass returned: {left:?}"
        );
    }
}

#[test]
fn the_repair_leaves_an_under_line_slot() {
    let (_tmp, shared, pool, store) = roots(0, 2);
    let slot = pool.join("o/r/slot-0");
    let rows = rotate_all(&shared, &pool, Ok(&store), 5, true);
    assert!(
        rows.contains(&(slot.clone(), Rotation::NotNeeded(DepsCount::Under(2)))),
        "{rows:?}"
    );
    assert_eq!(deps_len(&slot), 2);
}

#[test]
fn the_repair_refuses_a_target_whose_cargo_lock_is_held() {
    let (_tmp, shared, pool, store) = roots(6, 0);
    let shared_target = shared.join("o/r");
    let _held = hold_cargo_lock(&shared_target);
    let rows = rotate_all(&shared, &pool, Ok(&store), 5, true);
    assert!(
        rows.iter()
            .any(|(t, r)| *t == shared_target && matches!(r, Rotation::Busy(_))),
        "{rows:?}"
    );
    assert_eq!(deps_len(&shared_target), 6);
}

/// #10212 round 1 (critic HIGH): cargo drops `.cargo-lock` between the
/// commands of one leased run, so a held build lease alone must spare the slot.
#[test]
fn the_repair_spares_a_slot_whose_lease_is_held() {
    let (_tmp, shared, pool, store) = roots(0, 6);
    let slot = pool.join("o/r/slot-0");
    let _lease = store.try_acquire(0).expect("lock").expect("free");
    let rows = rotate_all(&shared, &pool, Ok(&store), 5, true);
    assert!(
        rows.iter()
            .any(|(t, r)| *t == slot && matches!(r, Rotation::Busy(h) if h.contains("lease"))),
        "{rows:?}"
    );
    assert_eq!(deps_len(&slot), 6, "the leased slot is untouched");
}

/// Fail-Open Check: an unusable lease store spares every slot, never rotates.
#[test]
fn an_unusable_lease_store_spares_every_slot() {
    let (_tmp, shared, pool, _store) = roots(0, 6);
    let slot = pool.join("o/r/slot-0");
    let rows = rotate_all(&shared, &pool, Err("EACCES"), 5, true);
    assert!(
        rows.iter()
            .any(|(t, r)| *t == slot && matches!(r, Rotation::Busy(h) if h.contains("EACCES"))),
        "{rows:?}"
    );
    assert_eq!(deps_len(&slot), 6);
}

/// Fail-Open Check: the repair reports an aside tree it could not delete.
#[test]
fn the_repair_reports_an_aside_tree_it_cannot_delete() {
    let (_tmp, shared, pool, store) = roots(0, 0);
    let slot = pool.join("o/r/slot-0");
    let tree = aside_in(&slot, DEAD_PID, 1);
    let locked = tree.join("deps");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).expect("chmod");
    let rows = rotate_all(&shared, &pool, Ok(&store), 5, true);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    assert!(
        rows.iter().any(
            |(_, r)| matches!(r, Rotation::DeleteFailed { tree: t, .. } if *t == tree)
                && r.is_failure()
        ),
        "{rows:?}"
    );
    assert!(tree.exists(), "left for the next pass");
}

/// #10212 round 1: the tick's reclaim stops at a cancel, across targets too.
#[test]
fn a_cancelled_tick_reclaims_nothing() {
    let (_tmp, shared, pool, _store) = roots(0, 0);
    let tree = aside_in(&pool.join("o/r/slot-0"), DEAD_PID, 1);
    let reclaim = reclaim_everywhere(&shared, &pool, &|| true);
    assert!(reclaim.cancelled, "{reclaim:?}");
    assert_eq!(reclaim.removed, 0);
    assert!(tree.exists());
}
