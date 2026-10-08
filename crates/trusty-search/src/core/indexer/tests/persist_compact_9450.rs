//! #9450: the incremental persister compacts a churned graph before it saves,
//! except during a staged reindex.
//!
//! Why: the persister is one of the two background points where churn past
//! the threshold gets a rebuilt graph (the idle write-cooldown persist is the
//! other, covered in `core::store::tests_9450`).
//! What: a colocated index whose store has churn above the threshold is
//! persisted twice — once while staging a reindex (no compaction), once after
//! (compaction, churn cleared, no vector lost).
//! Test: `the_persister_compacts_a_churned_graph_outside_a_reindex`.

use std::sync::Arc;

use crate::core::store::{UsearchStore, VectorStore as _};
use crate::core::CodeIndexer;
use crate::service::storage_layout::storage_layout_8438_tests::{wait_persist_task_done, Fixture};
use crate::service::storage_layout::StorageLayout;

/// Distinct, non-zero 4-dim vector for `i`.
fn vec4(i: usize) -> Vec<f32> {
    let t = i as f32;
    vec![t + 1.0, t.sin(), t.cos(), 1.0]
}

/// Why (#9450): see the module docs.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn the_persister_compacts_a_churned_graph_outside_a_reindex() {
    let fx = Fixture::new(false);
    let store = Arc::new(UsearchStore::new(4).expect("store"));
    let items: Vec<(String, Vec<f32>)> = (0..200).map(|i| (format!("c{i}"), vec4(i))).collect();
    store.upsert_batch(&items).await.expect("upsert");
    for i in 0..60 {
        store.remove(&format!("c{i}")).await.expect("remove");
    }
    // 60 churn against a threshold of max(140 / 10, 32) = 32.
    assert_eq!(store.churn_since_compact(), 60);

    let mut indexer = CodeIndexer::new("ts-9450-persist", fx.root.path())
        .with_storage_layout(StorageLayout::Colocated);
    indexer.set_store(store.clone());

    indexer.begin_reindex_staging();
    indexer.force_incremental_persist();
    assert!(wait_persist_task_done(&indexer).await, "persist finishes");
    assert_eq!(
        store.churn_since_compact(),
        60,
        "a reindex checkpoint does not compact"
    );
    indexer.end_reindex_staging();

    indexer.force_incremental_persist();
    assert!(wait_persist_task_done(&indexer).await, "persist finishes");
    assert_eq!(
        store.churn_since_compact(),
        0,
        "#9450: the persister compacts a churned graph before saving"
    );
    assert_eq!(store.len().await.expect("len"), 140, "no vector lost");
}
