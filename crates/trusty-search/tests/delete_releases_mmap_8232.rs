//! #8232: replacing an index's files after `DELETE /indexes/{id}` must not
//! crash the daemon through a handle that outlived the delete.
//!
//! Why: the reported SIGBUS came from a deleted index whose `hnsw.usearch` was
//! still memory-mapped by a surviving `Arc<IndexHandle>` (a deferred-embed job,
//! an in-flight request). Truncating or replacing that file under the mapping
//! turns the next vector search into a bus error, which kills the whole
//! process — every index, not just the deleted one. A SIGBUS cannot be caught
//! by a test harness, so this lives in its own test binary: on pre-fix code
//! the binary dies by signal instead of reporting a failed assertion.
//! What: builds a real colocated index with a saved HNSW snapshot, reloads it
//! so the snapshot is served from the mmap view, keeps a second handle alive,
//! deletes the index through the real router, truncates `hnsw.usearch` to zero
//! bytes, and searches through the surviving handle. The search must return an
//! error that names the deletion.
//! Test: `cargo test -p trusty-search --test delete_releases_mmap_8232`.

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tokio::sync::RwLock;
use tower::ServiceExt;

use trusty_common::embedder::MockEmbedder;
use trusty_search::core::indexer::SearchQuery;
use trusty_search::core::registry::{IndexHandle, IndexId, IndexRegistry};
use trusty_search::core::Embedder;
use trusty_search::service::persistence::{hnsw_path_for_entry, PersistedIndex};
use trusty_search::service::persistence_loader::build_indexer_from_entry;
use trusty_search::service::server::{build_router, SearchAppState};

const INDEX_ID: &str = "delete-mmap-8232";

/// Colocated entry rooted at `root`, the layout the #8232 host used.
fn colocated_entry(root: PathBuf) -> PersistedIndex {
    let mut entry = PersistedIndex::new(INDEX_ID.to_string(), root);
    entry.colocated = true;
    entry
}

/// Index one file and save the HNSW snapshot, so a later reload maps it.
async fn seed_snapshot(entry: &PersistedIndex, embedder: &Arc<dyn Embedder>) {
    let indexer = build_indexer_from_entry(entry, embedder)
        .await
        .expect("build seed indexer");
    for n in 0..64 {
        let body = format!("pub fn handler_{n}() -> u32 {{ {n} }}\n");
        indexer
            .index_file(&format!("src/file_{n}.rs"), &body)
            .await
            .expect("index_file");
    }
    let hnsw = hnsw_path_for_entry(entry).expect("hnsw path");
    let saved = indexer.save_vector_store(&hnsw).await.expect("save hnsw");
    assert!(saved, "the seed pass must write {}", hnsw.display());
}

/// #8232: a search through a handle that outlived `DELETE` must fail cleanly
/// after the snapshot file is truncated — never SIGBUS the process.
#[tokio::test]
async fn a_search_through_a_surviving_handle_after_delete_and_truncate_errors() {
    let data_dir = tempfile::tempdir().expect("data dir");
    // SAFETY: the only test in this binary, so no other thread reads the env.
    unsafe { std::env::set_var("TRUSTY_DATA_DIR", data_dir.path()) };
    let root = tempfile::tempdir().expect("root");
    let entry = colocated_entry(root.path().to_path_buf());
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(8));
    seed_snapshot(&entry, &embedder).await;

    // Reload: `load_from` serves the snapshot from an mmap view by default.
    let indexer = build_indexer_from_entry(&entry, &embedder)
        .await
        .expect("reload indexer");
    let registry = IndexRegistry::new();
    let surviving = registry.register(IndexHandle::bare(
        IndexId::new(INDEX_ID),
        Arc::new(RwLock::new(indexer)),
        entry.root_path.clone(),
    ));
    let router = build_router(SearchAppState::new(registry));

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
    assert_eq!(resp.status(), StatusCode::OK, "the delete must succeed");

    // The replace-under-live-daemon step from the #8232 delivery recipe.
    let hnsw = hnsw_path_for_entry(&entry).expect("hnsw path");
    std::fs::OpenOptions::new()
        .write(true)
        .open(&hnsw)
        .expect("open snapshot")
        .set_len(0)
        .expect("truncate snapshot");

    let query = SearchQuery {
        text: "handler returns a number".to_string(),
        ..Default::default()
    };
    let outcome = surviving.indexer.read().await.search(&query).await;
    let err = match outcome {
        Ok(hits) => panic!(
            "a deleted index must refuse the search, not answer {} hit(s) — an empty \
             or stale answer reads as a real one",
            hits.len()
        ),
        Err(e) => e,
    };
    assert!(
        format!("{err:#}").contains("deleted"),
        "the refusal must say the index was deleted: {err:#}"
    );
    unsafe { std::env::remove_var("TRUSTY_DATA_DIR") };
}
