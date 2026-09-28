//! Tests for the #8726 semantic-stage vector-gap reconcile.
//!
//! Why: the reconcile is what turns "the store holds fewer vectors than the
//! corpus holds chunks" from a silent `ready` into owed, queued work.
//! What: the pure rule, then the live handle both ways — short of vectors and
//! fully covered.
//! Test: this file.

use std::sync::Arc;
use std::time::Duration;

use super::*;
use crate::core::chunker::chunk_ast;
use crate::core::embed::{Embedder, MockEmbedder};
use crate::core::indexer::{CodeIndexer, ParsedBatch};
use crate::core::registry::IndexId;
use crate::core::store::{UsearchStore, VectorStore};
use crate::service::reindex::deferred_embed_queue_depth;

const DIM: usize = 8;
const SOURCE: &str = "pub fn alpha() -> u32 {\n    1\n}\n\npub fn beta() -> u32 {\n    2\n}\n";

/// Why (#8726, #8863): a `Ready` or `Pending` stage over a wired, short store
/// is a gap; a stage with a pass in flight or a terminal verdict is not.
/// Test: this test.
#[test]
fn gap_is_reported_for_a_ready_or_pending_stage_short_of_vectors() {
    use StageStatus::*;
    for owed in [Ready, Pending] {
        assert_eq!(semantic_vector_gap(owed, 12814, Some(11979)), Some(835));
        assert_eq!(semantic_vector_gap(owed, 10, Some(10)), None);
        assert_eq!(semantic_vector_gap(owed, 10, Some(12)), None, "orphans");
        assert_eq!(semantic_vector_gap(owed, 10, None), None, "no store");
    }
    for settled in [InProgress, Failed, Skipped] {
        assert_eq!(semantic_vector_gap(settled, 10, Some(3)), None, "{settled:?}");
    }
}

/// A handle whose corpus holds every chunk of [`SOURCE`] and whose store holds
/// a vector for only the first `vectors` of them, with `semantic` `Ready`.
async fn handle_with_vectors(id: &str, vectors: usize) -> (Arc<IndexHandle>, usize) {
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(DIM));
    let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(DIM).expect("usearch"));
    let indexer =
        CodeIndexer::new(id, "/tmp/vector-gap-8726").with_components(embedder, Arc::clone(&store));
    let (chunks, _) = chunk_ast("src/lib.rs", SOURCE);
    let total = chunks.len();
    assert!(total >= 2, "fixture must carry at least two chunks");
    let seeded: Vec<String> = chunks.iter().take(vectors).map(|c| c.id.clone()).collect();
    indexer
        .commit_parsed_batch(
            ParsedBatch {
                embeddings: vec![None; chunks.len()],
                chunks,
                entities_by_file: vec![],
                parse_ms: 0,
                embed_ms: 0,
                vector_count: 0,
            },
            false,
        )
        .await
        .expect("commit");
    for chunk_id in &seeded {
        store
            .upsert(chunk_id, vec![0.5; DIM])
            .await
            .expect("seed vector");
    }
    let handle = Arc::new(IndexHandle::bare(
        IndexId::new(id),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        std::path::PathBuf::from("/tmp/vector-gap-8726"),
    ));
    handle.stages.write().await.semantic.status = StageStatus::Ready;
    (handle, total)
}

