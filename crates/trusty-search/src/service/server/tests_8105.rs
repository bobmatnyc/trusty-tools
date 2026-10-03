//! Issue #8105: `POST /indexes/{id}/reindex` refuses a write-quarantined index
//! up front instead of queueing a run that can never persist.
//!
//! Why: on 0.52.0 such a reindex answered `queued: true`, embedded 496 chunks,
//! and then had every durable write refused, so `chunk_count` stayed null and
//! the caller's completion poll never ended.
//! What: the refusal through the real HTTP handler, and a healthy-index control
//! that proves the gate does not refuse an index whose corpus opened. Both
//! indexes come from `build_indexer_from_entry`; the quarantine is raised by a
//! DIRECTORY at the redb path, the technique `tests/corpus_open_quarantine_4122.rs`
//! uses, never by setting the flag by hand.
//! Test: this module IS the test.

use std::path::Path;
use std::sync::Arc;

use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use tokio::sync::RwLock;

use crate::core::embed::{Embedder, MockEmbedder};
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
use crate::service::persistence::PersistedIndex;
use crate::service::persistence_loader::build_indexer_from_entry;

use super::reindex_handlers::reindex_handler;
use super::state::SearchAppState;

/// Register one colocated index rooted at `root`, built by the production loader.
async fn state_with_index(id: &str, root: &Path) -> Arc<SearchAppState> {
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(8));
    let mut entry = PersistedIndex::new(id.to_string(), root.to_path_buf());
    entry.colocated = true;
    let indexer = build_indexer_from_entry(&entry, &embedder)
        .await
        .expect("build indexer");
    let registry = IndexRegistry::new();
    registry.register(IndexHandle::bare(
        IndexId::new(id),
        Arc::new(RwLock::new(indexer)),
        root.to_path_buf(),
    ));
    Arc::new(SearchAppState::new(registry))
}

/// Why (#8105): accepting the request is what left the caller polling forever.
/// What: a DIRECTORY at `<root>/.trusty-search/index.redb` fails the corpus
/// open, which raises the write quarantine. The handler must answer 409 with
/// the classified reason and queue nothing. Against pre-fix code it answers
/// `200 queued: true`.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reindex_of_a_write_quarantined_index_is_refused_with_409() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join(".trusty-search").join("index.redb"))
        .expect("create a directory at the redb path");
    let state = state_with_index("quarantined-8105", dir.path()).await;
    let index_id = IndexId::new("quarantined-8105");
    let handle = state.registry.get(&index_id).expect("registered");
    let kind = {
        let indexer = handle.indexer.read().await;
        assert!(
            indexer.is_write_quarantined(),
            "precondition: a directory at the redb path must quarantine the index"
        );
        indexer
            .corpus_open_failure
            .expect("the kind is recorded with the flag")
    };

    let refused = reindex_handler(
        State(Arc::clone(&state)),
        AxumPath(index_id.0.clone()),
        None,
    )
    .await
    .expect_err("#8105: a write-quarantined index must not accept a reindex");

    let (status, axum::Json(body)) = refused;
    assert_eq!(status, StatusCode::CONFLICT, "body: {body}");
    assert_eq!(body["error"], "index_write_quarantined");
    assert_eq!(body["index_id"], "quarantined-8105");
    assert_eq!(body["failure_kind"], kind.label());
    assert_eq!(body["queued"], false);
    assert_eq!(
        body["retryable"], false,
        "retrying the request cannot lift the quarantine"
    );
    let message = body["message"].as_str().expect("message is a string");
    assert!(
        message.contains("write-quarantined") && message.contains("restart the daemon"),
        "the message must name the quarantine and how to clear it: {message}"
    );
    assert!(
        message.contains(kind.stage_reason()),
        "the message must carry the classified reason verbatim: {message}"
    );
    assert!(
        state.reindex_progress.get(&index_id).is_none(),
        "#8105: nothing may be queued, so no caller waits on a completion"
    );
}

/// Why: a gate that refused every index would be a worse outage than #8105.
/// What: an index whose colocated corpus opened cleanly still gets
/// `200 queued: true` and a progress entry.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reindex_of_a_healthy_index_is_still_queued() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = state_with_index("healthy-8105", dir.path()).await;
    let index_id = IndexId::new("healthy-8105");
    let handle = state.registry.get(&index_id).expect("registered");
    assert!(
        !handle.indexer.read().await.is_write_quarantined(),
        "precondition: the control index must open its corpus"
    );

    let axum::Json(body) = reindex_handler(
        State(Arc::clone(&state)),
        AxumPath(index_id.0.clone()),
        None,
    )
    .await
    .map_err(|(status, axum::Json(body))| format!("{status}: {body}"))
    .expect("a healthy index must accept a reindex");

    assert_eq!(body["queued"], true, "body: {body}");
    assert!(
        state.reindex_progress.get(&index_id).is_some(),
        "an accepted reindex publishes its progress entry"
    );
}
