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

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tokio::sync::RwLock;
use tower::ServiceExt;
use trusty_common::embedder::MockEmbedder;

use super::build_router;
use super::delete_close::{close_index_files, CLOSE_BUDGET};
use super::tests_components::IsolatedDataDir;
use crate::core::indexer::TEST_REHYDRATE_DELAY_MS;
use crate::core::indexer::{IndexDeleted, SearchQuery};
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
use crate::core::Embedder;
use crate::service::persistence::{
    corpus_redb_path_for_entry, hnsw_path_for_entry, PersistedIndex,
};
use crate::service::persistence_loader::build_indexer_from_entry;
use crate::service::reindex::index_teardown_lock;
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

/// Poll until `index.redb` can be opened, i.e. this process closed it.
async fn redb_closes_within(redb: &std::path::Path, bound: Duration) -> bool {
    let deadline = std::time::Instant::now() + bound;
    while std::time::Instant::now() < deadline {
        if redb::Database::open(redb).is_ok() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    false
}

/// #8167: a `delete_data=false` delete whose writer outlives the quiesce wait
/// answers `quiesced: false`, and closes the files once the writer finishes.
/// A second delete cannot do it: the id is already deregistered.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn a_writer_outliving_the_quiesce_wait_closes_the_files_when_it_finishes() {
    let _isolated = IsolatedDataDir::new();
    let fx = fixture().await;
    let writer = index_teardown_lock(&IndexId::new(INDEX_ID))
        .read_owned()
        .await;

    let (status, body) = send_delete(&fx.router).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["quiesced"], false,
        "the writer must outlive the wait: {body}"
    );
    assert!(
        redb::Database::open(&fx.redb).is_err(),
        "nothing may close the files while the writer still runs"
    );

    drop(writer);
    assert!(
        redb_closes_within(&fx.redb, Duration::from_secs(5)).await,
        "index.redb must close once the writer that outlived the delete finishes"
    );
    assert_eq!(
        fx.handle.indexer.read().await.vector_count().await,
        None,
        "the vector store must be closed too"
    );
}

/// Resets the artificial rehydrate delay, even when an assertion panics.
struct RehydrateDelay;

impl RehydrateDelay {
    fn set(ms: u64) -> Self {
        TEST_REHYDRATE_DELAY_MS.store(ms, Ordering::Relaxed);
        Self
    }
}

impl Drop for RehydrateDelay {
    fn drop(&mut self) {
        TEST_REHYDRATE_DELAY_MS.store(0, Ordering::Relaxed);
    }
}

/// Evict the fixture's caches and leave a detached rehydrate (#3683) holding
/// its corpus clone for `SLOW_SCAN_MS`.
const SLOW_SCAN_MS: u64 = 800;

async fn start_slow_rehydrate(handle: &Arc<IndexHandle>) -> RehydrateDelay {
    let delay = RehydrateDelay::set(SLOW_SCAN_MS);
    let indexer = handle.indexer.read().await;
    assert!(
        indexer.reclaim_memory_now().await > 0,
        "precondition: warm caches"
    );
    // The search starts the rehydrate; dropping it on timeout leaves the
    // detached scan running, as a query-timeout 408 does.
    let _ = tokio::time::timeout(Duration::from_millis(50), indexer.search(&query())).await;
    assert!(
        indexer.rehydrate_gate().in_flight(),
        "precondition: the rehydrate scan is running"
    );
    delay
}

/// #3683 vs #8167: a delete landing during a slow corpus rehydrate waits the
/// scan out WITHOUT the indexer write lock — searches keep running — and then
/// succeeds. Before, it held the write lock through the close budget and
/// answered 500.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn a_delete_during_a_slow_rehydrate_waits_without_the_write_lock() {
    let _isolated = IsolatedDataDir::new();
    let fx = fixture().await;
    let _delay = start_slow_rehydrate(&fx.handle).await;

    let router = fx.router.clone();
    let delete = tokio::spawn(async move { send_delete(&router).await });
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(
        fx.handle.indexer.try_read().is_ok(),
        "a delete waiting out a rehydrate must not hold the indexer write lock"
    );

    let (status, body) = delete.await.expect("delete task");
    assert_eq!(status, StatusCode::OK, "the delete must succeed: {body}");
    assert!(
        redb::Database::open(&fx.redb).is_ok(),
        "index.redb must be closed"
    );
}

/// #3683 vs #8167: a rehydrate that outlasts the delete's rehydrate budget
/// abandons the delete with a retryable reason and nothing changed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn a_rehydrate_outlasting_its_budget_abandons_the_delete_unchanged() {
    let _isolated = IsolatedDataDir::new();
    let fx = fixture().await;
    let _delay = start_slow_rehydrate(&fx.handle).await;

    let started = std::time::Instant::now();
    let reason = close_index_files(Some(&fx.handle), Duration::from_millis(100), CLOSE_BUDGET)
        .await
        .expect_err("a rehydrate still scanning must abandon the close");
    assert!(
        reason.contains("rehydrate"),
        "the reason must name the scan: {reason}"
    );
    assert!(
        started.elapsed() < Duration::from_millis(SLOW_SCAN_MS),
        "the abandon must come at the rehydrate budget, not after the scan"
    );
    assert!(
        !fx.handle.indexer.read().await.is_deleted(),
        "nothing was changed"
    );
    assert!(
        !redb_closes_within(&fx.redb, Duration::from_millis(200)).await,
        "the index keeps its corpus open"
    );
}

/// #8167 LOW: a snapshot write reaching a deleted index is skipped quietly,
/// not reported as the #7920 staged-swap ERROR, and is not counted as a
/// quarantine refusal.
#[tokio::test]
#[serial_test::serial]
async fn a_deleted_index_skips_snapshot_writes_without_the_swap_error() {
    use tracing_subscriber::layer::SubscriberExt as _;
    use trusty_common::error_capture::{BugCaptureLayer, ErrorStore};

    let isolated = IsolatedDataDir::new();
    let fx = fixture().await;
    let surviving = Arc::clone(&fx.handle);
    let (status, body) = send_delete(&fx.router).await;
    assert_eq!(status, StatusCode::OK, "delete must succeed: {body}");

    let store = ErrorStore::with_path(Some(isolated.path().join("errors.jsonl")), 64);
    let layer = BugCaptureLayer::new(store.clone(), env!("CARGO_PKG_VERSION"));
    let _guard = tracing::subscriber::set_default(tracing_subscriber::registry().with(layer));
    let indexer = surviving.indexer.read().await;
    indexer.force_incremental_persist();

    let captured = store.recent_errors(16);
    assert!(
        !captured.iter().any(|e| e.message.contains("#7920")),
        "a deletion must not be logged as a staged reindex swap: {captured:?}"
    );
    assert_eq!(indexer.refused_incremental_writes(), 0);
}
