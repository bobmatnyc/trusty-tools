//! Regression tests for delete-during-in-flight-work on `DELETE /indexes/{id}`
//! (issue #3049).
//!
//! Why: `unregister_index` removed the registration and `remove_dir_all`'d the
//! data directory without awaiting or cancelling in-flight reindex / deferred-
//! embed / catch-up work. Recreating the same id immediately then let a
//! new-epoch task write the same on-disk paths as the old one, because those
//! paths are keyed purely by sanitized id. The same function also reported
//! `data_deleted` from the REQUEST while the removal itself was best-effort and
//! its failure was downgraded to a `warn!`, so a delete whose data removal
//! failed answered `data_deleted: true`.
//! What: three behaviours, each with its own test — the delete WAITS for the
//! per-index permit every long-running writer holds; it REFUSES on-disk removal
//! when that wait times out; and it reports `data_deleted` from the removal's
//! actual result. A fourth pins the cancel-flag eviction that keeps a recreated
//! index from starting cancelled.
//!
//! `multi_thread` is required, not incidental: `unregister_index` reaches
//! `watcher_manager.stop_for_index`, whose teardown uses `block_in_place`, which
//! PANICS on a current-thread runtime. On a single-threaded flavour these tests
//! would abort before their assertions and then "pass" post-fix by never
//! reaching the assertion at all.
//! Test: this module. Run with `cargo test -p trusty-search tests_3049`.

use super::search::unregister_index;
use super::tests_components::IsolatedDataDir;
use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
use crate::service::reindex::{index_cancel_flag, index_semaphore};
use crate::service::server::SearchAppState;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

/// Register `id` in a fresh app state and materialise its data directory with a
/// marker file, mirroring `tests_4123::router_with_index_and_data`.
///
/// The marker is what makes a "data survived" assertion meaningful: an empty
/// directory could vanish for unrelated reasons, a directory holding a known
/// byte string cannot.
fn state_with_index(
    data_root: &std::path::Path,
    id: &str,
) -> (Arc<SearchAppState>, std::path::PathBuf) {
    let registry = IndexRegistry::new();
    let root_path = data_root.join(format!("corpus-root-{id}"));
    std::fs::create_dir_all(&root_path).expect("create index root");
    registry.register(IndexHandle::bare(
        IndexId::new(id),
        Arc::new(RwLock::new(CodeIndexer::new(id, root_path.clone()))),
        root_path,
    ));
    let index_data_dir = data_root.join("indexes").join(id);
    std::fs::create_dir_all(&index_data_dir).expect("create index data dir");
    std::fs::write(index_data_dir.join("corpus.marker"), b"real corpus bytes")
        .expect("write marker");
    (Arc::new(SearchAppState::new(registry)), index_data_dir)
}

/// #3049: `unregister_index` must not return until in-flight work on the index
/// has finished.
///
/// Why: this is the issue itself. The per-index semaphore is the existing
/// quiescence point — every long-running writer (`runner::run_reindex`,
/// `defer_embed_queue`, the PATCH config handler) holds its permit for the full
/// duration of its work — so "the permit is free" is exactly "no writer is
/// mid-flight". Against the pre-fix commit this fails as a real assertion
/// failure: `unregister_index` never touched the permit, so it returned while
/// the simulated writer was still running and `finished` read `false`.
/// What: a fake in-flight task holds the permit, sleeps, sets `finished`, then
/// releases. The assertion is that `finished` is already set by the time
/// `unregister_index` returns.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn unregister_index_waits_for_an_in_flight_writer_to_release_the_permit() {
    let isolated = IsolatedDataDir::new();
    const ID: &str = "quiesce-wait-3049";
    let (state, _data_dir) = state_with_index(isolated.path(), ID);
    let index_id = IndexId::new(ID);

    let permit = index_semaphore(&index_id)
        .acquire_owned()
        .await
        .expect("per-index semaphore is never closed");
    let finished = Arc::new(AtomicBool::new(false));
    let writer_finished = finished.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        writer_finished.store(true, Ordering::Release);
        drop(permit);
    });

    let outcome = unregister_index(&state, ID, false).await;

    assert!(
        finished.load(Ordering::Acquire),
        "unregister_index returned while a writer still held the per-index permit — \
         the delete did not await in-flight work (issue #3049)"
    );
    assert!(
        outcome.quiesced,
        "the wait succeeded, so quiesced must be true"
    );
    assert!(outcome.removed, "the registration must still be removed");
}

