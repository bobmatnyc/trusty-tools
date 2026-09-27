//! `UsearchStore::close` releases the snapshot view and refuses later use
//! (#8167, #8232).
//!
//! Why: the delete path closes the store through one `Arc` while other clones
//! survive. Every surviving clone must then get an error — an empty answer
//! would read as "no matches", and a save would overwrite the preserved
//! snapshot with the empty, reset graph.
//! What: views a real snapshot, closes it through one clone, and drives the
//! other clone through search, write and save.
//! Test: this module. The truncate-under-mapping crash is proven in its own
//! binary, `tests/delete_releases_mmap_8232.rs`, because a SIGBUS would take
//! the whole lib test binary down with it.

use std::sync::Arc;

use super::types::VectorStore;
use super::usearch_store::UsearchStore;
use crate::core::chunk_id::ChunkIdShapes;

/// #8232: a closed store leaves view mode, errors on every use, and never
/// rewrites the snapshot it was serving.
#[tokio::test]
async fn close_unmaps_the_view_and_refuses_every_later_use() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("hnsw.usearch");
    let seed = UsearchStore::new(4).expect("store");
    seed.upsert("a", vec![1.0, 0.0, 0.0, 0.0])
        .await
        .expect("upsert");
    seed.upsert("b", vec![0.0, 1.0, 0.0, 0.0])
        .await
        .expect("upsert");
    seed.save(&path).await.expect("save");
    drop(seed);
    let snapshot_before = std::fs::read(&path).expect("read snapshot");

    let store: Arc<dyn VectorStore> = Arc::new(
        UsearchStore::load_from(&path)
            .await
            .expect("load")
            .expect("snapshot present"),
    );
    let surviving = Arc::clone(&store);
    let query = [1.0, 0.0, 0.0, 0.0];
    assert!(
        !surviving
            .search(&query, 2)
            .await
            .expect("open search")
            .is_empty(),
        "precondition: the viewed snapshot answers"
    );

    store.close().await.expect("close");
    store.close().await.expect("a second close is a no-op");

    for (op, err) in [
        ("search", surviving.search(&query, 2).await.err()),
        (
            "search_filtered",
            surviving
                .search_filtered(&query, 2, None, &[], ChunkIdShapes::NewOnly)
                .await
                .err(),
        ),
        ("len", surviving.len().await.err()),
        ("upsert", surviving.upsert("c", query.to_vec()).await.err()),
        ("remove", surviving.remove("a").await.err()),
        ("save", surviving.save_to(&path).await.err()),
    ] {
        let err = err.unwrap_or_else(|| panic!("{op} on a closed store must be an error"));
        assert!(
            format!("{err:#}").contains("deleted"),
            "{op} must say the index was deleted: {err:#}"
        );
    }
    assert!(
        !surviving.demote_to_view().await.expect("demote"),
        "the idle sweep must not map a closed store's file again"
    );
    assert_eq!(
        std::fs::read(&path).expect("reread snapshot"),
        snapshot_before,
        "closing must leave the on-disk snapshot byte-identical"
    );
}
