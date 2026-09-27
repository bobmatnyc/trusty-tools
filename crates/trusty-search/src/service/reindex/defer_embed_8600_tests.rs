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

/// An embedder that answers its first `answer` batch calls and then either
/// never answers (`fail: false`, a sidecar that wedges part-way through a
/// pass) or returns an error (`fail: true`, a per-call timeout).
struct WedgesAfterCalls {
    answer: usize,
    fail: bool,
    calls: AtomicUsize,
    answered_texts: Arc<AtomicUsize>,
}

/// The error a failing [`WedgesAfterCalls`] returns.
const CALL_TIMEOUT_TEXT: &str = "embedder sidecar call timed out after 120s";

#[async_trait::async_trait]
impl Embedder for WedgesAfterCalls {
    async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
        std::future::pending().await
    }
    async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        if self.calls.fetch_add(1, Ordering::SeqCst) >= self.answer {
            if self.fail {
                anyhow::bail!(CALL_TIMEOUT_TEXT);
            }
            return std::future::pending().await;
        }
        self.answered_texts.fetch_add(texts.len(), Ordering::SeqCst);
        Ok(texts
            .iter()
            .enumerate()
            .map(|(i, _)| (0..8).map(|d| (i + d + 1) as f32).collect())
            .collect())
    }
    fn dimension(&self) -> usize {
        8
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
    handle_with(
        id,
        root,
        redb_path,
        Arc::new(NeverCompletingEmbedder { calls }),
        3,
    )
    .await
}

/// An index of `n` chunks in a real redb corpus at `redb_path`, embedding
/// through `embedder`.
async fn handle_with(
    id: &str,
    root: &std::path::Path,
    redb_path: &std::path::Path,
    embedder: Arc<dyn Embedder>,
    n: usize,
) -> Arc<IndexHandle> {
    let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(8).expect("usearch"));
    let mut indexer = CodeIndexer::new(id, root).with_components(embedder, store);
    indexer.set_corpus_store(Arc::new(CorpusStore::open(redb_path).expect("open corpus")));
    let parsed = ParsedBatch {
        chunks: (0..n).map(chunk).collect(),
        embeddings: vec![None; n],
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

/// A pass whose second wave stalls commits the first wave before it settles
/// `Failed`, and keeps the pending marker so the next boot embeds the rest.
///
/// Pre-fix the stall was a bare `Err` from the embed loop, so the completed
/// first wave was dropped and `pending_embed_count` still read the full corpus.
/// Test: this IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_stalled_wave_commits_the_waves_before_it() {
    let id = "stall-prefix-8600";
    let tmp = tempfile::tempdir().expect("tempdir");
    let redb_path = tmp.path().join("index.redb");
    let mut entry = crate::service::persistence::PersistedIndex::new(id, tmp.path());
    entry.deferred_embed_pending = true;
    crate::service::persistence::upsert_index_registry_entry(entry).expect("persist entry");

    // Wave 1 is `inflight` sub-batches, all answered; every later call hangs.
    // More chunks than any wave can hold, whatever the batch size resolves to.
    let inflight = crate::core::indexer::resolve_embed_inflight();
    let n = inflight * 512 + 1;
    let answered_texts = Arc::new(AtomicUsize::new(0));
    let embedder = Arc::new(WedgesAfterCalls {
        answer: inflight,
        fail: false,
        calls: AtomicUsize::new(0),
        answered_texts: Arc::clone(&answered_texts),
    });
    let handle = handle_with(id, tmp.path(), &redb_path, embedder, n).await;

    let pass = crate::core::indexer::WAVE_DEADLINE_OVERRIDE.scope(
        Duration::from_millis(500),
        run_embed_catch_up(Arc::clone(&handle), Arc::new(ReindexProgress::new())),
    );
    tokio::time::timeout(Duration::from_secs(20), pass)
        .await
        .expect("a stalled pass must abort within its deadline");

    let wave_one = answered_texts.load(Ordering::SeqCst);
    assert!(
        wave_one > 0 && wave_one < n,
        "wave 1 was answered, wave 2 was not"
    );
    {
        let stages = handle.stages.read().await;
        assert_eq!(stages.semantic.status, StageStatus::Failed);
        let reason = format!("{:?}", stages.semantic.failure);
        assert!(
            reason.contains("no progress"),
            "failure names the cause: {reason}"
        );
    }
    let owed = handle.indexer.read().await.pending_embed_count().await;
    assert_eq!(
        owed,
        n - wave_one,
        "wave 1's {wave_one} vectors must be committed before the pass settles Failed"
    );
    let path = crate::service::persistence::indexes_toml_path().expect("indexes.toml path");
    let kept = crate::service::persistence::load_index_registry_at(&path)
        .expect("registry must load")
        .into_iter()
        .find(|e| e.id == id)
        .expect("entry must exist");
    assert!(
        kept.deferred_embed_pending,
        "the pending marker must survive so the next boot embeds the remainder"
    );
}

