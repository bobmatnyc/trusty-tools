//! Issue #8883: a serve-only index refuses every reindex and no automatic
//! writer touches it; a normal index reindexes as before.
//!
//! Why: a serving instance loads an index shipped from a dedicated indexer, and
//! a local reindex replaces it. The refusal sits in the reindex claim, so each
//! entry point that claims is driven here through its real function: the HTTP
//! handler, the internal spawn the boot reconcile uses, and the config-release
//! catch-up. The watcher spawn is the one automatic writer gated outside the
//! claim, so it is driven here too, as is a `POST /indexes` over a cold
//! serve-only id, which rewrites the id's `indexes.toml` record.
//! What: every subject is one planted index holding one committed file. A
//! refusal must leave its chunk count unchanged and publish no progress entry.
//! The socket transport is in `rpc::writes_tests`, the restore and boot re-arm
//! in `commands::start_restore`'s markers tests, the boot reconcile delta in
//! `reconcile_tests`, and the vector-gap backfill in `vector_gap_tests`.
//! Test: this module IS the test.

use std::path::Path;
use std::sync::Arc;

use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use tokio::sync::RwLock;

use crate::core::embed::{Embedder, MockEmbedder};
use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
use crate::core::store::{UsearchStore, VectorStore};
use crate::service::network_fs::MountKind;
use crate::service::reindex::{spawn_reindex_with_cleanup, ReindexClaimError, ReindexProgress};
use crate::service::watcher_manager::WatcherManager;

use super::reindex_handlers::{reindex_handler, start_release_catch_up};
use super::state::SearchAppState;

const DIM: usize = 8;

/// A registered index rooted at `root` holding one committed file.
async fn planted(id: &str, root: &Path, serve_only: bool) -> Arc<SearchAppState> {
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(DIM));
    let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(DIM).expect("usearch"));
    let indexer =
        CodeIndexer::new(id, root.to_str().expect("utf8 root")).with_components(embedder, store);
    indexer
        .index_file("src/lib.rs", "pub fn shipped() -> u32 {\n    1\n}\n")
        .await
        .expect("seed the shipped corpus");
    let registry = IndexRegistry::new();
    registry.register(IndexHandle {
        serve_only,
        ..IndexHandle::bare(
            IndexId::new(id),
            Arc::new(RwLock::new(indexer)),
            root.to_path_buf(),
        )
    });
    Arc::new(SearchAppState::new(registry))
}

/// The registered handle for `id` and its current chunk count.
async fn handle_and_chunks(state: &SearchAppState, id: &str) -> (Arc<IndexHandle>, usize) {
    let handle = state.registry.get(&IndexId::new(id)).expect("registered");
    let chunks = handle.indexer.read().await.chunk_count();
    (handle, chunks)
}

/// Why (#8883): `POST /indexes/{id}/reindex` (and MCP `reindex`, CLI `index
/// --force` / `reindex`, which all post to it) must not rebuild a serve-only
/// index.
/// What: the real handler answers `403 index_serve_only` naming the index and
/// why, queues nothing, and leaves the corpus as shipped. Against code without
/// the claim refusal it answers `200 queued: true`.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reindex_of_a_serve_only_index_is_refused_with_403() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = planted("serve-only-8883", dir.path(), true).await;
    let (_, before) = handle_and_chunks(&state, "serve-only-8883").await;
    assert!(before > 0, "precondition: the shipped corpus holds chunks");

    let refused = reindex_handler(
        State(Arc::clone(&state)),
        AxumPath("serve-only-8883".to_string()),
        None,
    )
    .await
    .expect_err("#8883: a serve-only index must not accept a reindex");

    let (status, axum::Json(body)) = refused;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
    assert_eq!(body["error"], "index_serve_only");
    assert_eq!(body["index_id"], "serve-only-8883");
    assert_eq!(body["queued"], false);
    assert_eq!(body["retryable"], false);
    let message = body["message"].as_str().expect("message is a string");
    assert!(
        message.contains("'serve-only-8883' is serve-only") && message.contains("indexes.toml"),
        "the message must name the index, why, and the remedy: {message}"
    );
    assert!(
        state
            .reindex_progress
            .get(&IndexId::new("serve-only-8883"))
            .is_none(),
        "a refused reindex must queue nothing"
    );
    let (_, after) = handle_and_chunks(&state, "serve-only-8883").await;
    assert_eq!(after, before, "the shipped corpus must be unchanged");
}

/// Why: a gate that refused every index would stop every indexer.
/// What: the same planted index without the mark is still queued and gets a
/// progress entry.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reindex_of_a_normal_index_is_still_queued() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = planted("normal-8883", dir.path(), false).await;

    let axum::Json(body) = reindex_handler(
        State(Arc::clone(&state)),
        AxumPath("normal-8883".to_string()),
        None,
    )
    .await
    .map_err(|(status, axum::Json(body))| format!("{status}: {body}"))
    .expect("a normal index must accept a reindex");

    assert_eq!(body["queued"], true, "body: {body}");
    assert!(
        state
            .reindex_progress
            .get(&IndexId::new("normal-8883"))
            .is_some(),
        "an accepted reindex publishes its progress entry"
    );
}

