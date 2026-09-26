//! `DELETE /indexes/{id}` closes the index's files while a handle clone
//! survives (#8167, #8232).
//!
//! Why: a queued deferred-embed job or an in-flight request holds its own
//! `Arc<IndexHandle>`. Before the fix, dropping the registry's reference left
//! `index.redb` open (umount answered `EBUSY`) and `hnsw.usearch` mapped (a
//! truncate turned the next search into SIGBUS). These tests hold such a clone
//! across the delete, the way `service::reindex::defer_embed_queue` does.
//! What: a real colocated index — redb corpus plus a saved HNSW snapshot,
//! reloaded so the snapshot is served from its mmap view — deleted through the
//! real router with `TRUSTY_DATA_DIR` redirected at a temp dir.
//! Test: this module; the SIGBUS reproduction is
//! `tests/delete_releases_mmap_8232.rs`.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tokio::sync::RwLock;
use tower::ServiceExt;
use trusty_common::embedder::MockEmbedder;

use super::build_router;
use super::tests_components::IsolatedDataDir;
use crate::core::indexer::{IndexDeleted, SearchQuery};
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
use crate::core::Embedder;
use crate::service::persistence::{
    corpus_redb_path_for_entry, hnsw_path_for_entry, PersistedIndex,
};
use crate::service::persistence_loader::build_indexer_from_entry;
use crate::service::server::SearchAppState;

const INDEX_ID: &str = "delete-close-8167";

/// A registered, mmap-served colocated index plus the paths of its files.
struct Fixture {
    router: axum::Router,
    registry: IndexRegistry,
    handle: Arc<IndexHandle>,
    redb: std::path::PathBuf,
    _root: tempfile::TempDir,
}

/// Seed a colocated index on disk, then reload it so the HNSW snapshot is
/// served from its view — the state a warm-booted daemon holds.
async fn fixture() -> Fixture {
    let root = tempfile::tempdir().expect("root");
    let mut entry = PersistedIndex::new(INDEX_ID.to_string(), root.path().to_path_buf());
    entry.colocated = true;
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(8));
    {
        let seed = build_indexer_from_entry(&entry, &embedder)
            .await
            .expect("seed indexer");
        seed.index_file("src/lib.rs", "pub fn handler() -> u32 { 7 }\n")
            .await
            .expect("index_file");
        let hnsw = hnsw_path_for_entry(&entry).expect("hnsw path");
        assert!(seed.save_vector_store(&hnsw).await.expect("save hnsw"));
    }
    let indexer = build_indexer_from_entry(&entry, &embedder)
        .await
        .expect("reload indexer");
    let registry = IndexRegistry::new();
    let handle = registry.register(IndexHandle::bare(
        IndexId::new(INDEX_ID),
        Arc::new(RwLock::new(indexer)),
        entry.root_path.clone(),
    ));
    Fixture {
        router: build_router(SearchAppState::new(registry.clone())),
        registry,
        handle,
        redb: corpus_redb_path_for_entry(&entry).expect("redb path"),
        _root: root,
    }
}

/// Send `DELETE /indexes/{INDEX_ID}` and return status plus decoded body.
async fn send_delete(router: &axum::Router) -> (StatusCode, serde_json::Value) {
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/indexes/{INDEX_ID}"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body");
    (status, serde_json::from_slice(&bytes).expect("json body"))
}

fn query() -> SearchQuery {
    SearchQuery {
        text: "handler".to_string(),
        ..Default::default()
    }
}

/// #8167: after DELETE, nothing in this process holds `index.redb` or the
/// HNSW snapshot, although a clone of the handle is still alive.
#[tokio::test]
#[serial_test::serial]
async fn delete_releases_the_corpus_and_vector_files_while_a_clone_survives() {
    let _isolated = IsolatedDataDir::new();
    let fx = fixture().await;
    let surviving = Arc::clone(&fx.handle);
    assert!(
        redb::Database::open(&fx.redb).is_err(),
        "precondition: the live index holds its redb file exclusively"
    );

    let (status, body) = send_delete(&fx.router).await;
    assert_eq!(status, StatusCode::OK, "delete must succeed: {body}");

    let reopened = redb::Database::open(&fx.redb);
    assert!(
        reopened.is_ok(),
        "a clone surviving DELETE must not keep index.redb open: {:?}",
        reopened.err()
    );
    drop(reopened);
    let indexer = surviving.indexer.read().await;
    assert!(indexer.has_vector_store(), "the store stays wired, closed");
    assert_eq!(
        indexer.vector_count().await,
        None,
        "the vector store must be closed, not still serving its mapping"
    );
}

/// #8232 Fail-Open Check: a surviving clone gets `IndexDeleted` from search
/// and from a write — never an empty result or a silent write.
#[tokio::test]
#[serial_test::serial]
async fn a_surviving_clone_gets_an_error_not_an_empty_result() {
    let _isolated = IsolatedDataDir::new();
    let fx = fixture().await;
    let surviving = Arc::clone(&fx.handle);
    let (status, body) = send_delete(&fx.router).await;
    assert_eq!(status, StatusCode::OK, "delete must succeed: {body}");

    let indexer = surviving.indexer.read().await;
    let search = indexer.search(&query()).await;
    let err = search.expect_err("a deleted index must refuse the search");
    assert!(err.downcast_ref::<IndexDeleted>().is_some(), "{err:#}");
    let write = indexer.index_file("src/new.rs", "pub fn late() {}\n").await;
    let err = write.expect_err("a deleted index must refuse the write");
    assert!(err.downcast_ref::<IndexDeleted>().is_some(), "{err:#}");
}

/// #8167: a corpus clone that outlives the close budget abandons the delete
/// with nothing changed, so a retry once it drops succeeds.
#[tokio::test]
#[serial_test::serial]
async fn a_corpus_still_referenced_elsewhere_abandons_the_delete() {
    let _isolated = IsolatedDataDir::new();
    let fx = fixture().await;
    let pinned = fx
        .handle
        .indexer
        .read()
        .await
        .corpus_store()
        .expect("colocated corpus");

    let (status, body) = send_delete(&fx.router).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert_eq!(body["removed"], false, "{body}");
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|e| e.contains("not closed")),
        "the failure must name the open file: {body}"
    );
    assert!(
        fx.registry.get(&IndexId::new(INDEX_ID)).is_some(),
        "an abandoned delete leaves the index registered"
    );
    let hits = fx.handle.indexer.read().await.search(&query()).await;
    assert!(
        hits.is_ok_and(|h| !h.is_empty()),
        "an abandoned delete leaves the index serving"
    );

    drop(pinned);
    let (status, body) = send_delete(&fx.router).await;
    assert_eq!(status, StatusCode::OK, "the retry must succeed: {body}");
    assert!(redb::Database::open(&fx.redb).is_ok());
}
