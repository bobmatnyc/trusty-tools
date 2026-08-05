//! Regression tests for timeout-parked index recovery (#4250).
//!
//! Why: a timed-out warm-boot restore was parked in the cold store (#4087) and
//! then left there. Parking makes an index reachable by a query naming its id
//! and by nothing else — `list_indexes` omits cold entries, so a client that
//! discovers indexes by listing never learns the id exists, and boot reconcile
//! walks `registry.list()`, so PR #4717's never-walked guard cannot see it
//! either. The owner's daemon served 25 of 55 registered indexes for hours
//! behind this; the original report ran 13.5 hours. Every symptom looks like a
//! healthy daemon, so a regression here ships green.
//! What: three groups — the cold store's typed reason, the recovery pass
//! itself, and the `/health` un-latching.
//! Test: these tests.

use std::sync::Arc;

use super::*;
use crate::core::embed::{Embedder, MockEmbedder};
use crate::core::registry::{IndexId, IndexRegistry};
use crate::service::colocated_storage::COLOCATED_DIR_NAME;
use crate::service::persistence::PersistedIndex;
use crate::service::timeout_recovery::{recover_timed_out_indexes, RecoveryTally};
use crate::service::warm_boot::BoundedRestoreOutcome;

/// A colocated entry at a real, populated root so a restore can succeed.
fn live_entry(id: &str, root: &std::path::Path) -> PersistedIndex {
    std::fs::create_dir_all(root.join(COLOCATED_DIR_NAME)).unwrap();
    PersistedIndex {
        id: id.to_string(),
        root_path: root.to_path_buf(),
        colocated: true,
        ..Default::default()
    }
}

async fn state_with_embedder() -> Arc<SearchAppState> {
    let state = Arc::new(SearchAppState::new(IndexRegistry::new()));
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(16));
    state.install_embedder(embedder).await;
    state
}

/// Why (#4250, the root conflation): the cold store held "deferred on purpose"
/// and "failed and papered over" in one map with nothing to tell them apart —
/// the same `bool`-shaped defect PR #4718 removed from `BoundedRestoreOutcome`.
/// That is precisely why a timed-out index stayed dark: no consumer could
/// single it out for retry without also retrying every index
/// `TRUSTY_WARMBOOT_MAX_INDEXES` deliberately deferred. This pins the
/// separation, including that the reason is assigned by the STORE (a caller
/// cannot label a deferral as a timeout, or the reverse).
/// What: parks one entry through `park_if_parkable(TimedOut)` and registers
/// another through `register_cold_entries`; asserts only the first appears in
/// the timeout cohort while both count as cold.
/// Test: this test.
#[test]
fn cold_store_timed_out_cohort_excludes_deferred_entries() {
    let store = crate::service::lazy_loader::ColdIndexStore::new();
    store.register_cold_entries(vec![PersistedIndex {
        id: "deferred".to_string(),
        ..Default::default()
    }]);
    store.park_if_parkable(
        PersistedIndex {
            id: "timed-out".to_string(),
            ..Default::default()
        },
        BoundedRestoreOutcome::TimedOut,
    );

    assert_eq!(store.len(), 2, "both are cold");
    assert_eq!(
        store.timed_out_len(),
        1,
        "only the timeout-parked entry is in the retry cohort — draining deferred \
         entries would load exactly the indexes TRUSTY_WARMBOOT_MAX_INDEXES said not \
         to load (issue #4250)"
    );
    let ids: Vec<String> = store
        .timed_out_entries()
        .into_iter()
        .map(|e| e.id)
        .collect();
    assert_eq!(ids, ["timed-out"]);
}