/// Why (#8726): a `Ready` stage short of vectors must stop reading `Ready` and
/// must get a backfill that closes the gap.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn a_gap_demotes_the_stage_and_queues_a_backfill() {
    let (handle, total) = handle_with_vectors("vector-gap-8726-short", 1).await;

    assert!(
        reconcile_semantic_vector_gap(&handle).await,
        "a gap with an embedder wired must queue a backfill"
    );
    assert_ne!(
        handle.stages.read().await.semantic.status,
        StageStatus::Ready,
        "the stage must not read ready while 1 of {total} chunks has a vector"
    );

    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while handle.stages.read().await.semantic.status != StageStatus::Ready {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the backfill never settled the stage: {:?}",
            handle.stages.read().await.semantic
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        handle.indexer.read().await.vector_count().await,
        Some(total),
        "the backfill must close the gap before the stage reads ready"
    );
    while deferred_embed_queue_depth() > 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "queue never drained"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Why (#8726): a fully covered store is healthy — the reconcile must not
/// demote it or spend the background permit on it.
/// Test: this test.
#[tokio::test]
async fn no_gap_leaves_a_ready_stage_alone() {
    let (handle, total) = handle_with_vectors("vector-gap-8726-full", usize::MAX).await;
    assert_eq!(
        handle.indexer.read().await.vector_count().await,
        Some(total)
    );
    assert!(!reconcile_semantic_vector_gap(&handle).await);
    assert_eq!(
        handle.stages.read().await.semantic.status,
        StageStatus::Ready
    );
}

/// The stages lazy restore derives when it discards a torn HNSW snapshot over
/// a full corpus: lexical `Ready`, semantic `Pending` (#8863).
fn stages_after_a_discarded_snapshot(chunk_count: usize) -> crate::core::registry::IndexStages {
    crate::service::warm_boot::derive_warm_boot_stages(crate::service::warm_boot::WarmBootInputs {
        chunk_count,
        hnsw_snapshot_ready: false,
        graph_node_count: 0,
        lexical_only: false,
        skip_kg: false,
        skip_vector: false,
        corpus_open_failure: None,
    })
}

/// Why (#8863): lazy restore discarded a torn snapshot, derived `pending` over
/// 8223 chunks and an empty store, and nothing queued the embed — the stage sat
/// at `pending` with 0 vectors through the watcher's rescan walk until a manual
/// reindex. The restore's reconcile must schedule the backfill and drive it to
/// `Ready` with vectors == chunks.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn a_pending_stage_left_by_a_discarded_snapshot_is_backfilled() {
    let (handle, total) = handle_with_vectors("vector-gap-8863-pending", 0).await;
    *handle.stages.write().await = stages_after_a_discarded_snapshot(total);
    assert_eq!(
        handle.stages.read().await.semantic.status,
        StageStatus::Pending,
        "sanity: restore derives pending when the snapshot was discarded"
    );
    assert_eq!(handle.indexer.read().await.vector_count().await, Some(0));

    assert!(
        reconcile_semantic_vector_gap(&handle).await,
        "a pending stage over {total} chunks and 0 vectors must get a backfill queued"
    );
    assert_ne!(
        handle.stages.read().await.semantic.status,
        StageStatus::Pending,
        "a queued pass must not read pending"
    );

    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while handle.stages.read().await.semantic.status != StageStatus::Ready {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the backfill never settled the stage: {:?}",
            handle.stages.read().await.semantic
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        handle.indexer.read().await.vector_count().await,
        Some(total),
        "ready must mean every chunk has a vector"
    );
    while deferred_embed_queue_depth() > 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "queue never drained"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A store whose size read always fails, as a closed store's does.
struct UnreadableStore;

#[async_trait::async_trait]
impl VectorStore for UnreadableStore {
    async fn upsert(&self, _id: &str, _embedding: Vec<f32>) -> anyhow::Result<()> {
        Ok(())
    }
    async fn search(
        &self,
        _query: &[f32],
        _top_k: usize,
    ) -> anyhow::Result<Vec<crate::core::store::VectorHit>> {
        Ok(Vec::new())
    }
    async fn remove(&self, _id: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn len(&self) -> anyhow::Result<usize> {
        anyhow::bail!("store is closed")
    }
}

/// Why (#8863): when the reconcile cannot decide — the store's size read
/// errors — a `pending` stage must fail closed with the reason on the stage,
/// not stay `pending` with nothing scheduled.
/// Test: this test.
#[tokio::test]
async fn an_unreadable_store_fails_a_pending_stage_closed() {
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(DIM));
    let store: Arc<dyn VectorStore> = Arc::new(UnreadableStore);
    let indexer = CodeIndexer::new("vector-gap-8863-unreadable", "/tmp/vector-gap-8863")
        .with_components(embedder, store);
    let (chunks, _) = chunk_ast("src/lib.rs", SOURCE);
    let total = chunks.len();
    indexer
        .commit_parsed_batch(
            ParsedBatch {
                embeddings: vec![None; chunks.len()],
                chunks,
                entities_by_file: vec![],
                parse_ms: 0,
                embed_ms: 0,
                vector_count: 0,
            },
            false,
        )
        .await
        .expect("commit");
    let handle = Arc::new(IndexHandle::bare(
        IndexId::new("vector-gap-8863-unreadable"),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        std::path::PathBuf::from("/tmp/vector-gap-8863"),
    ));
    *handle.stages.write().await = stages_after_a_discarded_snapshot(total);

    assert!(
        !reconcile_semantic_vector_gap(&handle).await,
        "nothing can be queued over a store that cannot be read"
    );
    let semantic = handle.stages.read().await.semantic.clone();
    assert_eq!(
        semantic.status,
        StageStatus::Failed,
        "an unschedulable stage must be terminal, not pending: {semantic:?}"
    );
    assert!(
        semantic
            .failure
            .as_deref()
            .is_some_and(|r| r.contains("could not be read")),
        "the failure must name why the embed was not scheduled: {semantic:?}"
    );
}
