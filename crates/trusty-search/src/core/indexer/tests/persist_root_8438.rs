//! #8438 regression test: the incremental persist loop resolves its target on
//! every iteration, so a colocated root deleted mid-loop is never recreated.
//!
//! Why: the task resolved `hnsw_path` (and the staging path) once and reused
//! them across up to 8 coalesced saves; `UsearchStore::save` runs
//! `create_dir_all(parent)`, so a later iteration brought a deleted root back.
//! What: holds the chunk-map write lock so iteration 1 parks after its HNSW
//! save, sets `dirty`, deletes the root, then releases the lock.
//! Test: `persist_loop_never_recreates_a_root_deleted_between_iterations`.

use crate::service::storage_layout::storage_layout_8438_tests::{
    remove_root, wait_for, wait_persist_task_done, Fixture,
};
use crate::service::storage_layout::{StorageLayout, HNSW_FILE, HNSW_STAGING_FILE};

/// Why (#8438): see the module docs. Runs in staged-reindex mode, so iteration
/// 1 writes the staging path and iteration 2 must skip rather than fall back to
/// the live path or recreate the root through a path cached at spawn.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn persist_loop_never_recreates_a_root_deleted_between_iterations() {
    let fx = Fixture::new(false);
    let handle = fx.handle("ts-8438-loop", StorageLayout::Colocated).await;
    let indexer = handle.indexer.read().await;
    indexer.begin_reindex_staging();

    // Park iteration 1 after its HNSW save: the legacy chunks.json snapshot
    // (no corpus store is wired) takes a read lock on the chunk map.
    let chunks = indexer.chunks.clone();
    let parked = chunks.write().await;
    indexer.force_incremental_persist();
    let staging = fx.repo_dir().join(HNSW_STAGING_FILE);
    assert!(
        wait_for(&staging).await,
        "iteration 1 must write the staging snapshot while the root exists"
    );

    // A second commit lands (sets `dirty`; the in-flight task owns the loop),
    // then the root is deleted before iteration 2 resolves its target.
    indexer.force_incremental_persist();
    remove_root(&fx);
    drop(parked);

    assert!(
        wait_persist_task_done(&indexer).await,
        "the persist task must finish"
    );
    assert!(
        !fx.root.path().exists(),
        "#8438: a later coalesced iteration must never recreate a deleted root"
    );
    assert!(
        !fx.repo_dir().join(HNSW_FILE).exists(),
        "#8438: a refused staging path must never fall back to the live path"
    );
    assert!(
        indexer.persist_flags_for_tests().1,
        "a skipped checkpoint must leave `dirty` set"
    );
    indexer.end_reindex_staging();
}