/// #3049: when in-flight work never drains, the delete must REFUSE to destroy
/// the data directory rather than `remove_dir_all` it under a live writer.
///
/// Why: the timeout exists so a stuck writer cannot wedge the HTTP handler, but
/// timing out is not permission to delete — that is the exact data-loss the
/// issue describes, just deferred by the length of the timeout. This is the
/// error arm of the wait added above. Pre-fix it fails for the right reason:
/// there was no wait and no refusal, so the marker file was destroyed and
/// `data_deleted` read `true`.
/// What: holds the permit for the whole test so the wait provably expires
/// (`DELETE_QUIESCE_TIMEOUT` is 1.5s under `cfg(test)`), then asserts the
/// on-disk marker survived and both flags report the refusal.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn delete_with_delete_data_refuses_removal_when_a_writer_never_quiesces() {
    let isolated = IsolatedDataDir::new();
    const ID: &str = "quiesce-timeout-3049";
    let (state, data_dir) = state_with_index(isolated.path(), ID);
    let marker = data_dir.join("corpus.marker");

    // Held for the duration of the call — the wait cannot succeed.
    let _stuck_writer = index_semaphore(&IndexId::new(ID))
        .acquire_owned()
        .await
        .expect("per-index semaphore is never closed");

    let outcome = unregister_index(&state, ID, true).await;

    assert!(
        !outcome.quiesced,
        "a permit held for the whole call must expire the quiesce wait"
    );
    assert!(
        !outcome.data_deleted,
        "a refused removal must report data_deleted=false (issue #3049)"
    );
    assert!(
        marker.exists(),
        "data must NOT be destroyed under a live writer; {} is gone",
        marker.display()
    );
    assert_eq!(
        std::fs::read(&marker).expect("read marker"),
        b"real corpus bytes",
        "preserved data must be byte-identical, not merely present"
    );
    assert!(
        outcome.removed,
        "registry teardown still proceeds — only the disk removal is refused"
    );
}

/// #3049 fail-open arm: when `remove_index_data_dir` FAILS, the response must
/// report `data_deleted: false`.
///
/// Why: `data_deleted` used to be computed as `removed && params.delete_data` —
/// straight from the request — while the removal itself was best-effort and its
/// failure was downgraded to a `tracing::warn!`. A caller that reclaimed disk on
/// the strength of that field recorded the corpus as gone while every byte
/// remained, and the discrepancy was invisible. Pre-fix this test fails for the
/// right reason: the assertion on `data_deleted` trips because the handler
/// answered `true` for a removal that never happened.
/// What: plants a regular FILE where `remove_index_data_dir` expects the index
/// directory. `exists()` is true so the removal is attempted, and `remove_dir_all`
/// returns `NotADirectory` — a deterministic failure needing no permission games
/// and no root/non-root distinction.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn delete_reports_data_deleted_false_when_the_removal_fails() {
    let isolated = IsolatedDataDir::new();
    const ID: &str = "removal-fails-3049";
    let registry = IndexRegistry::new();
    let root_path = isolated.path().join("corpus-root-fail");
    std::fs::create_dir_all(&root_path).expect("create index root");
    registry.register(IndexHandle::bare(
        IndexId::new(ID),
        Arc::new(RwLock::new(CodeIndexer::new(ID, root_path.clone()))),
        root_path,
    ));
    let state = Arc::new(SearchAppState::new(registry));

    // A FILE, not a directory, at the path the removal targets.
    let indexes_dir = isolated.path().join("indexes");
    std::fs::create_dir_all(&indexes_dir).expect("create indexes dir");
    let data_path = indexes_dir.join(ID);
    std::fs::write(&data_path, b"not a directory").expect("plant file");

    let outcome = unregister_index(&state, ID, true).await;

    assert!(
        outcome.quiesced,
        "no writer is in flight, so the quiesce wait must succeed"
    );
    assert!(
        !outcome.data_deleted,
        "a FAILED data removal must report data_deleted=false, not echo the request \
         flag (issue #3049)"
    );
    assert!(
        data_path.exists(),
        "the planted file must survive — proving the removal really did fail"
    );
}

/// #3049: the cancel flag must be evicted when the delete completes, so an index
/// recreated under the same id does not start out cancelled.
///
/// Why: `unregister_index` sets the flag to ask in-flight writers to stop. A
/// flag that outlived the delete would make the recreated index's very first
/// reindex abort at its first batch — trading one bug for a worse one. This is
/// the same growth/staleness argument `remove_index_semaphore` already makes for
/// `INDEX_LOCKS`.
/// What: asserts the flag is set during teardown by reading it through a handle
/// captured beforehand (eviction removes the map entry, not the `Arc`), then
/// asserts a fresh `index_cancel_flag` call for the same id reads `false`.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn index_cancel_flag_is_evicted_so_a_recreated_index_starts_uncancelled() {
    let isolated = IsolatedDataDir::new();
    const ID: &str = "cancel-evict-3049";
    let (state, _data_dir) = state_with_index(isolated.path(), ID);
    let index_id = IndexId::new(ID);

    // Captured BEFORE the delete: this `Arc` survives the map eviction, so it
    // still witnesses the signal the delete sent.
    let observed = index_cancel_flag(&index_id);
    assert!(
        !observed.load(Ordering::Acquire),
        "a fresh index must start uncancelled"
    );

    let outcome = unregister_index(&state, ID, false).await;
    assert!(outcome.removed);

    assert!(
        observed.load(Ordering::Acquire),
        "unregister_index must SIGNAL the cancel flag so in-flight writers stop \
         (issue #3049)"
    );
    assert!(
        !index_cancel_flag(&index_id).load(Ordering::Acquire),
        "after the delete the flag must be evicted, so an index recreated under the \
         same id gets a fresh uncancelled flag (issue #3049)"
    );
}
