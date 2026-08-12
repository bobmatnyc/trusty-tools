//! #5349 — a write against a cold-parked index drives the lazy load the read
//! path drives, and a load that fails refuses the write.
//!
//! Why: `index-file` / `remove-file` answered `503 index_not_resident` for an
//! index a plain `search` would have restored on the spot, telling the caller
//! to issue an unrelated search purely for its side effect and then retry. On a
//! network-mounted root those two endpoints are the only thing keeping the
//! index current (#3408), so the writes simply stopped. The tests here drive
//! the real mechanism — a genuine colocated redb corpus on disk, a real park, a
//! real restore — because a fake handle would prove only that the code compiles.
//!
//! The failure arm gets equal weight: making the write path load an index is
//! only safe if a load that CANNOT succeed still refuses the write instead of
//! absorbing the failure into a `200`.
//!
//! Test: this file.

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use tokio::sync::RwLock;

use super::state::SearchAppState;
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
use crate::core::Embedder;
use crate::service::lazy_loader::cold_park_index;
use crate::service::persistence::PersistedIndex;
use crate::service::persistence_loader::build_indexer_from_entry;

/// Daemon state with a mock embedder installed — the cold-load path refuses to
/// run without one, so every test here needs it.
async fn mock_state() -> Arc<SearchAppState> {
    let state = SearchAppState::new(IndexRegistry::new());
    let embedder: Arc<dyn Embedder> = Arc::new(crate::core::embed::MockEmbedder::new(8));
    state.install_embedder(embedder).await;
    Arc::new(state)
}

/// A colocated entry, so the restore finds its corpus under `<root>/.trusty-search/`.
fn colocated_entry(id: &str, root: PathBuf) -> PersistedIndex {
    let mut entry = PersistedIndex::new(id.to_string(), root);
    entry.colocated = true;
    entry
}

fn index_request(path: &str, content: &str) -> super::router::IndexFileRequest {
    serde_json::from_value(serde_json::json!({ "path": path, "content": content }))
        .expect("IndexFileRequest")
}

fn remove_request(path: &str) -> super::router::RemoveFileRequest {
    serde_json::from_value(serde_json::json!({ "path": path })).expect("RemoveFileRequest")
}

/// Files currently represented in the index's chunk corpus, deduped and sorted.
///
/// Chunks materialised from the durable corpus carry the file path resolved
/// against the index root, so these are absolute; callers match on the relative
/// suffix they wrote via [`contains_file`].
async fn indexed_files(state: &Arc<SearchAppState>, id: &str) -> Vec<String> {
    let handle = state
        .registry
        .get(&IndexId::new(id.to_string()))
        .expect("index must be resident");
    let indexer = handle.indexer.read().await;
    let (_total, chunks) = indexer.enumerate_chunks(0, 1_000).await;
    let mut files: Vec<String> = chunks.into_iter().map(|c| c.file).collect();
    files.sort();
    files.dedup();
    files
}

/// Whether `files` holds an entry for the relative path `rel`.
fn contains_file(files: &[String], rel: &str) -> bool {
    files.iter().any(|f| f == rel || f.ends_with(rel))
}

