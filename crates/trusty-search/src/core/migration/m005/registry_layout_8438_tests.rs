//! #8438 regression test for M005's snapshot resolution.
//!
//! Why: M005 re-keyed and rewrote `<root>/.trusty-search/hnsw.usearch`
//! whenever that FILE existed, so a `colocated=false` index rewrote another
//! instance's snapshot in the repo.
//! What: the repo copy exists; the resolver must name the data dir, and
//! remapping through it must leave the repo copy byte-identical.
//! Test: this file.

use super::resolve_hnsw_path;
use crate::service::storage_layout::storage_layout_8438_tests::Fixture;
use crate::service::storage_layout::{StorageLayout, HNSW_FILE};

/// Why (#8438): see the module docs.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn m005_rewrites_the_registry_named_snapshot_not_the_repo_copy() {
    let fx = Fixture::new(true);
    let handle = fx.handle("ts-8438-m005", StorageLayout::DataDir).await;
    let repo_copy = fx.repo_dir().join(HNSW_FILE);
    std::fs::write(&repo_copy, b"another instance's snapshot").unwrap();

    let path = resolve_hnsw_path(&handle).await.expect("resolves");
    assert_eq!(path, fx.data_index_dir("ts-8438-m005").join(HNSW_FILE));

    let indexer = handle.indexer.read().await;
    indexer
        .remap_vector_store_keys(&path, &|id| Some(format!("{id}-remapped")))
        .await
        .expect("remap");
    assert_eq!(
        std::fs::read(&repo_copy).unwrap(),
        b"another instance's snapshot",
        "M005 must not touch the repo copy of a colocated=false index"
    );
}
