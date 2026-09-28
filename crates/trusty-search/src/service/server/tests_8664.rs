//! `DELETE /indexes/{id}` on a cold-parked index closes the files a queued
//! deferred-embed job holds, and the job then fails cleanly (#8664).
//!
//! Why: a cold-parked index has no registry handle, so the #8167 close found
//! nothing to close while a queued job's own `Arc<IndexHandle>` kept
//! `index.redb` open and `hnsw.usearch` mapped. The job then ran its embed
//! pass against a deleted index — the #8232 SIGBUS shape.
//! What: a real colocated index, parked in the cold store only, with one job
//! pushed onto the deferred-embed heap. The test deletes it through the real
//! router, then drives the queued job itself.
//! Test: this module.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tokio::sync::RwLock;
use tower::ServiceExt;
use trusty_common::embedder::MockEmbedder;

use super::build_router;
use super::tests_components::IsolatedDataDir;
use crate::core::indexer::{CodeIndexer, IndexDeleted};
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
use crate::core::Embedder;
use crate::service::persistence::{corpus_redb_path_for_entry, PersistedIndex};
use crate::service::persistence_loader::build_indexer_from_entry;
use crate::service::reindex::{push_job, wait_for_turn, LiveJob, ReindexProgress};
use crate::service::server::SearchAppState;

const INDEX_ID: &str = "delete-queued-job-8664";

/// Drive a queued job to its end under a deadline, so a wedged queue fails
/// the test instead of hanging CI (#8770).
async fn settle(job: LiveJob) -> Result<(), IndexDeleted> {
    tokio::time::timeout(Duration::from_secs(60), wait_for_turn(job))
        .await
        .expect("the queued job never settled — the deferred-embed queue is wedged")
}

/// #8664: DELETE of a cold-parked index releases `index.redb` although a
/// queued embed job holds the only handle, and that job then ends with
/// `IndexDeleted` instead of embedding into the deleted index.
#[tokio::test]
#[serial_test::serial]
async fn delete_closes_the_files_a_queued_embed_job_holds_for_a_cold_index() {
    let _isolated = IsolatedDataDir::new();
    let root = tempfile::tempdir().expect("root");
    let mut entry = PersistedIndex::new(INDEX_ID.to_string(), root.path().to_path_buf());
    entry.colocated = true;
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(8));
    let indexer = build_indexer_from_entry(&entry, &embedder)
        .await
        .expect("indexer");
    indexer
        .index_file("src/lib.rs", "pub fn handler() -> u32 { 7 }\n")
        .await
        .expect("index_file");
    let handle = Arc::new(IndexHandle::bare(
        IndexId::new(INDEX_ID),
        Arc::new(RwLock::new(indexer)),
        entry.root_path.clone(),
    ));
    let redb = corpus_redb_path_for_entry(&entry).expect("redb path");

    // Cold-parked: the registry holds nothing, the queued job holds the handle.
    let state = SearchAppState::new(IndexRegistry::new());
    state.cold_store.register_cold_entries(vec![entry]);
    let router = build_router(state);
    let job = push_job(handle, Arc::new(ReindexProgress::new()), 1);
    assert!(
        redb::Database::open(&redb).is_err(),
        "precondition: the queued job's handle holds index.redb open"
    );

    let resp = router
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/indexes/{INDEX_ID}"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::OK, "delete must succeed");

    let reopened = redb::Database::open(&redb);
    let closed = reopened.is_ok();
    drop(reopened);
    let outcome = settle(job).await;
    assert!(
        closed,
        "a queued embed job must not keep a deleted index's redb file open"
    );
    let deleted = outcome.expect_err("a job for a deleted index must not run its pass");
    assert_eq!(deleted.index_id, INDEX_ID);
}

/// #8664: a queued job's handle that cannot be closed abandons the whole
/// delete, and because job handles close before the hot one, the resident
/// index is still registered and serving afterwards.
#[tokio::test]
#[serial_test::serial]
async fn a_job_handle_that_cannot_close_abandons_the_delete_before_the_hot_index() {
    const ID: &str = "delete-abandon-8664";
    let _isolated = IsolatedDataDir::new();
    let root = tempfile::tempdir().expect("root");
    let bare = || {
        IndexHandle::bare(
            IndexId::new(ID),
            Arc::new(RwLock::new(CodeIndexer::new(
                ID,
                root.path().display().to_string(),
            ))),
            root.path().to_path_buf(),
        )
    };
    let registry = IndexRegistry::new();
    let hot = registry.register(bare());
    // A detached handle with its own indexer, as a job queued before a park holds.
    let job = Arc::new(bare());
    let queued = push_job(Arc::clone(&job), Arc::new(ReindexProgress::new()), 1);
    let router = build_router(SearchAppState::new(registry.clone()));

    // Holding the job indexer's write lock makes its close time out.
    let held = job.indexer.write().await;
    let resp = router
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/indexes/{ID}"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    drop(held);
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body");
    let body: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    let hot_deleted = hot.indexer.read().await.is_deleted();
    let _ = job.indexer.write().await.detach_for_delete();
    let outcome = settle(queued).await;

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert_eq!(body["removed"], false, "{body}");
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|e| e.contains("not closed")),
        "the abandon must name the unclosed files: {body}"
    );
    assert!(
        registry.get(&IndexId::new(ID)).is_some(),
        "an abandoned delete leaves the index registered"
    );
    assert!(
        !hot_deleted,
        "the hot handle must not be closed before a job handle's close fails"
    );
    assert!(outcome.is_err(), "the settled job must not run its pass");
}
