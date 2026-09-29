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
        assert_eq!(
            semantic_vector_gap(settled, 10, Some(3)),
            None,
            "{settled:?}"
        );
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
    let (handle, total) = handle_over_unreadable_store("vector-gap-8863-unreadable").await;
    *handle.stages.write().await = stages_after_a_discarded_snapshot(total);
    assert_fails_closed_on_unreadable_store(&handle).await;
}

/// Why (#8863 review): a `ready` stage over a store whose size read errors
/// cannot be confirmed ready. It used to hit `semantic_vector_gap`'s
/// `vector_count?` early return and stay `ready` with the fault hidden; it must
/// fail closed with the same reason the `pending` case gives.
/// Test: this test.
#[tokio::test]
async fn an_unreadable_store_fails_a_ready_stage_closed() {
    let (handle, _) = handle_over_unreadable_store("vector-gap-8863-unreadable-ready").await;
    handle.stages.write().await.semantic.status = StageStatus::Ready;
    assert_fails_closed_on_unreadable_store(&handle).await;
}

/// A handle whose corpus holds every chunk of [`SOURCE`] over an
/// [`UnreadableStore`], with an embedder wired.
async fn handle_over_unreadable_store(id: &str) -> (Arc<IndexHandle>, usize) {
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(DIM));
    let store: Arc<dyn VectorStore> = Arc::new(UnreadableStore);
    let indexer = CodeIndexer::new(id, "/tmp/vector-gap-8863").with_components(embedder, store);
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
        IndexId::new(id),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        std::path::PathBuf::from("/tmp/vector-gap-8863"),
    ));
    (handle, total)
}

/// The reconcile queues nothing over an unreadable store and leaves the stage
/// `Failed`, naming the unreadable size as the reason.
async fn assert_fails_closed_on_unreadable_store(handle: &Arc<IndexHandle>) {
    assert!(
        !reconcile_semantic_vector_gap(handle).await,
        "nothing can be queued over a store that cannot be read"
    );
    let semantic = handle.stages.read().await.semantic.clone();
    assert_eq!(
        semantic.status,
        StageStatus::Failed,
        "a stage over an unreadable store must fail closed: {semantic:?}"
    );
    assert!(
        semantic
            .failure
            .as_deref()
            .is_some_and(|r| r.contains("could not be read")),
        "the failure must name why the embed was not scheduled: {semantic:?}"
    );
}

/// Why (#8863): a gap with no embedder wired has no pass that can close it. It
/// used to log a warning and leave the stage `pending`, which is exactly the
/// never-started state #8863 reports; it must be a named, terminal `failed`.
/// Test: this test.
#[tokio::test]
async fn a_gap_with_no_embedder_fails_the_stage_with_a_reason() {
    let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(DIM).expect("usearch"));
    let mut indexer = CodeIndexer::new("vector-gap-8863-no-embedder", "/tmp/vector-gap-8863");
    indexer.set_store(store);
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
        IndexId::new("vector-gap-8863-no-embedder"),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        std::path::PathBuf::from("/tmp/vector-gap-8863"),
    ));
    *handle.stages.write().await = stages_after_a_discarded_snapshot(total);
    assert!(!handle.indexer.read().await.has_embedder(), "sanity");

    assert!(
        !reconcile_semantic_vector_gap(&handle).await,
        "nothing can be queued without an embedder"
    );
    let semantic = handle.stages.read().await.semantic.clone();
    assert_eq!(
        semantic.status,
        StageStatus::Failed,
        "a gap no pass can close must be terminal, not pending: {semantic:?}"
    );
    assert!(
        semantic
            .failure
            .as_deref()
            .is_some_and(|r| r.contains("no embedder wired")),
        "the failure must name the missing embedder: {semantic:?}"
    );
}

