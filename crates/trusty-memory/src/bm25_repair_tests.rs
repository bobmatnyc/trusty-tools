//! Unit tests for [`super`] — the BM25 coverage repair sweep.
//!
//! Why: split out of `bm25_repair.rs` to keep the production module under the
//! 500-SLOC cap, wired back in via `#[path] mod tests;`.
//! What: covers the dirty-set contract, the interval knob, and the repair
//! pass's two terminal branches — a palace that is gone is dropped, a palace
//! whose coverage is still unverified stays queued.
//! Test: this *is* the test file.

use super::*;
use trusty_common::memory_core::palace::{Drawer, PalaceId};
use trusty_common::memory_core::retrieval::PalaceHandle;
use trusty_common::memory_core::store::kg::KnowledgeGraph;
use trusty_common::memory_core::store::vector::UsearchStore;
use uuid::Uuid;

/// Why: a write burst drops many requests for one palace, and each drop calls
/// `mark_dirty` on the hot path. If the set were a queue, forty drops would
/// mean forty repairs of a palace that needs one.
/// Test: this test itself.
#[tokio::test]
async fn mark_dirty_is_idempotent() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = AppState::new(tmp.path().to_path_buf());
    assert!(dirty_palaces(&state).is_empty());

    for _ in 0..40 {
        mark_dirty(&state, "alpha");
    }
    mark_dirty(&state, "beta");

    let mut queued = dirty_palaces(&state);
    queued.sort();
    assert_eq!(queued, vec!["alpha".to_string(), "beta".to_string()]);
}

/// Why: `0` must be a documented way to turn the sweep off, distinct from a
/// zero-second interval — which would be a busy loop, not a disabled sweep.
/// A typo must not silently disable it either.
/// Test: this test itself.
#[test]
fn repair_interval_honours_env_override() {
    let prev = std::env::var(ENV_REPAIR_INTERVAL_SECS).ok();
    let cases: [(&str, Option<Duration>); 3] = [
        ("30", Some(Duration::from_secs(30))),
        ("0", None),
        (
            "banana",
            Some(Duration::from_secs(DEFAULT_REPAIR_INTERVAL_SECS)),
        ),
    ];
    for (raw, expected) in cases {
        // SAFETY: test-only env mutation, restored at the end of this test.
        unsafe { std::env::set_var(ENV_REPAIR_INTERVAL_SECS, raw) };
        assert_eq!(repair_interval(), expected, "value {raw:?}");
    }
    // SAFETY: same invariant.
    unsafe { std::env::remove_var(ENV_REPAIR_INTERVAL_SECS) };
    assert_eq!(
        repair_interval(),
        Some(Duration::from_secs(DEFAULT_REPAIR_INTERVAL_SECS))
    );
    if let Some(v) = prev {
        // SAFETY: restoring the caller's environment.
        unsafe { std::env::set_var(ENV_REPAIR_INTERVAL_SECS, v) };
    }
}

/// Why: with the lane off there is nothing to repair, and arming a timer that
/// wakes every five minutes for the rest of the process's life to do nothing
/// is a cost with no benefit. The lane is off in every shipped deployment
/// until piece 2 flips it, so this is the common case.
/// Test: this test itself.
#[tokio::test]
async fn repair_sweep_is_a_noop_without_the_lane() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = AppState::new(tmp.path().to_path_buf());
    assert!(state.bm25_client.is_none());
    spawn_repair_sweep(&state);
    mark_dirty(&state, "alpha");
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        dirty_palaces(&state),
        vec!["alpha".to_string()],
        "no sweep may run, so nothing may be drained"
    );
}

/// Why: a palace queued for repair that is no longer resident must be dropped,
/// not re-queued — otherwise a deleted palace spins in the set forever and
/// every pass logs about it.
/// Test: this test itself.
#[tokio::test]
async fn a_vanished_palace_is_dropped_from_the_queue() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = AppState::new(tmp.path().to_path_buf());
    mark_dirty(&state, "no-such-palace");

    let (attempted, repaired) = run_repair_pass(&state).await;

    assert_eq!(attempted, 1);
    assert_eq!(repaired, 0);
    assert!(
        dirty_palaces(&state).is_empty(),
        "a palace that no longer exists must not stay queued forever"
    );
}

/// Why: this is the half of the drop-on-full trade that was missing. A repair
/// pass that could not restore coverage must leave the palace queued, so the
/// next pass tries again — otherwise a daemon that happens to be down during
/// one pass converts a recoverable drop back into a permanent gap.
/// What: a resident palace with the lane off, so `backfill_state_palace`
/// returns `Disabled` and coverage stays unverified. Removing the re-insert in
/// `run_repair_pass`'s failure arm empties the queue and fails this.
/// Test: this test itself.
#[tokio::test]
async fn an_unrepairable_palace_stays_queued() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = AppState::new(tmp.path().to_path_buf());

    let vs = UsearchStore::new(tmp.path().join("idx.usearch"), 384).expect("vector store");
    let kg = KnowledgeGraph::open(&tmp.path().join("kg.db")).expect("kg");
    let handle = PalaceHandle::new(PalaceId::new("resident"), String::new(), vs, kg);
    handle
        .drawers
        .write()
        .push(Drawer::new(Uuid::new_v4(), "content worth indexing"));
    state.registry.register(handle);

    mark_dirty(&state, "resident");
    let (attempted, repaired) = run_repair_pass(&state).await;

    assert_eq!(attempted, 1);
    assert_eq!(repaired, 0, "the lane is off, so nothing can be repaired");
    assert_eq!(
        dirty_palaces(&state),
        vec!["resident".to_string()],
        "an unverified palace must stay queued for the next pass"
    );
}