/// A write against a cold-parked index loads it and lands, exactly as a read
/// against the same state would.
///
/// Why (#5349): pre-fix this handler did a bare hot-registry lookup and
/// returned `503 index_not_resident`, so `expect` below fails against the
/// pre-fix commit. Asserting the CHUNK landed — not merely that the call
/// returned `200` — is what keeps a future "load it, then forget to write"
/// regression from passing.
/// Test: this test.
#[tokio::test]
async fn cold_parked_index_accepts_a_write_by_driving_the_load() {
    let state = mock_state().await;
    let (_dir, root) = super::test_support::allowlisted_index_root("ts-5349-write-");
    let id = "cold-accepts-write";
    state
        .cold_store
        .register_cold_entries(vec![colocated_entry(id, root)]);

    let axum::Json(body) = super::files::index_file_handler(
        State(Arc::clone(&state)),
        axum::extract::Path(id.to_string()),
        axum::Json(index_request(
            "src/auth.rs",
            "fn authenticate() -> bool { true }",
        )),
    )
    .await
    .expect("a cold-parked index must accept the write by loading itself");

    assert_eq!(body["indexed"], true);
    assert_eq!(body["path"], "src/auth.rs");
    assert!(
        state.registry.get(&IndexId::new(id.to_string())).is_some(),
        "the write must have driven the load, leaving the index resident"
    );
    assert!(
        !state.cold_store.contains(&IndexId::new(id.to_string())),
        "a completed load consumes the cold entry"
    );
    let files = indexed_files(&state, id).await;
    assert!(
        contains_file(&files, "src/auth.rs"),
        "reporting `indexed: true` without the chunk landing is the failure this \
         test exists for; corpus holds {files:?}"
    );
}

/// The delete half drives the load too, and actually removes the chunks.
///
/// Why (#5349): `index-file` and `remove-file` are one contract. A caller
/// replaying a `git diff` against a network mount issues both, so a delete that
/// still 503s would leave stale chunks behind for every file removed while the
/// index was parked. Pre-fix `expect` fails here for the same reason as the
/// write test.
/// Test: this test.
#[tokio::test]
async fn cold_parked_index_accepts_a_delete_by_driving_the_load() {
    let state = mock_state().await;
    let (_dir, root) = super::test_support::allowlisted_index_root("ts-5349-delete-");
    let id = "cold-accepts-delete";
    let index_id = IndexId::new(id.to_string());
    let entry = colocated_entry(id, root);

    // Populate a real on-disk corpus, register it live, then park it — the
    // exact state `TRUSTY_MAX_RESIDENT_INDEXES` produces at runtime.
    {
        let embedder = state.current_embedder().await.expect("mock embedder");
        let indexer = build_indexer_from_entry(&entry, &embedder)
            .await
            .expect("build indexer");
        indexer
            .index_file("src/auth.rs", "fn authenticate() -> bool { true }")
            .await
            .expect("seed the corpus");
        state.registry.register(IndexHandle::bare(
            index_id.clone(),
            Arc::new(RwLock::new(indexer)),
            entry.root_path.clone(),
        ));
    }
    assert!(
        cold_park_index(&index_id, &state.registry, &state.cold_store, entry.clone()).await,
        "the index must be cold-parked before the delete is issued"
    );

    let axum::Json(body) = super::files::remove_file_handler(
        State(Arc::clone(&state)),
        axum::extract::Path(id.to_string()),
        axum::Json(remove_request("src/auth.rs")),
    )
    .await
    .expect("a cold-parked index must accept the delete by loading itself");

    assert!(
        body["removed_chunks"].as_u64().unwrap_or(0) >= 1,
        "the delete must reach real chunks, not an empty freshly-built index: {body}"
    );
    assert!(
        indexed_files(&state, id).await.is_empty(),
        "the removed file must be gone from the loaded index"
    );
}

