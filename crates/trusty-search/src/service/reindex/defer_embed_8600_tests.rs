//! #8600: a deferred-embed pass must let go of its index at shutdown, and must
//! not wait forever on an embedder that never answers.
//!
//! Why: a restart that landed mid-pass left the corpus held open, so the next
//! warm-boot fell back to a full cold start; and a pass waiting on a wedged
//! embedder held the one background permit with zero progress.
//! What: drives `run_embed_catch_up` against an embedder whose calls never
//! complete — once with a shutdown drain, once with a short no-progress
//! deadline — over an index backed by a real redb corpus.
//! Test: this file.

use super::run_embed_catch_up;
use crate::core::chunker::{ChunkType, RawChunk};
use crate::core::corpus::CorpusStore;
use crate::core::embed::Embedder;
use crate::core::indexer::{CodeIndexer, ParsedBatch};
use crate::core::registry::{IndexHandle, IndexId, StageStatus};
use crate::core::store::{UsearchStore, VectorStore};
use crate::service::reindex::ReindexProgress;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// An embedder whose calls are accepted and never answered.
struct NeverCompletingEmbedder {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl Embedder for NeverCompletingEmbedder {
    async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        std::future::pending().await
    }
    async fn embed_batch(&self, _texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        std::future::pending().await
    }
    fn dimension(&self) -> usize {
        8
    }
}

fn chunk(n: usize) -> RawChunk {
    RawChunk {
        id: format!("f{n}.rs:1:1"),
        file: format!("f{n}.rs"),
        start_line: 1,
        end_line: 1,
        content: format!("fn function_number_{n}() {{}}"),
        function_name: None,
        language: Some("rust".into()),
        chunk_type: ChunkType::Code,
        calls: vec![],
        inherits_from: vec![],
        chunk_depth: 0,
        parent_chunk_id: None,
        child_chunk_ids: vec![],
        nlp_keywords: vec![],
        nlp_code_refs: vec![],
        virtual_terms: vec![],
    }
}

/// An index whose three chunks live in a real redb corpus at `redb_path`, and
/// whose embedder never answers.
async fn handle_over_redb(
    id: &str,
    root: &std::path::Path,
    redb_path: &std::path::Path,
    calls: Arc<AtomicUsize>,
) -> Arc<IndexHandle> {
    let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(8).expect("usearch"));
    let mut indexer = CodeIndexer::new(id, root)
        .with_components(Arc::new(NeverCompletingEmbedder { calls }), store);
    indexer.set_corpus_store(Arc::new(CorpusStore::open(redb_path).expect("open corpus")));
    let parsed = ParsedBatch {
        chunks: (0..3).map(chunk).collect(),
        embeddings: vec![None, None, None],
        entities_by_file: vec![],
        parse_ms: 0,
        embed_ms: 0,
        vector_count: 0,
    };
    indexer
        .commit_parsed_batch(parsed, false)
        .await
        .expect("commit");
    Arc::new(IndexHandle::bare(
        IndexId::new(id),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        root.to_path_buf(),
    ))
}

/// A drain landing while a wave is in flight ends the pass, and once the pass
/// has let go the corpus opens again with its chunks — no cold start.
///
/// Pre-fix the drain only released a PARKED pass: the in-flight wave kept
/// waiting, the task never finished, and the `timeout` below failed.
/// Test: this IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_drain_abandons_an_in_flight_embed_wave_and_releases_the_corpus() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let redb_path = tmp.path().join("index.redb");
    let calls = Arc::new(AtomicUsize::new(0));
    let handle = handle_over_redb("drain-8600", tmp.path(), &redb_path, Arc::clone(&calls)).await;

    let pass = tokio::spawn(run_embed_catch_up(
        Arc::clone(&handle),
        Arc::new(ReindexProgress::new()),
    ));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while calls.load(Ordering::SeqCst) == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the pass never embedded"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    handle.embedding_pause.drain();
    tokio::time::timeout(Duration::from_secs(5), pass)
        .await
        .expect("a drained pass must end instead of waiting on its in-flight wave")
        .expect("the pass task must not panic");
    assert_ne!(
        handle.stages.read().await.semantic.status,
        StageStatus::Ready,
        "an abandoned pass must never be reported complete"
    );
    drop(handle);

    let reopened = CorpusStore::open(&redb_path)
        .expect("with the pass gone the corpus must open, not report DatabaseAlreadyOpen");
    assert_eq!(
        reopened.chunk_count().expect("chunk count"),
        3,
        "the reopened corpus keeps its chunks — no cold start"
    );
}

/// A pass whose embedder never answers aborts within the no-progress deadline
/// and settles `Failed`, instead of holding the background permit forever.
///
/// Pre-fix the wave awaited the embedder with no bound and the `timeout` below
/// failed.
/// Test: this IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_never_completing_embedder_aborts_the_pass_within_the_deadline() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let redb_path = tmp.path().join("index.redb");
    let calls = Arc::new(AtomicUsize::new(0));
    let handle = handle_over_redb("stall-8600", tmp.path(), &redb_path, calls).await;

    let pass = crate::core::indexer::WAVE_DEADLINE_OVERRIDE.scope(
        Duration::from_millis(300),
        run_embed_catch_up(Arc::clone(&handle), Arc::new(ReindexProgress::new())),
    );
    tokio::time::timeout(Duration::from_secs(10), pass)
        .await
        .expect("a no-progress pass must abort within its deadline");

    let stages = handle.stages.read().await;
    assert_eq!(stages.semantic.status, StageStatus::Failed);
    let reason = format!("{:?}", stages.semantic.failure);
    assert!(
        reason.contains("no progress"),
        "failure names the cause: {reason}"
    );
}