/// Why (#8884): a deferred pass whose plan cannot see a corpus row finishes
/// without error. Publishing `Ready` then hides that row's missing vector
/// until a restart, which is the fail-open settle this issue reported.
/// What: commits [`SOURCE`] through the indexer, writes one more chunk straight
/// into the durable corpus so the chunk map never sees it, runs the pass, and
/// requires `Failed` naming the gap, never `Ready`.
/// Test: this test.
#[tokio::test]
async fn a_deferred_pass_that_leaves_chunks_unembedded_is_not_ready() {
    let id = "vector-gap-8884-hidden-row";
    let dir = tempfile::tempdir().expect("tempdir");
    let corpus = Arc::new(
        crate::core::corpus::CorpusStore::open(&dir.path().join("index.redb")).expect("corpus"),
    );
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(DIM));
    let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(DIM).expect("usearch"));
    let mut indexer = CodeIndexer::new(id, dir.path()).with_components(embedder, store);
    indexer.set_corpus_store(Arc::clone(&corpus));
    let (chunks, _) = chunk_ast("src/lib.rs", SOURCE);
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
    let (hidden, _) = chunk_ast("src/hidden.rs", "pub fn hidden() -> u32 {\n    3\n}\n");
    corpus
        .upsert_chunks(&hidden)
        .expect("write a row around the chunk map");
    let handle = Arc::new(IndexHandle::bare(
        IndexId::new(id),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        dir.path().to_path_buf(),
    ));

    crate::service::reindex::run_embed_catch_up(
        Arc::clone(&handle),
        Arc::new(crate::service::reindex::ReindexProgress::new()),
    )
    .await;

    let semantic = handle.stages.read().await.semantic.clone();
    assert_eq!(
        semantic.status,
        StageStatus::Failed,
        "#8884: a pass that left a corpus chunk without a vector must not read ready: \
         {semantic:?}"
    );
    assert!(
        semantic
            .failure
            .as_deref()
            .is_some_and(|r| r.contains("still without a vector")),
        "the failure must name the gap: {semantic:?}"
    );
}

/// An embedder that returns an all-zero vector for any text naming `beta`,
/// which `commit_vectors_batch` refuses on every pass (#764).
struct ZeroForBeta(MockEmbedder);

#[async_trait::async_trait]
impl Embedder for ZeroForBeta {
    async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        Ok(self.embed_batch(&[text]).await?.remove(0))
    }
    async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        let mut out = Embedder::embed_batch(&self.0, texts).await?;
        for (text, vector) in texts.iter().zip(out.iter_mut()) {
            if text.contains("beta") {
                *vector = vec![0.0; DIM];
            }
        }
        Ok(out)
    }
    fn dimension(&self) -> usize {
        DIM
    }
}

/// A handle over a real corpus holding [`SOURCE`], committed through the
/// indexer so the chunk map sees every row. Returns the chunk count, the
/// corpus, and the tempdir that owns it.
async fn corpus_handle(
    id: &str,
    embedder: Arc<dyn Embedder>,
    store: Arc<dyn VectorStore>,
) -> (
    Arc<IndexHandle>,
    usize,
    Arc<crate::core::corpus::CorpusStore>,
    tempfile::TempDir,
) {
    let dir = tempfile::tempdir().expect("tempdir");
    let corpus = Arc::new(
        crate::core::corpus::CorpusStore::open(&dir.path().join("index.redb")).expect("corpus"),
    );
    let mut indexer = CodeIndexer::new(id, dir.path()).with_components(embedder, store);
    indexer.set_corpus_store(Arc::clone(&corpus));
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
        IndexId::new(id),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        dir.path().to_path_buf(),
    ));
    (handle, total, corpus, dir)
}

/// Run one deferred-embed pass to its settle and return the semantic stage.
async fn run_pass(handle: &Arc<IndexHandle>) -> StageState {
    crate::service::reindex::run_embed_catch_up(
        Arc::clone(handle),
        Arc::new(crate::service::reindex::ReindexProgress::new()),
    )
    .await;
    handle.stages.read().await.semantic.clone()
}

/// A corpus row no chunk-map entry and no vector covers: what the #8884
/// adoption left, and what `remove_file` leaves between its vector removal
/// and its redb delete.
fn write_hidden_row(corpus: &crate::core::corpus::CorpusStore) -> String {
    let (hidden, _) = chunk_ast("src/hidden.rs", "pub fn hidden() -> u32 {\n    3\n}\n");
    corpus
        .upsert_chunks(&hidden)
        .expect("write a row around the chunk map");
    hidden[0].id.clone()
}

