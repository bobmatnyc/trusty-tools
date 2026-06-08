//! Deferred-embedding background pass for issue #923 (DEFER-EMBED).
//!
//! Why: the fast pass (C1) stores all chunks in BM25 + redb without embedding
//! them so the index is searchable lexically within seconds. This module
//! provides the C2 catch-up job that embeds all corpus chunks and upserts the
//! resulting vectors into HNSW, then marks the semantic stage `Ready` (or
//! `Failed` on error, so /indexes/:id/status exposes the failure — issue #928).
//!
//! What: a single public entry point `spawn_deferred_embed_pass` that:
//! 1. Acquires the background reindex semaphore (serialises against concurrent
//!    reindexes on the same handle).
//! 2. Calls `CodeIndexer::embed_deferred_chunks` under the indexer's READ lock
//!    (no write lock held during embedding — the long operation).
//! 3. On success: forces an HNSW snapshot and marks semantic `Ready`.
//! 4. On failure: marks semantic `Failed` with the error reason (issue #928).
//!
//! Test: `deferred_embed_pass_marks_semantic_ready_and_is_idempotent` and
//! `failing_deferred_embed_pass_marks_semantic_failed` in
//! `service::reindex::defer_embed::tests`.

use crate::core::registry::{IndexHandle, StageState, StageStatus};
use crate::service::reindex::{background_reindex_semaphore, now_rfc3339, ReindexProgress};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// Spawn the C2 deferred-embed background pass (issue #923).
///
/// Why: the fast pass (C1) stored all chunks in BM25 + redb without embedding
/// them so the index was searchable lexically within seconds. This function
/// spawns the catch-up job that embeds all corpus chunks and upserts the
/// resulting vectors into HNSW, then marks the semantic stage `Ready`.
///
/// What: acquires the background reindex semaphore (one permit) so the embed
/// pass never races with a concurrent reindex, calls
/// `CodeIndexer::embed_deferred_chunks` under the indexer's READ lock (the
/// embed step holds no write lock), forces an HNSW snapshot, then marks
/// semantic `Ready` (or `Failed` when embedding errors, issue #928). The job
/// is idempotent: re-running after a partial failure re-embeds all chunks
/// (HNSW upsert is idempotent).
///
/// Test: `deferred_embed_pass_marks_semantic_ready_and_is_idempotent` and
/// `failing_deferred_embed_pass_marks_semantic_failed` in this module's tests.
pub(super) fn spawn_deferred_embed_pass(handle: Arc<IndexHandle>, progress: Arc<ReindexProgress>) {
    let index_id = handle.id.clone();
    tokio::spawn(async move {
        // Re-use the background semaphore to avoid racing with a concurrent
        // reindex or another deferred-embed pass on the same handle.
        let _permit = match background_reindex_semaphore().acquire().await {
            Ok(p) => p,
            Err(_) => {
                tracing::warn!(
                    "deferred_embed[{}]: background semaphore closed — skipping embed pass",
                    index_id.0,
                );
                return;
            }
        };

        let total_chunks = {
            let indexer = handle.indexer.read().await;
            indexer.chunk_count()
        };

        tracing::info!(
            "deferred_embed[{}]: starting background embed pass ({} chunks)",
            index_id.0,
            total_chunks,
        );

        // Issue #929: populate total + embedded=0 before embedding starts so
        // `GET /indexes/:id/status` shows real N/total progress rather than 0/0.
        {
            let mut stages = handle.stages.write().await;
            stages.semantic.started_at = Some(now_rfc3339());
            stages.semantic.total = Some(total_chunks);
            stages.semantic.embedded = Some(0);
        }

        // Emit an SSE event so observers (UI, CLI `--watch`) know embedding
        // has started. This fires on the progress handle after the fast-pass
        // `complete` event, so late SSE subscribers may see it.
        progress
            .push(serde_json::json!({
                "event": "embed_start",
                "index_id": index_id.0,
                "total_chunks": total_chunks,
            }))
            .await;

        // Issue #929: wire a per-wave progress channel so the stage counter
        // advances in real time while embedding is in progress.
        let (progress_tx, mut progress_rx) = tokio::sync::mpsc::unbounded_channel::<(usize, u64)>();
        let embedded_counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = Arc::clone(&embedded_counter);
        let stages_clone = Arc::clone(&handle.stages);
        // Spawn a task that drains wave notifications and updates stages.semantic.embedded.
        let progress_updater = tokio::spawn(async move {
            while let Some((wave_chunks, _ms)) = progress_rx.recv().await {
                let n = counter_clone.fetch_add(wave_chunks, Ordering::AcqRel) + wave_chunks;
                let mut stages = stages_clone.write().await;
                stages.semantic.embedded = Some(n);
            }
        });

        let result = {
            let indexer = handle.indexer.read().await;
            indexer.embed_deferred_chunks(Some(&progress_tx)).await
        };
        // Drop the sender so the updater task's recv loop terminates.
        drop(progress_tx);
        // Wait for the updater to finish processing any buffered notifications.
        let _ = progress_updater.await;

        match result {
            Ok((embedded, total)) => {
                // Force an HNSW snapshot so the vectors survive a daemon
                // restart even if no subsequent reindex runs.
                {
                    let indexer = handle.indexer.read().await;
                    indexer.force_incremental_persist();
                }
                tracing::info!(
                    "deferred_embed[{}]: embedded {}/{} chunks — marking semantic Ready",
                    index_id.0,
                    embedded,
                    total,
                );
                // Mark the semantic stage Ready — the full HNSW lane is now
                // queryable. We write the stage directly (not via
                // `mark_semantic_ready_graph_in_progress`) because the graph
                // stage is already Ready from the fast-pass KG rebuild; we
                // must not flip it back to InProgress.
                {
                    let mut stages = handle.stages.write().await;
                    stages.semantic.status = StageStatus::Ready;
                    stages.semantic.completed_at = Some(now_rfc3339());
                    stages.semantic.embedded = Some(embedded);
                    stages.semantic.total = Some(total);
                }
                progress
                    .push(serde_json::json!({
                        "event": "embed_complete",
                        "index_id": index_id.0,
                        "embedded": embedded,
                        "total": total,
                    }))
                    .await;
            }
            Err(e) => {
                let reason = format!("{e:#}");
                tracing::error!(
                    "deferred_embed[{}]: embed pass failed — {reason}",
                    index_id.0,
                );
                // Issue #928: mark semantic stage as Failed so the /status
                // endpoint exposes the failure. Without this, the stage stays
                // in whatever pre-Ready state it was in (Pending or InProgress)
                // and operators polling /indexes/:id/status cannot tell that
                // embedding failed — it silently looks like "still embedding".
                {
                    let mut stages = handle.stages.write().await;
                    stages.semantic = StageState::failed(reason.clone());
                }
                progress
                    .push(serde_json::json!({
                        "event": "embed_error",
                        "index_id": index_id.0,
                        "message": reason,
                    }))
                    .await;
                // Issue #929: semantic.total was pre-seeded before embedding;
                // the Failed state replaces it (StageState::failed clears it).
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{indexer::CodeIndexer, registry::IndexId};
    use crate::service::reindex::ReindexProgress;
    use std::sync::Arc;

    /// Issue #928: `spawn_deferred_embed_pass` with a BM25-only (no embedder)
    /// handle must mark semantic Ready after the pass completes. Without an
    /// embedder `embed_deferred_chunks` returns `Ok((0, 0))` — that is the
    /// expected no-op fast path.
    ///
    /// Why: confirms the success path of `spawn_deferred_embed_pass` marks the
    /// semantic stage Ready so that /indexes/:id/status reflects completion.
    /// What: constructs a bare handle with `defer_embed=true`, calls
    /// `spawn_deferred_embed_pass`, and polls until semantic.status == Ready.
    /// Test: this test.
    #[tokio::test]
    async fn deferred_embed_pass_marks_semantic_ready_and_is_idempotent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        let indexer = CodeIndexer::new("defer-ready-test", root.clone());
        let handle = Arc::new(crate::core::registry::IndexHandle::bare(
            IndexId::new("defer-ready-test"),
            Arc::new(tokio::sync::RwLock::new(indexer)),
            root,
        ));
        let progress = Arc::new(ReindexProgress::new());
        spawn_deferred_embed_pass(handle.clone(), progress.clone());

        // Poll until semantic stage transitions out of Pending.
        for _ in 0..100 {
            let stages = handle.stages.read().await;
            if stages.semantic.status != crate::core::registry::StageStatus::Pending {
                break;
            }
            drop(stages);
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        let stages = handle.stages.read().await;
        assert_eq!(
            stages.semantic.status,
            crate::core::registry::StageStatus::Ready,
            "deferred embed pass (no-embedder) must flip semantic to Ready"
        );
    }

    /// Issue #928: when the background embed pass fails, `spawn_deferred_embed_pass`
    /// must mark the semantic stage as `Failed` (not leave it in a stuck pre-Ready
    /// state). Operators polling /indexes/:id/status must see the failure.
    ///
    /// Why: before this fix, the Err branch only logged + pushed SSE — the stage
    /// was never updated, leaving it in Pending/InProgress indefinitely. This test
    /// locks in the fix by asserting the semantic stage is Failed on error.
    /// What: constructs a CodeIndexer with a FailingEmbedder and a live HNSW
    /// store, commits a chunk so `embed_deferred_chunks` has work to do, then
    /// calls `spawn_deferred_embed_pass` and asserts semantic.status == Failed.
    /// Test: this test.
    #[tokio::test]
    async fn failing_deferred_embed_pass_marks_semantic_failed() {
        use crate::core::{
            chunker::{ChunkType, RawChunk},
            embed::Embedder,
            indexer::ParsedBatch,
            store::{UsearchStore, VectorStore},
        };
        use anyhow::bail;
        use std::sync::Arc as StdArc;

        /// A test-only embedder that always returns an error.
        struct FailingEmbedder;

        #[async_trait::async_trait]
        impl Embedder for FailingEmbedder {
            async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
                bail!("injected embed failure for test")
            }

            async fn embed_batch(&self, _texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
                bail!("injected embed failure for test")
            }

            fn dimension(&self) -> usize {
                8
            }
        }

        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        let store: StdArc<dyn VectorStore> =
            StdArc::new(UsearchStore::new(8).expect("usearch new"));
        let indexer = CodeIndexer::new("defer-fail-test", root.clone())
            .with_components(StdArc::new(FailingEmbedder), store);
        // Commit a synthetic chunk so embed_deferred_chunks has work to do.
        let parsed = ParsedBatch {
            chunks: vec![RawChunk {
                id: "test:1:1".into(),
                file: "test.rs".into(),
                start_line: 1,
                end_line: 1,
                content: "fn test_fn() {}".into(),
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
            }],
            embeddings: vec![None],
            entities_by_file: vec![],
            parse_ms: 0,
            embed_ms: 0,
            vector_count: 0,
        };
        indexer.commit_parsed_batch(parsed, false).await.ok();

        let handle = Arc::new(crate::core::registry::IndexHandle::bare(
            IndexId::new("defer-fail-test"),
            Arc::new(tokio::sync::RwLock::new(indexer)),
            root,
        ));
        let progress = Arc::new(ReindexProgress::new());
        spawn_deferred_embed_pass(handle.clone(), progress.clone());

        // Poll until semantic stage transitions out of Pending.
        for _ in 0..100 {
            let stages = handle.stages.read().await;
            if stages.semantic.status != crate::core::registry::StageStatus::Pending {
                break;
            }
            drop(stages);
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        let stages = handle.stages.read().await;
        assert_eq!(
            stages.semantic.status,
            crate::core::registry::StageStatus::Failed,
            "failing deferred embed pass must flip semantic to Failed (issue #928)"
        );
        assert!(
            stages.semantic.failure.is_some(),
            "Failed stage must carry the failure reason"
        );
    }

    /// Issue #929: `spawn_deferred_embed_pass` must populate `stages.semantic.total`
    /// (and `embedded = 0`) BEFORE calling `embed_deferred_chunks` so that
    /// `GET /indexes/:id/status` returns a non-trivial `N / total` even if polling
    /// starts immediately after the fast pass completes.
    ///
    /// Why: without pre-seeding `total`, the `print_stage_row` and non-TTY watch
    /// loop both receive `total = 0`, rendering `0 / 0 (0%)` for the entire
    /// background embed pass. Pre-seeding lets operators see real progress.
    /// What: commits one chunk (so total_chunks = 1), calls `spawn_deferred_embed_pass`
    /// on a BM25-only handle, and asserts that `semantic.total == Some(1)` is
    /// visible BEFORE the pass completes (by racing a read against the spawn).
    /// Test: this test.
    #[tokio::test]
    async fn deferred_embed_pass_pre_seeds_total_before_embedding() {
        use crate::core::{
            chunker::{ChunkType, RawChunk},
            indexer::ParsedBatch,
        };

        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        let indexer = CodeIndexer::new("defer-total-test", root.clone());
        // Commit one synthetic chunk so total_chunks = 1.
        let parsed = ParsedBatch {
            chunks: vec![RawChunk {
                id: "test:1:1".into(),
                file: "test.rs".into(),
                start_line: 1,
                end_line: 1,
                content: "fn total_test() {}".into(),
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
            }],
            embeddings: vec![None],
            entities_by_file: vec![],
            parse_ms: 0,
            embed_ms: 0,
            vector_count: 0,
        };
        indexer.commit_parsed_batch(parsed, false).await.ok();

        let handle = Arc::new(crate::core::registry::IndexHandle::bare(
            IndexId::new("defer-total-test"),
            Arc::new(tokio::sync::RwLock::new(indexer)),
            root,
        ));
        let progress = Arc::new(ReindexProgress::new());
        spawn_deferred_embed_pass(handle.clone(), progress.clone());

        // Poll until total is populated (pre-seeded before embed starts).
        let mut total_seen: Option<usize> = None;
        for _ in 0..200 {
            let stages = handle.stages.read().await;
            if stages.semantic.total.is_some() {
                total_seen = stages.semantic.total;
                break;
            }
            drop(stages);
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(
            total_seen,
            Some(1),
            "stages.semantic.total must be pre-seeded to 1 (the chunk count) \
             before embed_deferred_chunks runs — so /indexes/:id/status shows \
             real N/total progress even during embedding"
        );
    }
}
