//! #9450: the incremental persister compacts a churned graph before it saves,
//! except during a staged reindex.
//!
//! Why: the persister is one of the two background points where churn past
//! the threshold gets a rebuilt graph (the idle write-cooldown persist is the
//! other, covered in `core::store::tests_9450`).
//! What: a colocated index whose store has churn above the threshold is
//! persisted twice — once while staging a reindex (no compaction), once after
//! (compaction, churn cleared, no vector lost).
//! The idle write-cooldown persist skips the compaction the same way. A
//! commit-spawned persist that meets unquiet writes compacts once they stop.
//! Test: `the_persister_compacts_a_churned_graph_outside_a_reindex`,
//! `the_idle_persist_never_compacts_during_a_reindex`,
//! `a_commit_spawned_persist_compacts_once_writes_stop_with_the_idle_persist_off`.

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

    // #9450: an `IfDue` compaction starts only once writes are quiet.
    tokio::time::sleep(store.compaction_quiet_window()).await;
    indexer.force_incremental_persist();
    assert!(wait_persist_task_done(&indexer).await, "persist finishes");
    assert_eq!(
        store.churn_since_compact(),
        0,
        "#9450: the persister compacts a churned graph before saving"
    );
    assert_eq!(store.len().await.expect("len"), 140, "no vector lost");
}

/// #9450 fix round 3: a persist spawned by a commit, while writes are not yet
/// quiet, still gets the churned graph compacted once writes stop, with no
/// idle persist and no later persist.
/// Why: `TRUSTY_HNSW_DEMOTE_COOLDOWN_SECS=off` or `TRUSTY_HNSW_REVIEW_IDLE=off`
/// turns the idle write-cooldown persist off, which left the commit-spawned
/// persist as the only in-process compaction, and it always met unquiet
/// writes.
/// What: churn past the threshold, then `HNSW_SNAPSHOT_BATCH_INTERVAL` real
/// commits, the last of which spawns the persist. The test runs no idle
/// persist (the state either switch produces; the env vars are not set here
/// because this binary's other tests read them) and never sleeps the quiet
/// window: it polls the churn count.
/// Red before the fix: churn stays at 60.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn a_commit_spawned_persist_compacts_once_writes_stop_with_the_idle_persist_off() {
    let fx = Fixture::new(false);
    let store = Arc::new(UsearchStore::new(4).expect("store"));
    let items: Vec<(String, Vec<f32>)> = (0..200).map(|i| (format!("c{i}"), vec4(i))).collect();
    store.upsert_batch(&items).await.expect("upsert");
    for i in 0..60 {
        store.remove(&format!("c{i}")).await.expect("remove");
    }
    let mut indexer = CodeIndexer::new("ts-9450-quiet", fx.root.path())
        .with_storage_layout(StorageLayout::Colocated);
    indexer.set_store(store.clone());
    // What the idle ticker passes for `TRUSTY_HNSW_DEMOTE_COOLDOWN_SECS=off`.
    assert!(indexer
        .write_cooldown_persist(std::time::Duration::ZERO)
        .is_none());

    let batches = crate::core::indexer::HNSW_SNAPSHOT_BATCH_INTERVAL as usize;
    for b in 0..batches {
        let parsed = crate::core::indexer::ParsedBatch {
            chunks: vec![super::raw(&format!("n{b}"), "src/n.rs", "fn n() {}")],
            embeddings: vec![Some(vec4(1000 + b))],
            entities_by_file: vec![],
            parse_ms: 0,
            embed_ms: 0,
            vector_count: 1,
        };
        indexer
            .commit_parsed_batch(parsed, true)
            .await
            .expect("commit");
    }
    // 60 churn against a threshold of max(156 / 10, 32) = 32.
    assert_eq!(store.churn_since_compact(), 60);
    assert!(wait_persist_task_done(&indexer).await, "persist finishes");
    assert_eq!(
        store.churn_since_compact(),
        60,
        "precondition: the persist met unquiet writes and did not compact"
    );

    let deadline = tokio::time::Instant::now()
        + store.compaction_quiet_window()
        + std::time::Duration::from_secs(10);
    while store.churn_since_compact() != 0 && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(
        store.churn_since_compact(),
        0,
        "#9450: churn past the threshold is compacted once writes stop"
    );
    assert_eq!(store.len().await.expect("len"), 156, "no vector lost");
}

/// #9450 fix round: the idle write-cooldown persist never compacts while a
/// staged reindex runs, and does once it ends.
/// Red before the fix: the idle path compacted whatever the reindex state.
/// Test: this test.
#[tokio::test]
async fn the_idle_persist_never_compacts_during_a_reindex() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("hnsw.usearch");
    let store = Arc::new(UsearchStore::new(4).expect("store"));
    let items: Vec<(String, Vec<f32>)> = (0..200).map(|i| (format!("c{i}"), vec4(i))).collect();
    store.upsert_batch(&items).await.expect("upsert");
    store.save(&path).await.expect("save");
    for i in 0..60 {
        store.remove(&format!("c{i}")).await.expect("remove");
    }
    // 60 churn against a threshold of max(140 / 10, 32) = 32.
    assert_eq!(store.churn_since_compact(), 60);

    let mut indexer = CodeIndexer::new("ts-9450-idle", dir.path());
    indexer.set_store(store.clone());
    let cooldown = std::time::Duration::from_millis(1);

    indexer.begin_reindex_staging();
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert!(
        indexer
            .persist_and_demote_vector_store_after_write_cooldown(cooldown)
            .await,
        "the persist itself still runs"
    );
    assert_eq!(
        store.churn_since_compact(),
        60,
        "#9450: no compaction during a staged reindex"
    );
    indexer.end_reindex_staging();

    store.remove("c60").await.expect("remove");
    // #9450: an `IfDue` compaction starts only once writes are quiet.
    tokio::time::sleep(store.compaction_quiet_window()).await;
    assert!(
        indexer
            .persist_and_demote_vector_store_after_write_cooldown(cooldown)
            .await
    );
    assert_eq!(store.churn_since_compact(), 0, "compacts once it ends");
    assert_eq!(store.len().await.expect("len"), 139, "no vector lost");
}
