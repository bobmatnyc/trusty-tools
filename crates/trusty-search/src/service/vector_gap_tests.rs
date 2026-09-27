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

/// Why (#8726): only a `Ready` stage over a wired, short store is a gap.
/// Test: this test.
#[test]
fn gap_is_reported_only_for_a_ready_stage_short_of_vectors() {
    use StageStatus::*;
    assert_eq!(semantic_vector_gap(Ready, 12814, Some(11979)), Some(835));
    assert_eq!(semantic_vector_gap(Ready, 10, Some(10)), None);
    assert_eq!(semantic_vector_gap(Ready, 10, Some(12)), None, "orphans");
    assert_eq!(semantic_vector_gap(Ready, 10, None), None, "no store");
    for owed in [Pending, InProgress, Failed, Skipped] {
        assert_eq!(semantic_vector_gap(owed, 10, Some(3)), None, "{owed:?}");
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