/// Why (#8883): the boot reconcile's full reindex and every library caller
/// start through `spawn_reindex_with_cleanup`, with no HTTP handler in front.
/// What: the spawn is refused with `ReindexClaimError::ServeOnly` and the
/// corpus is unchanged. Against code without the claim refusal it returns `Ok`.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_internal_reindex_spawn_refuses_a_serve_only_index() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = planted("serve-only-spawn-8883", dir.path(), true).await;
    let (handle, before) = handle_and_chunks(&state, "serve-only-spawn-8883").await;

    let refused = spawn_reindex_with_cleanup(
        Arc::clone(&handle),
        Arc::new(ReindexProgress::new()),
        true,
        None,
        None,
        None,
        false,
        None,
    );

    assert!(
        matches!(refused, Err(ReindexClaimError::ServeOnly { ref index_id, .. }) if index_id == "serve-only-spawn-8883"),
        "#8883: the internal spawn must be refused as serve-only, got {refused:?}"
    );
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(handle.indexer.read().await.chunk_count(), before);
}

/// Why (#8883): a config PATCH that releases a held index starts a catch-up
/// reindex on its own (#9059); it must not rebuild a serve-only index.
/// What: `start_release_catch_up` answers `started: false` with the
/// serve-only reason and publishes no progress entry.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_config_release_catch_up_refuses_a_serve_only_index() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = planted("serve-only-release-8883", dir.path(), true).await;
    let (handle, _) = handle_and_chunks(&state, "serve-only-release-8883").await;

    let answer = start_release_catch_up(&state, handle, false).await;

    assert_eq!(answer["started"], false, "answer: {answer}");
    assert!(
        answer["reason"]
            .as_str()
            .is_some_and(|r| r.contains("is serve-only")),
        "the refusal must say why: {answer}"
    );
    assert!(state
        .reindex_progress
        .get(&IndexId::new("serve-only-release-8883"))
        .is_none());
}

/// Why (#8883): a watcher writes every saved file into its index, so it would
/// drift a serve-only index away from the one that was shipped.
/// What: the spawn is a no-op for a serve-only index on a local root. Against
/// code without the gate the index is watched.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_watcher_starts_for_a_serve_only_index() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = planted("serve-only-watch-8883", dir.path(), true).await;
    let (handle, _) = handle_and_chunks(&state, "serve-only-watch-8883").await;
    let manager = WatcherManager::new();

    manager
        .spawn_for_index_with_mount_kind(&handle, MountKind::Local)
        .await;

    assert!(
        !manager.is_watching(&handle.id).await,
        "#8883: a serve-only index must get no file watcher"
    );
    manager.stop_all().await;
}

/// Why (#8883, code-critic HIGH): `POST /indexes` over a cold-parked id
/// rewrites that id's whole `indexes.toml` record. Auto-register sends these
/// routinely, so one idempotent create must not make a shipped index
/// reindexable.
/// What: a serve-only entry in `indexes.toml` and the cold store, then a create
/// with the same id and root. The record keeps `serve_only = true`, the new
/// live handle carries it, and a reindex is still refused with 403. Against
/// code that builds the record with `serve_only: false` the record is cleared
/// and the reindex is queued.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn a_create_over_a_cold_serve_only_index_keeps_the_mark() {
    use crate::service::persistence::{
        find_index_registry_entry, remove_index_registry_entry, upsert_index_registry_entry,
        PersistedIndex,
    };
    let id = "serve-only-cold-8883";
    let (_dir, root) = super::test_support::allowlisted_index_root("ts-8883-cold-");
    let mut entry = PersistedIndex::new(id, root.clone());
    entry.serve_only = true;
    upsert_index_registry_entry(entry.clone()).expect("persist the shipped entry");
    let state = Arc::new(SearchAppState::new(IndexRegistry::new()));
    state
        .install_embedder(Arc::new(MockEmbedder::new(DIM)))
        .await;
    state.cold_store.register_cold_entries(vec![entry]);
    assert!(
        state.registry.get(&IndexId::new(id)).is_none(),
        "precondition: cold"
    );

    let req = serde_json::from_value(serde_json::json!({ "id": id, "root_path": root }))
        .expect("create request");
    super::create_index_report(&state, req)
        .await
        .map_err(|(status, body)| format!("{status}: {body}"))
        .expect("the create over the cold id succeeds");

    let persisted = find_index_registry_entry(id)
        .expect("indexes.toml reads")
        .expect("the entry is still registered");
    let handle = state.registry.get(&IndexId::new(id)).expect("now live");
    let refused = reindex_handler(State(Arc::clone(&state)), AxumPath(id.to_string()), None).await;
    remove_index_registry_entry(id).expect("clean up the entry");

    assert!(
        persisted.serve_only,
        "#8883: the create cleared the operator's mark"
    );
    assert!(handle.serve_only, "#8883: the live handle lost the mark");
    let (status, axum::Json(body)) = refused.expect_err("#8883: the reindex must stay refused");
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
    assert_eq!(body["error"], "index_serve_only");
}