/// Why (#4250, the defect): before this pass existed, a timeout-parked index
/// stayed absent from the registry — and therefore from `list_indexes` and from
/// boot reconcile — until a human restarted the daemon. This test states that
/// as a before/after: absent after parking, present after one recovery pass.
/// The "before" half is the pre-fix behaviour verbatim, so the test fails
/// against it (nothing ever moved the index out of that state).
/// What: parks a live-rooted entry as `TimedOut`, asserts it is absent from the
/// registry, runs one pass, asserts it is registered and the cohort drained.
/// Test: this test.
#[tokio::test]
async fn timed_out_index_is_driven_back_into_the_registry() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("recoverable");
    let entry = live_entry("recoverable", &root);
    let state = state_with_embedder().await;
    let id = IndexId::new("recoverable");

    state
        .cold_store
        .park_if_parkable(entry, BoundedRestoreOutcome::TimedOut);

    // This is the pre-fix steady state, and it is where the index used to stay:
    // registered in indexes.toml, invisible to `list_indexes`, unreachable by
    // reconcile, waiting for a query that names an id no listing reveals.
    assert!(
        state.registry.get(&id).is_none(),
        "a timeout-parked index starts absent from the registry"
    );
    assert_eq!(state.cold_store.timed_out_len(), 1);

    let attempts = dashmap::DashMap::new();
    let tally = recover_timed_out_indexes(&state, &attempts).await;

    assert_eq!(
        tally,
        RecoveryTally {
            recovered: 1,
            still_timing_out: 0,
            gave_up: 0,
        },
        "one pass must bring the index back — nothing else ever will (issue #4250)"
    );
    assert!(
        state.registry.get(&id).is_some(),
        "the recovered index must be in the registry, so `list_indexes` shows it and \
         PR #4717's never-walked guard can reach it (issue #4250)"
    );
    assert_eq!(
        state.cold_store.timed_out_len(),
        0,
        "a recovered index must leave the timeout cohort, or /health stays degraded \
         forever after recovery"
    );
}

/// Why: the recovery pass must not defeat `TRUSTY_WARMBOOT_MAX_INDEXES`.
/// Deferred entries are lazy BY DESIGN — loading them proactively would undo
/// the memory bound an operator asked for.
/// What: registers a deferred entry at a perfectly loadable root and asserts a
/// recovery pass leaves it exactly where it is.
/// Test: this test.
#[tokio::test]
async fn recovery_pass_leaves_deferred_entries_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("deferred-on-purpose");
    let entry = live_entry("deferred-on-purpose", &root);
    let state = state_with_embedder().await;

    state.cold_store.register_cold_entries(vec![entry]);

    let attempts = dashmap::DashMap::new();
    let tally = recover_timed_out_indexes(&state, &attempts).await;

    assert_eq!(
        tally,
        RecoveryTally::default(),
        "a deferred entry is not a failure and must not be retried (issue #4250)"
    );
    assert!(
        state
            .registry
            .get(&IndexId::new("deferred-on-purpose"))
            .is_none(),
        "the deferred index must stay cold until a query asks for it"
    );
    assert_eq!(state.cold_store.len(), 1);
}

/// Why (#4250, the latch): `recompute_warm_boot_degraded` deliberately treated
/// `indexes_skipped_timeout` as a permanent input, on the stated premise that
/// "a scan timeout genuinely CANNOT heal without a daemon restart". That held
/// only because nothing retried a timed-out index. With the recovery pass the
/// premise is false, and a daemon that got every index back kept reporting
/// degraded for its whole life — which is how #4250's reporter saw `degraded`
/// long after the daemon had usable indexes.
/// What: sets the boot-time counter as `record_warm_boot_result` would, drains
/// the cohort, recomputes, and asserts the flag clears. Against the pre-fix
/// function the flag stays `true` because it read the frozen counter.
/// Test: this test.
#[tokio::test]
async fn warm_boot_degraded_clears_once_the_timeout_cohort_drains() {
    let state = state_with_embedder().await;
    if let Ok(mut s) = state.warmboot_summary.lock() {
        s.indexes_skipped_timeout = 3;
        s.warm_boot_degraded = true;
    }

    // Cohort is empty (recovery drained it), nothing else is degraded.
    assert_eq!(state.cold_store.timed_out_len(), 0);
    super::health::recompute_warm_boot_degraded(&state);

    let after = state.warmboot_summary.lock().unwrap().warm_boot_degraded;
    assert!(
        !after,
        "warm_boot_degraded must clear once every timeout-parked index has come \
         back — it used to latch until a daemon restart (issue #4250)"
    );
}

/// Why: the un-latch must not weaken the signal in the direction that matters.
/// An index that never came back has to keep the daemon degraded, or #4250
/// would be traded for a worse bug: a genuinely broken daemon reporting `ok`.
/// What: same setup with one entry STILL parked; asserts the flag stays set.
/// Test: this test.
#[tokio::test]
async fn warm_boot_degraded_stays_set_while_an_index_is_still_parked() {
    let state = state_with_embedder().await;
    state.cold_store.park_if_parkable(
        PersistedIndex {
            id: "still-down".to_string(),
            ..Default::default()
        },
        BoundedRestoreOutcome::TimedOut,
    );

    super::health::recompute_warm_boot_degraded(&state);

    let after = state.warmboot_summary.lock().unwrap().warm_boot_degraded;
    assert!(
        after,
        "a still-parked timeout must keep the daemon degraded (issue #4250)"
    );
}
