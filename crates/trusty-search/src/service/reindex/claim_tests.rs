//! Tests for the one-reindex-per-index claim (#8889).
//!
//! Why: the claim is only a guard if it refuses a second run, releases on every
//! exit path, and refuses when it cannot be checked.
//! What: unit tests on `try_claim_reindex`, the library spawn entry point, and
//! the HTTP core's fail-closed arm. Every test uses its own index id, because
//! the claim slots are process-global.
//! Test: this module IS the test.

use super::*;
use std::sync::Arc;

use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexRegistry};
use crate::service::reindex::ReindexProgress;
use crate::service::server::SearchAppState;

/// Why: a refusal that does not name the running job leaves the caller guessing.
/// What: a second claim on a held index fails with the holder's run id; a claim
/// on another index is unaffected.
/// Test: this function IS the test.
#[test]
fn a_second_claim_is_refused_and_names_the_running_job() {
    let id = IndexId::new("claim-8889-second");
    let first = try_claim_reindex(&id, "http", true).expect("first claim");
    match try_claim_reindex(&id, "reconcile", false) {
        Err(ReindexClaimError::AlreadyRunning { running, .. }) => {
            assert_eq!(running, first.run().clone());
            assert_eq!(running.origin, "http");
            assert!(running.force);
        }
        other => panic!("#8889: expected AlreadyRunning, got {other:?}"),
    }
    let other = try_claim_reindex(&IndexId::new("claim-8889-second-other"), "http", false);
    assert!(other.is_ok(), "another index must not contend");
}

/// Why (fail-closed): a guard that cannot be read must refuse, never admit.
/// What: poisons the index's slot by panicking while holding it, then claims.
/// Test: this function IS the test.
#[test]
fn a_poisoned_slot_refuses_the_claim() {
    let id = IndexId::new("claim-8889-poisoned");
    poison(&id);
    match try_claim_reindex(&id, "http", false) {
        Err(ReindexClaimError::GuardUnavailable { index_id, .. }) => assert_eq!(index_id, id.0),
        other => panic!("#8889: a poisoned slot must refuse, got {other:?}"),
    }
}

/// Poison `id`'s claim slot from a thread that panics while holding it.
fn poison(id: &IndexId) {
    let slot = claim_slot(id);
    let _ = std::thread::spawn(move || {
        let _held = slot.lock().expect("fresh slot");
        panic!("poison the #8889 claim slot");
    })
    .join();
    assert!(
        claim_slot(id).is_poisoned(),
        "precondition: slot is poisoned"
    );
}

/// Why: a failed or cancelled reindex must not lock its index forever.
/// What: a claim moved into a task is released when the task panics, when the
/// task is aborted mid-await (the daemon-shutdown path), and on a normal drop.
/// Test: this function IS the test.
#[tokio::test]
async fn the_claim_is_released_on_panic_and_on_cancellation() {
    let id = IndexId::new("claim-8889-release");

    let claim = try_claim_reindex(&id, "test", false).expect("claim");
    let panicked = tokio::spawn(async move {
        let _claim = claim;
        panic!("reindex task panics");
    })
    .await;
    assert!(panicked.is_err(), "the task panicked");
    let claim = try_claim_reindex(&id, "test", false).expect("#8889: released after a panic");

    let parked = tokio::spawn(async move {
        let _claim = claim;
        std::future::pending::<()>().await;
    });
    tokio::task::yield_now().await;
    assert!(
        try_claim_reindex(&id, "test", false).is_err(),
        "held while parked"
    );
    parked.abort();
    assert!(parked.await.expect_err("aborted").is_cancelled());
    let claim = try_claim_reindex(&id, "test", false).expect("#8889: released on cancellation");

    drop(claim);
    try_claim_reindex(&id, "test", false).expect("released on drop");
}

/// Why: two runs sharing one staging file is the #8889 corruption itself.
/// What: two successive claims on one index, and two concurrent claims on two
/// indexes, all get distinct staging names that the leftover sweep recognises.
/// Test: this function IS the test.
#[test]
fn two_claims_get_distinct_staging_names() {
    let id = IndexId::new("claim-8889-staging");
    let first = try_claim_reindex(&id, "test", false).expect("claim");
    let first_name = first.staging_file_name();
    drop(first);
    let second = try_claim_reindex(&id, "test", false).expect("claim");
    let other =
        try_claim_reindex(&IndexId::new("claim-8889-staging-b"), "test", false).expect("claim");
    let names = [
        first_name,
        second.staging_file_name(),
        other.staging_file_name(),
    ];
    assert_ne!(names[0], names[1]);
    assert_ne!(names[1], names[2]);
    assert_ne!(names[0], names[2]);
    for name in &names {
        assert_ne!(name, crate::service::storage_layout::REDB_TMP_FILE);
        assert!(
            super::super::staging_leftovers::is_staging_leftover(name),
            "{name}"
        );
    }
}

/// A handle over an in-memory indexer rooted at `root`.
fn bare_handle(id: &str, root: &std::path::Path) -> IndexHandle {
    let indexer = CodeIndexer::new(id, root.to_path_buf());
    IndexHandle::bare(
        IndexId::new(id),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        root.to_path_buf(),
    )
}

/// Why: library and boot-reconcile callers enter through the spawn functions,
/// not the HTTP route, and must be refused too.
/// What: with the index claimed, `spawn_reindex` and `spawn_reindex_with_cleanup`
/// both return `AlreadyRunning` and leave the progress untouched.
/// Test: this function IS the test.
#[tokio::test]
async fn spawn_reindex_refuses_while_the_index_is_claimed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let handle = Arc::new(bare_handle("claim-8889-spawn", dir.path()));
    let _held = try_claim_reindex(&handle.id, "http", false).expect("claim");
    let progress = Arc::new(ReindexProgress::new());

    let refused =
        crate::service::reindex::spawn_reindex(Arc::clone(&handle), Arc::clone(&progress), false);
    assert!(matches!(
        refused,
        Err(ReindexClaimError::AlreadyRunning { .. })
    ));
    let refused = crate::service::reindex::spawn_reindex_with_cleanup(
        Arc::clone(&handle),
        Arc::clone(&progress),
        false,
        None,
        None,
        None,
        false,
        None,
    );
    assert!(matches!(
        refused,
        Err(ReindexClaimError::AlreadyRunning { .. })
    ));
}

/// Why (Fail-Open Check, #8889): if the guard cannot be checked, the HTTP entry
/// point must refuse and queue nothing. Before #8889 there was no guard at all,
/// so the request was queued.
/// What: poisons the index's slot, then calls `reindex_report`; expects `503
/// reindex_guard_unavailable` and no progress entry.
/// Test: this function IS the test.
#[tokio::test]
async fn a_reindex_whose_guard_is_poisoned_is_refused_and_queues_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let id = "claim-8889-http-poisoned";
    let registry = IndexRegistry::new();
    registry.register(bare_handle(id, dir.path()));
    let state = Arc::new(SearchAppState::new(registry));
    poison(&IndexId::new(id));

    let (status, body) = crate::service::server::reindex_report(&state, id, None)
        .await
        .expect_err("#8889: a guard that cannot be checked must refuse");
    assert_eq!(
        status,
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        "{body}"
    );
    assert_eq!(body["error"], "reindex_guard_unavailable");
    assert_eq!(body["queued"], false);
    assert!(state.reindex_progress.get(&IndexId::new(id)).is_none());
}