/// A pass whose second wave returns an error — a per-call timeout, not a hang
/// — commits the first wave, settles `Failed` with the error, and keeps the
/// pending marker.
///
/// Pre-fix a sub-batch `Err` returned early from the embed loop with `?`, so
/// the completed first wave was dropped and `pending_embed_count` still read
/// the full corpus.
/// Test: this IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_wave_commits_the_waves_before_it() {
    let id = "fail-prefix-8600";
    let tmp = tempfile::tempdir().expect("tempdir");
    let redb_path = tmp.path().join("index.redb");
    let mut entry = crate::service::persistence::PersistedIndex::new(id, tmp.path());
    entry.deferred_embed_pending = true;
    crate::service::persistence::upsert_index_registry_entry(entry).expect("persist entry");

    // Wave 1 is `inflight` sub-batches, all answered; every later call errors.
    let inflight = crate::core::indexer::resolve_embed_inflight();
    let n = inflight * 512 + 1;
    let answered_texts = Arc::new(AtomicUsize::new(0));
    let embedder = Arc::new(WedgesAfterCalls {
        answer: inflight,
        fail: true,
        calls: AtomicUsize::new(0),
        answered_texts: Arc::clone(&answered_texts),
    });
    let handle = handle_with(id, tmp.path(), &redb_path, embedder, n).await;

    tokio::time::timeout(
        Duration::from_secs(20),
        run_embed_catch_up(Arc::clone(&handle), Arc::new(ReindexProgress::new())),
    )
    .await
    .expect("a failed pass must settle, not hang");

    let wave_one = answered_texts.load(Ordering::SeqCst);
    assert!(
        wave_one > 0 && wave_one < n,
        "wave 1 was answered, wave 2 was not"
    );
    {
        let stages = handle.stages.read().await;
        assert_eq!(stages.semantic.status, StageStatus::Failed);
        let reason = format!("{:?}", stages.semantic.failure);
        assert!(
            reason.contains(CALL_TIMEOUT_TEXT),
            "failure names the cause: {reason}"
        );
    }
    let owed = handle.indexer.read().await.pending_embed_count().await;
    assert_eq!(
        owed,
        n - wave_one,
        "wave 1's {wave_one} vectors must be committed before the pass settles Failed"
    );
    let path = crate::service::persistence::indexes_toml_path().expect("indexes.toml path");
    let kept = crate::service::persistence::load_index_registry_at(&path)
        .expect("registry must load")
        .into_iter()
        .find(|e| e.id == id)
        .expect("entry must exist");
    assert!(
        kept.deferred_embed_pending,
        "the pending marker must survive so the next boot embeds the remainder"
    );
}

/// Status answers while an embed pass is mid-wave and a writer is queued on
/// the indexer lock.
///
/// Pre-fix the pass held the indexer read guard across embedding. tokio's
/// `RwLock` is fair, so the queued writer blocked the status read behind it
/// for the rest of the pass, and the 2 s `timeout` below failed.
/// Test: this IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn status_answers_during_an_embed_pass_with_a_writer_queued() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let redb_path = tmp.path().join("index.redb");
    let calls = Arc::new(AtomicUsize::new(0));
    let handle = handle_over_redb("status-8600", tmp.path(), &redb_path, Arc::clone(&calls)).await;

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

    // A writer — e.g. a PATCH component toggle — queues on the indexer lock.
    let writer_handle = Arc::clone(&handle);
    let writer = tokio::spawn(async move {
        let _guard = writer_handle.indexer.write().await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    let registry = crate::core::registry::IndexRegistry::new();
    registry.register(IndexHandle::bare(
        handle.id.clone(),
        Arc::clone(&handle.indexer),
        handle.root_path.clone(),
    ));
    let state = Arc::new(crate::service::server::SearchAppState::new(registry));
    let body = tokio::time::timeout(
        Duration::from_secs(2),
        crate::service::server::index_status_report(&state, "status-8600"),
    )
    .await
    .expect("status must answer within 2 s while an embed pass runs and a writer is queued")
    .expect("status 200");
    assert_eq!(body["index_id"], "status-8600");
    tokio::time::timeout(Duration::from_secs(2), writer)
        .await
        .expect("the writer must not wait out the embed pass")
        .expect("the writer task must not panic");

    handle.embedding_pause.drain();
    tokio::time::timeout(Duration::from_secs(5), pass)
        .await
        .expect("a drained pass must end")
        .expect("the pass task must not panic");
}
