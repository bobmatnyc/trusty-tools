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

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tokio::sync::RwLock;
use tower::ServiceExt;
use trusty_common::embedder::MockEmbedder;

use super::build_router;
use super::tests_components::IsolatedDataDir;
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
use crate::core::Embedder;
use crate::service::persistence::{corpus_redb_path_for_entry, PersistedIndex};
use crate::service::persistence_loader::build_indexer_from_entry;
use crate::service::reindex::{push_job, wait_for_turn, ReindexProgress};
use crate::service::server::SearchAppState;

const INDEX_ID: &str = "delete-queued-job-8664";

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
    let seq = push_job(handle, Arc::new(ReindexProgress::new()), 1);
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
    let outcome = wait_for_turn(seq).await;
    assert!(
        closed,
        "a queued embed job must not keep a deleted index's redb file open"
    );
    let deleted = outcome.expect_err("a job for a deleted index must not run its pass");
    assert_eq!(deleted.index_id, INDEX_ID);
}