/// Why (#8884 review): the store refuses a NaN or all-zero embedding on every
/// pass, so a count gap over such a chunk never closes and held the stage
/// `failed` for good, taking the index lexical-only.
/// What: an embedder zeroes the `beta` chunk; the pass must settle `Ready`
/// and name the refused chunk in `vectors_rejected`.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn a_rejected_embedding_is_reported_and_does_not_fail_the_stage() {
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroForBeta(MockEmbedder::new(DIM)));
    let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(DIM).expect("usearch"));
    let (handle, total, _corpus, _dir) =
        corpus_handle("vector-gap-8884-rejected", embedder, store).await;

    let semantic = run_pass(&handle).await;
    assert_eq!(
        semantic.status,
        StageStatus::Ready,
        "a refused embedding is not a gap a pass can close: {semantic:?}"
    );
    assert!(
        semantic
            .vectors_rejected
            .is_some_and(|n| n >= 1 && n < total),
        "the refused chunk must be reported on the stage: {semantic:?}"
    );
}

/// Why (#8884 review): the gap is measured by id. An orphan vector used to
/// balance the count, so a chunk no pass planned read as covered.
/// What: a corpus row outside the chunk map plus one orphan vector; the counts
/// match, and the pass must still settle `Failed` naming the gap.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn a_never_attempted_chunk_fails_the_stage_even_when_the_counts_match() {
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(DIM));
    let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(DIM).expect("usearch"));
    let (handle, _, corpus, _dir) =
        corpus_handle("vector-gap-8884-orphan", embedder, Arc::clone(&store)).await;
    write_hidden_row(&corpus);
    store
        .upsert("src/orphan.rs:1:3", vec![0.5; DIM])
        .await
        .expect("orphan vector");

    let semantic = run_pass(&handle).await;
    assert_eq!(
        semantic.status,
        StageStatus::Failed,
        "a chunk no pass attempted must not read ready: {semantic:?}"
    );
    assert!(
        semantic
            .failure
            .as_deref()
            .is_some_and(|r| r.contains("1 of") && r.contains("still without a vector")),
        "the failure must name the gap: {semantic:?}"
    );
}

/// Why (#8884 review): `remove_file` removes a vector before its redb row,
/// under the indexer read guard. A settle that reads between the two saw a
/// gap the removal was about to close, and failed the stage.
/// What: a remover holds the read guard over a row whose vector is already
/// gone, and deletes the row only once the stage settles or two seconds pass.
/// The pass must wait it out and settle `Ready`, never `Failed`.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn a_removal_racing_the_settle_does_not_fail_the_stage() {
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(DIM));
    let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(DIM).expect("usearch"));
    let (handle, _, corpus, _dir) = corpus_handle("vector-gap-8884-race", embedder, store).await;
    let removing = write_hidden_row(&corpus);

    let guard = Arc::clone(&handle.indexer).read_owned().await;
    let stages = Arc::clone(&handle.stages);
    let remover = tokio::spawn(async move {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline {
            let status = stages.read().await.semantic.status;
            if matches!(status, StageStatus::Ready | StageStatus::Failed) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        corpus.delete_chunks(&[removing]).expect("delete the row");
        drop(guard);
    });

    let semantic = run_pass(&handle).await;
    remover.await.expect("remover");
    assert_eq!(
        semantic.status,
        StageStatus::Ready,
        "an in-flight removal is not a vector gap: {semantic:?}"
    );
}

/// Why (#8884 review, mirrors #8863): a store whose size read errors cannot
/// confirm coverage, and the settle used to fall open to `Ready`, clearing the
/// pending marker.
/// What: runs the pass over [`UnreadableStore`] and requires `Failed` naming
/// the unreadable size.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn an_unreadable_store_does_not_let_a_pass_settle_ready() {
    let (handle, _) = handle_over_unreadable_store("vector-gap-8884-unreadable").await;
    let semantic = run_pass(&handle).await;
    assert_eq!(
        semantic.status,
        StageStatus::Failed,
        "an unreadable store must not settle ready: {semantic:?}"
    );
    assert!(
        semantic
            .failure
            .as_deref()
            .is_some_and(|r| r.contains("could not be read")),
        "the failure must name the unreadable store: {semantic:?}"
    );
}
