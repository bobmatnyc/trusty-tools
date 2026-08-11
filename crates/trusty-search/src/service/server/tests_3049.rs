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
//! What: one test per behaviour. Round 1 — the delete WAITS for in-flight work,
//! reports `data_deleted` from the removal's actual result, and evicts the cancel
//! flag so a recreated index does not start cancelled. Round 2 — the wait covers
//! writers that hold no reindex permit. Round 3 — a `delete_data` delete that
//! times out ABANDONS itself so the retry it advertises works; a writer parked
//! across a teardown stays visible to the next delete; and neither `POST
//! /indexes` nor `PATCH /indexes/:id/config` can slip inside a teardown window.
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
use crate::service::reindex::{index_cancel_flag, index_teardown_lock};
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

    let permit = index_teardown_lock(&index_id).read_owned().await;
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
        "unregister_index returned while a writer still held the teardown lock — \
         the delete did not await in-flight work (issue #3049)"
    );
    assert!(
        outcome.quiesced,
        "the wait succeeded, so quiesced must be true"
    );
    assert!(outcome.removed, "the registration must still be removed");
}

/// #3049: when in-flight work never drains, a `delete_data` delete must ABANDON
/// itself rather than `remove_dir_all` under a live writer.
///
/// Why: the timeout exists so a stuck writer cannot wedge the HTTP handler, but
/// timing out is not permission to delete — that is the exact data-loss the
/// issue describes, just deferred by the length of the timeout. Round 2 refused
/// only the removal and tore the registration down anyway; round 3 leaves
/// everything in place, which is what makes the retry the log advertises real
/// (see `a_second_delete_after_an_abandoned_one_reclaims_the_data`).
/// What: holds the shared side for the whole test so the wait provably expires
/// (`DELETE_QUIESCE_TIMEOUT` is 1.5s under `cfg(test)`), then asserts the
/// on-disk marker survived and every field of the outcome reports "nothing
/// happened".
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn delete_with_delete_data_refuses_removal_when_a_writer_never_quiesces() {
    let isolated = IsolatedDataDir::new();
    const ID: &str = "quiesce-timeout-3049";
    let (state, data_dir) = state_with_index(isolated.path(), ID);
    let marker = data_dir.join("corpus.marker");

    // Held for the duration of the call — the exclusive wait cannot succeed.
    let _stuck_writer = index_teardown_lock(&IndexId::new(ID)).read_owned().await;

    let outcome = unregister_index(&state, ID, true).await;

    assert!(
        !outcome.quiesced,
        "a read guard held for the whole call must expire the quiesce wait"
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
        !outcome.removed,
        "an abandoned delete must report removed=false — round 2 reported true \
         here and stranded the data directory forever (issue #3049)"
    );
}

/// #3049 round 3: after a delete abandons itself on a quiesce timeout, the
/// re-issued delete its log tells the operator to make must actually reclaim the
/// disk.
///
/// Why: this is the orphan-leak arm. Round 2's timeout path still ran
/// `registry.remove_and_get`, `remove_index_registry_entry` and `remove_root`,
/// and refused only the `remove_dir_all`. The retry then found `removed=false`
/// and `was_cold=false`, so the whole `if removed { … }` block — including the
/// data-removal branch — never ran, `remove_index_data_dir` was never attempted
/// a second time, and `spawn_orphan_reaper_ticker` does not cover this shape (it
/// reaps registrations whose root_path vanished, not data directories with no
/// registration). The directory leaked permanently, invisibly after a restart.
/// Against round 2 this fails for the right reason: the retry's assertions on
/// `removed` / `data_deleted` / the marker all trip, because the first delete
/// consumed the registration the retry needed.
/// What: times out one delete, asserts the whole registration survived (registry
/// entry present, cancel flag cleared so the surviving index is not born
/// cancelled), then releases the writer and re-issues the delete.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn a_second_delete_after_an_abandoned_one_reclaims_the_data() {
    let isolated = IsolatedDataDir::new();
    const ID: &str = "retry-after-abandon-3049";
    let (state, data_dir) = state_with_index(isolated.path(), ID);
    let marker = data_dir.join("corpus.marker");
    let index_id = IndexId::new(ID);

    let stuck_writer = index_teardown_lock(&index_id).read_owned().await;
    let abandoned = unregister_index(&state, ID, true).await;
    assert!(!abandoned.quiesced, "the wait must have expired");

    // The state a retry depends on: nothing was torn down.
    assert!(
        state.registry.get(&index_id).is_some(),
        "an abandoned delete must leave the registration in place, or the retry it \
         advertises can never reach the data-removal branch (issue #3049)"
    );
    assert!(
        !index_cancel_flag(&index_id).load(Ordering::Acquire),
        "an abandoned delete must un-signal the cancel it sent, or the surviving \
         index aborts its next reindex at the first checkpoint (issue #3049)"
    );

    drop(stuck_writer);
    let retry = unregister_index(&state, ID, true).await;

    assert!(
        retry.quiesced,
        "no writer is left, so the retry must quiesce"
    );
    assert!(
        retry.removed && retry.data_deleted,
        "the re-issued delete must actually deregister AND reclaim the disk \
         (removed={}, data_deleted={}) — issue #3049",
        retry.removed,
        retry.data_deleted,
    );
    assert!(
        !marker.exists(),
        "the retry reported the data deleted, so {} must be gone",
        marker.display()
    );
}

