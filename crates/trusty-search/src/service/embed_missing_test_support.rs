//! Shared fixtures for the #10551 embed-missing and embed-retry tests.
//!
//! Why: both test files need an index whose embedder died part-way through the
//! semantic stage, so the fixture lives once.
//! What: [`FlakyEmbedder`] answers the first `fail_from` calls and then fails
//! like a dead child (`embedder stdio IPC error: child closed stdout`) until
//! [`FlakyEmbedder::heal`]. [`failed_mid_stage`] runs the real deferred-embed
//! pass over it, so the stage ends `Failed` with a committed prefix and a tail
//! of chunks that have no vector.
//! Test: used by `embed_missing_tests` and `embed_retry_tests`.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::core::chunker::{ChunkType, RawChunk};
use crate::core::embed::Embedder;
use crate::core::indexer::{CodeIndexer, ParsedBatch};
use crate::core::registry::{IndexHandle, IndexId, StageStatus};
use crate::core::store::{UsearchStore, VectorStore};
use crate::service::reindex::{deferred_embed_queue_depth, run_embed_catch_up, ReindexProgress};

pub(crate) const DIM: usize = 8;
/// Chunks beyond the first wave's nominal size, so a second wave exists.
const EXTRA: usize = 7;
/// The text the probe embeds, so a test can tell it from chunk text.
pub(crate) const PROBE_TEXT: &str = "trusty-search embedder probe";

/// An embedder that dies after `fail_from` calls and can be healed.
pub(crate) struct FlakyEmbedder {
    fail_from: usize,
    healed: AtomicBool,
    pub(crate) calls: AtomicUsize,
    /// Every text answered after the heal, probes included.
    pub(crate) answered_after_heal: Mutex<Vec<String>>,
}

impl FlakyEmbedder {
    pub(crate) fn heal(&self) {
        self.healed.store(true, Ordering::SeqCst);
    }

    /// Chunk texts answered after the heal, without the probe's.
    pub(crate) fn healed_chunk_texts(&self) -> Vec<String> {
        let all = self.answered_after_heal.lock().expect("answered lock");
        all.iter().filter(|t| *t != PROBE_TEXT).cloned().collect()
    }
}

#[async_trait::async_trait]
impl Embedder for FlakyEmbedder {
    async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
        std::future::pending().await
    }
    async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if self.healed.load(Ordering::SeqCst) {
            let mut log = self.answered_after_heal.lock().expect("answered lock");
            log.extend(texts.iter().map(|t| t.to_string()));
        } else if call >= self.fail_from {
            anyhow::bail!("embedder stdio IPC error: child closed stdout");
        }
        Ok(texts
            .iter()
            .enumerate()
            .map(|(i, _)| (0..DIM).map(|d| (i + d + 1) as f32).collect())
            .collect())
    }
    fn dimension(&self) -> usize {
        DIM
    }
}

fn chunk(n: usize) -> RawChunk {
    RawChunk {
        id: format!("m{n}.rs:1:1"),
        file: format!("m{n}.rs"),
        start_line: 1,
        end_line: 1,
        content: format!("fn missing_function_{n}() {{}}"),
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

/// A resident index of `n` unembedded chunks over `embedder` and a fresh store.
async fn unembedded_handle(
    id: &str,
    n: usize,
    embedder: Arc<dyn Embedder>,
) -> (Arc<IndexHandle>, Arc<dyn VectorStore>) {
    let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(DIM).expect("usearch"));
    let indexer = CodeIndexer::new(id, "/tmp/embed-missing-10551")
        .with_components(embedder, Arc::clone(&store));
    indexer
        .commit_parsed_batch(
            ParsedBatch {
                chunks: (0..n).map(chunk).collect(),
                embeddings: vec![None; n],
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
        std::path::PathBuf::from("/tmp/embed-missing-10551"),
    ));
    (handle, store)
}

/// An index whose embedder died in wave 2 of the semantic stage.
///
/// Returns the handle, the embedder, the store, the chunk count and the number
/// of chunks the committed waves cover, as measured: the wave size follows the
/// host's batch tuning, so it is read back, never assumed. The stage is
/// `Failed` with `0 < committed < n`.
pub(crate) async fn failed_mid_stage(
    id: &str,
) -> (
    Arc<IndexHandle>,
    Arc<FlakyEmbedder>,
    Arc<dyn VectorStore>,
    usize,
    usize,
) {
    let inflight = crate::core::indexer::resolve_embed_inflight();
    let n = inflight * 512 + EXTRA;
    let embedder = Arc::new(FlakyEmbedder {
        fail_from: inflight,
        healed: AtomicBool::new(false),
        calls: AtomicUsize::new(0),
        answered_after_heal: Mutex::new(Vec::new()),
    });
    let (handle, store) = unembedded_handle(id, n, embedder.clone()).await;
    run_embed_catch_up(Arc::clone(&handle), Arc::new(ReindexProgress::new())).await;
    let semantic = handle.stages.read().await.semantic.clone();
    assert_eq!(semantic.status, StageStatus::Failed, "{semantic:?}");
    let committed = store.len().await.expect("len");
    assert!(committed > 0 && committed < n, "{committed} of {n}");
    (handle, embedder, store, n, committed)
}

/// Wait until the stage reads `want` and the embed queue is empty.
pub(crate) async fn wait_for_stage(handle: &Arc<IndexHandle>, want: StageStatus) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while handle.stages.read().await.semantic.status != want || deferred_embed_queue_depth() > 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "stage never reached {want:?}: {:?}",
            handle.stages.read().await.semantic
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
