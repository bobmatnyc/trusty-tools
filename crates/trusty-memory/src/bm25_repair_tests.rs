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
    assert!(state.bm25.is_none());
    spawn_repair_sweep(&state);
    mark_dirty(&state, "alpha");
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        dirty_palaces(&state),
        vec!["alpha".to_string()],
        "no sweep may run, so nothing may be drained"
    );
}

/// Create a palace that exists ON DISK under `root`.
///
/// Why: the eviction test needs the difference between "absent from the LRU"
/// and "absent from disk" to be real, so the fixture has to go through the
/// registry's own create path rather than `register`, which only populates the
/// in-memory cache.
/// What: creates the palace and pushes one indexable drawer.
/// Test: used by the two tests below.
fn create_on_disk(state: &AppState, id: &str) {
    let handle = state
        .registry
        .create_palace(
            &state.data_root,
            trusty_common::memory_core::palace::Palace {
                id: PalaceId::new(id),
                name: id.to_string(),
                description: None,
                created_at: chrono::Utc::now(),
                data_dir: state.data_root.join(id),
            },
        )
        .expect("create palace on disk");
    // Persist through redb: the drawer table is rebuilt from there on open.
    let drawer = Drawer::new(Uuid::new_v4(), "content worth indexing");
    handle
        .kg
        .upsert_drawer_sync(&drawer)
        .expect("persist drawer");
    handle.drawers.write().push(drawer);
}

/// Why (#5048 re-review): this is the same fail-open as the coverage predicate,
/// one layer out. A dirty palace is exactly the kind that goes idle — its
/// writes are failing — so it is exactly the kind the LRU evicts. Resolving it
/// with `registry.get`, a bare cache lookup, dropped it from the queue
/// permanently and its gap waited for a restart, which is the outcome this
/// module exists to prevent.
/// What: builds a palace on disk through one `AppState`, then drives the repair
/// pass from a SECOND `AppState` over the same data root — a cold registry, so
/// the palace is on disk and absent from the LRU, which is what eviction looks
/// like. The pass must hydrate it and (with the lane off) leave it queued.
/// Test: this test itself. Swap `open_palace` back to `registry.get` and the
/// palace is dropped, so the queue is empty and this fails.
#[tokio::test]
async fn an_evicted_palace_is_rehydrated_not_dropped() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    create_on_disk(&AppState::new(root.clone()), "evicted");

    // A fresh AppState has an empty LRU — the palace is on disk and nowhere in
    // memory, exactly as after an idle eviction.
    let cold = AppState::new(root);
    assert!(
        cold.registry.list().is_empty(),
        "precondition: the registry must be cold, so `get` would miss"
    );
    mark_dirty(&cold, "evicted");

    let (attempted, repaired) = run_repair_pass(&cold).await;

    assert_eq!(attempted, 1);
    assert_eq!(repaired, 0, "the lane is off, so nothing can be repaired");
    assert_eq!(
        dirty_palaces(&cold),
        vec!["evicted".to_string()],
        "an evicted palace must be hydrated and kept queued, never dropped"
    );
    assert!(
        cold.registry.list().iter().any(|p| p.as_str() == "evicted"),
        "and the pass must actually have opened it"
    );
}

/// Why: the counterpart. A palace genuinely deleted from disk must leave the
/// queue, otherwise it spins forever and every pass logs about it. This is the
/// discrimination the on-disk listing buys — without it, "evicted" and
/// "deleted" are indistinguishable and one of the two answers is always wrong.
/// Test: this test itself.
#[tokio::test]
async fn a_palace_absent_from_disk_is_dropped_from_the_queue() {
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

    create_on_disk(&state, "resident");
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