/// #3049 round 3: a writer parked on the teardown lock across a delete must
/// still be visible to the NEXT delete.
///
/// Why: `remove_index_teardown_lock` evicts the map entry, so a writer that was
/// parked on that entry wakes holding an `Arc` nothing else can reach. The next
/// delete calls `index_teardown_lock`, gets a brand-new uncontended lock, reports
/// `quiesced: true` instantly, and `remove_dir_all`s the directory the parked
/// writer is writing into — the round-2 shape of the recreate window, moved off
/// the semaphore and onto the teardown lock. `acquire_index_teardown_read`'s
/// `Arc::ptr_eq` re-validation is the fix. Against round 2 this fails for the
/// right reason: `!outcome.quiesced` trips because the second delete saw no
/// contention at all, and the marker assertion trips right behind it.
/// What: stages the exact interleaving — gen-1 holds the lock, a delete parks on
/// the write side, gen-2 parks on the read side of the SAME entry, gen-1 releases
/// so the delete completes and evicts — then asserts a `delete_data` delete is
/// refused while gen-2 still holds its guard.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
async fn a_writer_parked_across_teardown_is_visible_to_the_next_delete() {
    let isolated = IsolatedDataDir::new();
    const ID: &str = "parked-writer-3049";
    let (state, data_dir) = state_with_index(isolated.path(), ID);
    let marker = data_dir.join("corpus.marker");
    let index_id = IndexId::new(ID);

    // Generation 1: holds the lock the delete is about to wait on.
    let gen1 = index_teardown_lock(&index_id).read_owned().await;

    let delete_state = Arc::clone(&state);
    let delete = tokio::spawn(async move { unregister_index(&delete_state, ID, false).await });
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Generation 2 parks on the same entry, behind the delete's pending write.
    // The oneshot hands the guard back so the main test can hold it across the
    // second delete without a sleep-based handoff.
    let (tx, rx) = tokio::sync::oneshot::channel();
    let gen2_id = index_id.clone();
    tokio::spawn(async move {
        let guard = crate::service::reindex::acquire_index_teardown_read(&gen2_id).await;
        let _ = tx.send(guard);
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    drop(gen1);
    let first = delete.await.expect("delete task");
    assert!(
        first.quiesced,
        "gen-1 released, so the first delete quiesces"
    );
    let _gen2 = rx.await.expect("gen-2 acquires once the delete releases");

    // Gen-2 is writing. A delete that would destroy the disk must be refused.
    let second = unregister_index(&state, ID, true).await;
    assert!(
        !second.quiesced,
        "the second delete reported quiesced=true while a writer parked across the \
         first teardown still held the lock — it was handed a fresh, uncontended \
         instance (issue #3049)"
    );
    assert!(
        marker.exists(),
        "data must survive a delete raced against a parked writer; {} is gone",
        marker.display()
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

/// #3049 round 2: a DELETE must wait for `POST /indexes/:id/index-file`, which
/// holds NO reindex permit.
///
/// Why: this is the CRITICAL the first round missed. `unregister_index` waited
/// on `index_semaphore`, which only `run_reindex`, `defer_embed_queue`, and the
/// PATCH config handler ever acquire. `index_file_handler` writes straight to
/// `handle.indexer` after a bare `registry.get()`, so the delete's acquire
/// succeeded instantly against zero contention, reported `quiesced: true`, and
/// `remove_dir_all` ran while the write was still landing in the same redb and
/// HNSW files. Five sibling paths had the same hole — see the table on
/// `INDEX_TEARDOWN_LOCKS`.
///
/// What: takes the teardown lock's SHARED side exactly as `index_file_handler`
/// now does, and asserts the delete blocks until it is released. Against the
/// round-1 commit this fails as a real assertion failure — the delete returned
/// immediately because that writer held no semaphore permit.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn delete_waits_for_an_ungated_index_file_write() {
    let isolated = IsolatedDataDir::new();
    const ID: &str = "ungated-writer-3049";
    let (state, data_dir) = state_with_index(isolated.path(), ID);
    let marker = data_dir.join("corpus.marker");
    let index_id = IndexId::new(ID);

    // Exactly what `index_file_handler` holds for the span of its write.
    let write_guard = index_teardown_lock(&index_id).read_owned().await;
    let write_finished = Arc::new(AtomicBool::new(false));
    let flag = write_finished.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        flag.store(true, Ordering::Release);
        drop(write_guard);
    });

    let outcome = unregister_index(&state, ID, true).await;

    assert!(
        write_finished.load(Ordering::Acquire),
        "DELETE returned while an index-file write was still in flight — the \
         quiescence point does not cover writers that hold no reindex permit \
         (issue #3049)"
    );
    assert!(
        outcome.quiesced,
        "the write completed within the timeout, so the wait must have succeeded"
    );
    assert!(
        outcome.data_deleted,
        "a quiesced delete with delete_data=true must actually remove the data"
    );
    assert!(
        !marker.exists(),
        "data removal was reported, so the marker must be gone"
    );
}

/// #3049 round 3: `POST /indexes` must not register a second generation of an id
/// whose delete is still tearing it down.
///
/// Why: `unregister_index` removes the hot registry entry partway through its
/// teardown, and `create_index_handler`'s only guard was
/// `state.registry.get(&id).is_some()`. A create landing after that removal but
/// before teardown finished registered a fresh handle and could spawn a reindex
/// into the directory the delete was about to `remove_dir_all` — and, once the
/// delete evicted the id's lock entries, the two generations no longer shared a
/// quiescence primitive at all. Against round 2 this fails for the right reason:
/// the ordering assertion trips because the create took no lock and returned
/// while the delete was still parked on the writer.
/// What: an in-flight writer holds the delete off; the create is issued while the
/// delete is parked; both record their completion order. The create must land
/// AFTER the delete, and must then succeed on a registry that tells the truth.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
async fn create_index_cannot_register_while_a_delete_is_tearing_the_id_down() {
    let _isolated = IsolatedDataDir::new();
    const ID: &str = "recreate-window-3049";
    let (dir, root) = super::test_support::allowlisted_index_root("ts-3049-recreate-");
    let state = Arc::new(SearchAppState::new(IndexRegistry::new()));
    let embedder: Arc<dyn crate::core::embed::Embedder> =
        Arc::new(crate::core::embed::MockEmbedder::new(8));
    state.install_embedder(embedder).await;

    // Generation 1, registered through the real handler.
    let created = super::indexes::create_index_handler(
        axum::extract::State(Arc::clone(&state)),
        axum::Json(create_req(ID, root.clone())),
    )
    .await;
    assert_eq!(created.status(), axum::http::StatusCode::OK, "gen-1 create");

    // An in-flight writer keeps the delete parked long enough for the create to
    // be issued squarely inside the teardown window.
    let writer_guard = index_teardown_lock(&IndexId::new(ID)).read_owned().await;
    let order = Arc::new(std::sync::Mutex::new(Vec::<&'static str>::new()));

    let delete_state = Arc::clone(&state);
    let delete_order = Arc::clone(&order);
    let delete = tokio::spawn(async move {
        let outcome = unregister_index(&delete_state, ID, false).await;
        delete_order.lock().expect("order lock").push("delete");
        outcome
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    let create_state = Arc::clone(&state);
    let create_order = Arc::clone(&order);
    let create_root = root.clone();
    let create = tokio::spawn(async move {
        let resp = super::indexes::create_index_handler(
            axum::extract::State(create_state),
            axum::Json(create_req(ID, create_root)),
        )
        .await;
        create_order.lock().expect("order lock").push("create");
        resp
    });

    tokio::time::sleep(Duration::from_millis(200)).await;
    drop(writer_guard);

    let deleted = delete.await.expect("delete task");
    let recreated = create.await.expect("create task");
    assert!(deleted.removed, "the delete must deregister generation 1");

    assert_eq!(
        *order.lock().expect("order lock"),
        vec!["delete", "create"],
        "the recreate must not land until teardown has finished — a create that \
         returns first has registered a second generation over a half-deleted \
         index (issue #3049)"
    );
    assert_eq!(
        recreated.status(),
        axum::http::StatusCode::OK,
        "once teardown is done the recreate is an ordinary create and must succeed"
    );
    assert!(
        state.registry.get(&IndexId::new(ID)).is_some(),
        "generation 2 must be the surviving registration"
    );
    drop(dir);
}

/// #3049 round 3: `PATCH /indexes/:id/config` must wait for an in-flight
/// teardown.
///
/// Why: round 2's writer table listed this path as taking the teardown lock's
/// read side and it never did. Round 1 quiesced on `index_semaphore`, which this
/// handler DOES hold, so moving quiescence onto the teardown lock silently
/// un-guarded it — leaving a PATCH free to re-register the handle and re-upsert
/// the `indexes.toml` entry a concurrent delete had just removed, resurrecting a
/// deleted index at the next warm boot. Against round 2 this fails for the right
/// reason: the PATCH returns while the teardown guard is still held.
/// What: holds the exclusive side exactly as `unregister_index` does, then
/// asserts the handler has not answered while it is held and does once released.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
async fn patch_index_config_waits_for_an_in_flight_teardown() {
    let isolated = IsolatedDataDir::new();
    const ID: &str = "patch-during-teardown-3049";
    let (state, _data_dir) = state_with_index(isolated.path(), ID);

    // Exactly what `unregister_index` holds across its teardown.
    let teardown_guard =
        crate::service::reindex::acquire_index_teardown_write(&IndexId::new(ID)).await;

    let patch_state = Arc::clone(&state);
    let patch = tokio::spawn(async move {
        super::index_config::patch_index_config_handler(
            axum::extract::State(patch_state),
            axum::extract::Path(ID.to_string()),
            axum::Json(super::index_config::PatchIndexConfigRequest {
                include_docs: Some(false),
                ..Default::default()
            }),
        )
        .await
    });

    tokio::time::sleep(Duration::from_millis(250)).await;
    assert!(
        !patch.is_finished(),
        "PATCH answered while a teardown held the exclusive lock — it can re-upsert \
         the indexes.toml entry the delete just removed (issue #3049)"
    );

    drop(teardown_guard);
    let resp = patch.await.expect("patch task");
    assert_eq!(
        resp.status(),
        axum::http::StatusCode::OK,
        "once teardown releases, the PATCH proceeds normally"
    );
}

/// Build a `CreateIndexRequest` with every optional field defaulted — the same
/// shape `tests_2336::create_req` uses, kept local so neither module has to make
/// its helper `pub(super)`.
fn create_req(id: &str, root_path: std::path::PathBuf) -> super::router::CreateIndexRequest {
    super::router::CreateIndexRequest {
        id: id.to_string(),
        root_path,
        include_paths: None,
        exclude_globs: None,
        extensions: None,
        domain_terms: None,
        path_filter: None,
        include_docs: None,
        respect_gitignore: None,
        follow_links: None,
        lexical_only: None,
        skip_kg: None,
        skip_vector: None,
        defer_embed: None,
        extra_skip_dirs: None,
        data_file_max_bytes: None,
        allow_sensitive_path: false,
    }
}

/// #3049 round 4: a `delete_data=false` delete that TIMES OUT must leave the
/// still-running writer's teardown lock reachable, so the next delete sees it.
///
/// Why: round 3 evicted `INDEX_LOCKS` and `INDEX_TEARDOWN_LOCKS` unconditionally,
/// including on the `!quiesced && !delete_data` branch — and `delete_data`
/// defaults to `false`, so that is the DEFAULT endpoint behaviour whenever a
/// writer outlasts the wait. The eviction handed the next caller a fresh,
/// uncontended lock disconnected from the live writer, so a subsequent
/// `?delete_data=true` reported `quiesced: true` and `remove_dir_all`'d the
/// directory that writer was writing into — the stale-primitive corruption
/// round 3 fixed for the recreate race, reached through the timeout instead.
/// `a_writer_parked_across_teardown_is_visible_to_the_next_delete` does not
/// cover it: its gen-1 writer releases before the wait expires, so its first
/// delete quiesces and never enters this branch.
/// What: one writer holds the shared side for the whole test, so BOTH deletes
/// time out. The index is re-registered between them because the first delete
/// deregisters it — without that the second delete short-circuits on
/// `removed=false` and never reaches the removal branch, hiding the data-loss
/// half of the regression. Against round 3 both assertions trip: the second
/// delete reports `quiesced: true` and the marker is gone.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
async fn a_timed_out_delete_leaves_the_writers_lock_reachable_to_the_next_delete() {
    let isolated = IsolatedDataDir::new();
    const ID: &str = "timeout-evict-3049";
    let (state, data_dir) = state_with_index(isolated.path(), ID);
    let marker = data_dir.join("corpus.marker");
    let index_id = IndexId::new(ID);

    // A writer that never finishes: held across both deletes.
    let _writer = crate::service::reindex::acquire_index_teardown_read(&index_id).await;

    // First delete: the DEFAULT mode. The wait expires, and round 3 deregistered
    // and evicted anyway.
    let first = unregister_index(&state, ID, false).await;
    assert!(
        !first.quiesced,
        "the writer never released, so the first delete must report quiesced=false"
    );
    assert!(
        first.removed,
        "delete_data=false still deregisters on timeout (issue #4123 semantics)"
    );

    // The caller re-registers the id — the realistic sequence, and what puts the
    // second delete on the path that actually removes data.
    let root_path = isolated.path().join(format!("corpus-root-{ID}"));
    state.registry.register(IndexHandle::bare(
        index_id.clone(),
        Arc::new(RwLock::new(CodeIndexer::new(ID, root_path.clone()))),
        root_path,
    ));

    let second = unregister_index(&state, ID, true).await;
    assert!(
        !second.quiesced,
        "the second delete reported quiesced=true while the first writer was STILL \
         holding the teardown lock — the timed-out delete evicted the lock and handed \
         this one a fresh, uncontended instance (issue #3049 round 4)"
    );
    assert!(
        !second.data_deleted,
        "a delete that never quiesced must not report data_deleted=true"
    );
    assert!(
        marker.exists(),
        "data must survive a delete raced against a writer that outlasted an earlier \
         delete's quiesce wait; {} is gone",
        marker.display()
    );
}

/// #3049 round 4: a `delete_data=true` racing an in-flight startup schema
/// migration must be refused, not destroy the corpus mid-migration.
///
/// Why: `spawn_index_migrations` runs at every boot, once per registered index,
/// detached. `M001PerPubConstRust::apply` loops `commit_parsed_batch` over
/// 64-file batches, so it is a durable writer for its whole run — but it took no
/// teardown guard, so a delete landing on a still-unmigrated index found the lock
/// uncontended, reported `quiesced: true`, and `remove_dir_all`'d the directory
/// mid-commit. It sat outside the hand-derived writer table for three rounds
/// because it lives in `src/commands`, inside an anonymous `tokio::spawn`.
/// What: holds the indexer's WRITE lock, which blocks the spawned task inside
/// `run_migrations` at its first `read_schema_version` — parking it
/// deterministically while it holds the teardown guard, with no sleep-based
/// race. The delete must then time out and change nothing. Against the pre-fix
/// code the `quiesced` assertion trips: the spawned migration held no guard.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
async fn a_delete_cannot_destroy_an_index_mid_migration() {
    let isolated = IsolatedDataDir::new();
    const ID: &str = "migration-race-3049";
    let (state, data_dir) = state_with_index(isolated.path(), ID);
    let marker = data_dir.join("corpus.marker");
    let index_id = IndexId::new(ID);

    let handle = state.registry.get(&index_id).expect("registered");
    // Taken BEFORE the spawn so the migration task cannot get past
    // `read_schema_version`, which needs the indexer read lock.
    let indexer_write = Arc::clone(&handle.indexer).write_owned().await;

    crate::core::migration::spawn_index_migrations(&state);
    // Let the spawned task reach its teardown-guard acquire and then park on the
    // indexer lock. Only ordering is timed here: the task cannot proceed past the
    // indexer lock at all while `indexer_write` is alive, so a slow scheduler
    // delays the assertion rather than changing its outcome.
    tokio::time::sleep(Duration::from_millis(250)).await;

    let outcome = unregister_index(&state, ID, true).await;

    assert!(
        !outcome.quiesced,
        "the delete reported quiesced=true while a schema migration was in flight — \
         the migration task holds no teardown guard, so this delete would have \
         removed the data directory mid-commit (issue #3049 round 4)"
    );
    assert!(
        !outcome.data_deleted,
        "a delete that never quiesced must not report data_deleted=true"
    );
    assert!(
        marker.exists(),
        "the corpus must survive a delete raced against an in-flight migration; {} is gone",
        marker.display()
    );
    drop(indexer_write);
}