/// A load that cannot succeed refuses the write — it never reports success.
///
/// Why (#5349): this is the arm that makes the change safe. The cold entry
/// points at a root that no longer exists, so `restore_index_on_demand` marks
/// it permanently failed and registers nothing. The handler must surface that
/// verdict rather than returning `200` for a write no index ever received.
/// Pre-fix the same call answered `index_not_resident` with `retryable: true` —
/// the wrong verdict, and one that would send this caller into a retry loop
/// against an index that can never come back — so the two assertions on `error`
/// and `retryable` fail against the pre-fix commit.
/// Test: this test.
#[tokio::test]
async fn write_against_an_unloadable_cold_index_fails_loudly() {
    let state = mock_state().await;
    let (_dir, root) = super::test_support::allowlisted_index_root("ts-5349-gone-");
    let id = "cold-unloadable";
    let missing_root = root.join("deleted-since-it-was-parked");
    state
        .cold_store
        .register_cold_entries(vec![colocated_entry(id, missing_root)]);

    let (code, axum::Json(body)) = super::files::index_file_handler(
        State(Arc::clone(&state)),
        axum::extract::Path(id.to_string()),
        axum::Json(index_request("src/auth.rs", "fn authenticate() {}")),
    )
    .await
    .expect_err("a write whose index could not be loaded must not report success");

    assert_eq!(code, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"], "index_restore_failed");
    assert_eq!(
        body["retryable"], false,
        "a deleted root never comes back on its own — advising a retry would loop forever"
    );
    assert!(
        state.registry.get(&IndexId::new(id.to_string())).is_none(),
        "a failed load must leave no half-built index behind for the next write to land in"
    );
    assert!(
        state.cold_store.is_failed(&IndexId::new(id.to_string())),
        "the failure must be recorded so the next write fails fast instead of retrying the restore"
    );
}

/// A genuinely unknown id is still a 404 on the write path.
///
/// Why (#5349): driving the load must not widen the status code. #4715's
/// invariant — a 404 from an index-scoped endpoint rules out every store — is
/// what stops the MCP layer classifying an unknown index as "not built yet".
/// Test: this test.
#[tokio::test]
async fn unknown_index_is_still_404_after_the_write_path_learned_to_load() {
    let state = mock_state().await;
    let (code, axum::Json(body)) = super::files::index_file_handler(
        State(Arc::clone(&state)),
        axum::extract::Path("never-existed".to_string()),
        axum::Json(index_request("src/auth.rs", "fn authenticate() {}")),
    )
    .await
    .expect_err("genuinely unknown");

    assert_eq!(code, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "unknown index: never-existed");
}

/// Two writes arriving together on a cold index both land in ONE loaded index.
///
/// Why (#5349): `get_or_load_index`'s per-index loading gate serialises the
/// restore, so the second caller re-checks the hot registry after the gate and
/// gets the handle the first one registered. Without that, both would build an
/// indexer over the same colocated redb — the loser hits `DatabaseAlreadyOpen`
/// and one of the two writes lands in a handle nobody ever reads. Asserting
/// BOTH files are present in the ONE resident index is what distinguishes those
/// two outcomes; a pair of `200`s alone would not.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_writes_on_a_cold_index_both_land_in_one_loaded_index() {
    let state = mock_state().await;
    let (_dir, root) = super::test_support::allowlisted_index_root("ts-5349-race-");
    let id = "cold-concurrent-writes";
    state
        .cold_store
        .register_cold_entries(vec![colocated_entry(id, root)]);

    let first = tokio::spawn({
        let state = Arc::clone(&state);
        async move {
            super::files::index_file_handler(
                State(state),
                axum::extract::Path(id.to_string()),
                axum::Json(index_request("src/one.rs", "fn one() -> u8 { 1 }")),
            )
            .await
            .map(|axum::Json(body)| body)
        }
    });
    let second = tokio::spawn({
        let state = Arc::clone(&state);
        async move {
            super::files::index_file_handler(
                State(state),
                axum::extract::Path(id.to_string()),
                axum::Json(index_request("src/two.rs", "fn two() -> u8 { 2 }")),
            )
            .await
            .map(|axum::Json(body)| body)
        }
    });

    let (first, second) = (
        first.await.expect("first task"),
        second.await.expect("second task"),
    );
    assert!(
        first.is_ok() && second.is_ok(),
        "both concurrent writes must succeed: {first:?} / {second:?}"
    );
    assert_eq!(
        state.registry.list().len(),
        1,
        "a second load would mean two indexers over one redb, and one lost write"
    );
    let files = indexed_files(&state, id).await;
    assert!(
        contains_file(&files, "src/one.rs") && contains_file(&files, "src/two.rs"),
        "every write that answered 200 must be present in the index that survived; \
         corpus holds {files:?}"
    );
}
