//! #8438 regression test for the shutdown flush.
//!
//! Why: the flush wrote the chunk corpus and HNSW snapshot into
//! `<root>/.trusty-search/` whenever that directory existed, and never created
//! the data dir the registry named.
//! What: a `colocated=false` index whose root holds an empty
//! `.trusty-search/` is flushed; the data dir must receive `hnsw.usearch` and
//! the repo directory must stay empty. A colocated index's flush must put
//! `chunks.json` in the data dir and only its HNSW snapshot in the repo.
//! Test: this file.

use super::*;
use crate::service::storage_layout::storage_layout_8438_tests::{remove_root, Fixture};
use crate::service::storage_layout::{StorageLayout, CHUNKS_JSON_FILE, HNSW_FILE};

/// Why (#8438): see the module docs.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn shutdown_flush_writes_the_data_dir_not_the_repo() {
    let fx = Fixture::new(true);
    let handle = fx.handle("ts-8438-shutdown", StorageLayout::DataDir).await;
    let id = handle.id.clone();
    let registry = crate::core::registry::IndexRegistry::new();
    registry.register(handle);
    let progress: Arc<DashMap<IndexId, Arc<ReindexProgress>>> = Arc::new(DashMap::new());

    flush_one_index_on_shutdown(
        &registry,
        &progress,
        id,
        ShutdownBudget::from_window(std::time::Duration::from_secs(65)),
    )
    .await;

    assert!(
        fx.data_index_dir("ts-8438-shutdown")
            .join(HNSW_FILE)
            .exists(),
        "the shutdown flush must write the data dir"
    );
    fx.assert_repo_dir_empty();
}

/// Why (#8438): the shutdown flush of a colocated index whose root was deleted
/// resolved `chunks.json` and `hnsw.usearch` through `create_dir_all`, which
/// recreated `<root>`; the next warm boot then loaded HNSW with no corpus.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn shutdown_flush_never_recreates_a_missing_colocated_root() {
    let fx = Fixture::new(false);
    let handle = fx.handle("ts-8438-gone-sd", StorageLayout::Colocated).await;
    let id = handle.id.clone();
    let registry = crate::core::registry::IndexRegistry::new();
    registry.register(handle);
    remove_root(&fx);
    let progress: Arc<DashMap<IndexId, Arc<ReindexProgress>>> = Arc::new(DashMap::new());

    flush_one_index_on_shutdown(
        &registry,
        &progress,
        id,
        ShutdownBudget::from_window(std::time::Duration::from_secs(65)),
    )
    .await;

    assert!(
        !fx.root.path().exists(),
        "#8438: the shutdown flush must never recreate a deleted colocated root"
    );
}

/// Why (#8438): `chunks.json` is a data-dir-only artifact for every layout —
/// its one reader looks nowhere else — so a colocated index's flush must not
/// put it in `<root>/.trusty-search/`, where only the HNSW snapshot belongs.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn colocated_shutdown_flush_writes_chunks_json_to_the_data_dir() {
    let fx = Fixture::new(false);
    let handle = fx
        .handle("ts-8438-coloc-sd", StorageLayout::Colocated)
        .await;
    let id = handle.id.clone();
    let registry = crate::core::registry::IndexRegistry::new();
    registry.register(handle);
    let progress: Arc<DashMap<IndexId, Arc<ReindexProgress>>> = Arc::new(DashMap::new());

    flush_one_index_on_shutdown(
        &registry,
        &progress,
        id,
        ShutdownBudget::from_window(std::time::Duration::from_secs(65)),
    )
    .await;

    assert!(
        fx.data_index_dir("ts-8438-coloc-sd")
            .join(CHUNKS_JSON_FILE)
            .exists(),
        "a colocated index's shutdown flush must write chunks.json to the data dir"
    );
    let repo_entries: Vec<String> = std::fs::read_dir(fx.repo_dir())
        .expect("the colocated flush creates <root>/.trusty-search/")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        repo_entries.iter().any(|n| n == HNSW_FILE),
        "the colocated HNSW snapshot belongs in <root>/.trusty-search/, found {repo_entries:?}"
    );
    assert!(
        repo_entries
            .iter()
            .all(|n| n == HNSW_FILE || n == "hnsw.keys.json"),
        "#8438: <root>/.trusty-search/ must hold only the HNSW snapshot, found {repo_entries:?}"
    );
}
