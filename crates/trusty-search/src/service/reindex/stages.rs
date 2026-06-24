//! Stage-pipeline state transitions for the three-lane reindex flow.
//!
//! Why: the reindex orchestrator flips `IndexStages` through a well-defined
//! sequence (Pending → InProgress → Ready/Skipped/Failed) for the three search
//! lanes: lexical (BM25), semantic (HNSW), and graph (KG). Extracting the
//! transitions here keeps the orchestrator body free of write-lock boilerplate
//! and makes each transition trivially reviewable.
//!
//! What:
//! - `now_rfc3339` — RFC-3339 timestamp helper.
//! - `reset_stages_for_reindex` — wipe stages at reindex start.
//! - `mark_lexical_ready_semantic_in_progress` — after the BM25 phase drains.
//! - `mark_semantic_ready_graph_in_progress` — after embed phase completes.
//! - `mark_reindex_failed` — on zero-vector embed failure (#601).
//! - `mark_graph_ready` — after KG rebuild.
//! - `schedule_progress_cleanup` — GC the progress entry after TTL.
//! - `refresh_context_embedding` — refresh per-index routing embedding (#112).
//!
//! Test: covered indirectly by the reindex integration tests; direct unit
//! coverage in the validate module tests.

use crate::core::registry::{IndexHandle, IndexId, IndexStages, StageState, StageStatus};
use dashmap::DashMap;
use std::sync::Arc;

use super::progress::ReindexProgress;

/// How long to keep a completed (`Complete` / `Failed`) `ReindexProgress`
/// on `SearchAppState::reindex_progress` before garbage-collecting it.
/// 60 s is enough for late SSE subscribers to attach and read the final
/// state but short enough that long-running daemons don't accumulate
/// thousands of stale progress entries.
pub(super) const REINDEX_PROGRESS_TTL_SECS: u64 = 60;

/// RFC-3339 timestamp helper used by the staged-pipeline status surface
/// (issue #109, Phase 1).
///
/// Why: each `StageState` carries optional `started_at` / `completed_at`
/// timestamps so external dashboards can compute stage durations without
/// inferring them from event ordering. Centralising the formatter keeps
/// the timestamp shape consistent across every transition.
pub(crate) fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Reset every stage to `Pending` (or `Skipped` for lexical-only) and stamp
/// `lexical.started_at` at the start of a reindex. The previous run's
/// counters are wiped so the search-capabilities array doesn't briefly
/// report stale lanes mid-reindex.
///
/// Why: a fresh reindex must announce itself by flipping capabilities back to
/// Pending so concurrent searches see that the index is rebuilding, not stale.
/// What: write-locks `handle.stages` and resets to the appropriate initial state.
/// Test: covered by reindex integration tests observing stage transitions.
pub(crate) async fn reset_stages_for_reindex(handle: &Arc<IndexHandle>) {
    let mut stages = handle.stages.write().await;
    if handle.lexical_only {
        *stages = IndexStages {
            lexical: StageState {
                status: StageStatus::InProgress,
                started_at: Some(now_rfc3339()),
                ..Default::default()
            },
            semantic: StageState::skipped(),
            graph: StageState::skipped(),
        };
    } else {
        // Issue #313: skip_kg forces graph to Skipped from the start of the
        // reindex. Semantic is unaffected (skip_kg is orthogonal to embedding).
        let graph_init = if handle.skip_kg {
            StageState::skipped()
        } else {
            StageState::pending()
        };
        *stages = IndexStages {
            lexical: StageState {
                status: StageStatus::InProgress,
                started_at: Some(now_rfc3339()),
                ..Default::default()
            },
            semantic: StageState::pending(),
            graph: graph_init,
        };
    }
}

/// Flip the lexical stage to `Ready` and stash file / chunk counters. Stage
/// 2 (semantic) is flipped to `InProgress` simultaneously since the
/// pipelined producer has already been consuming embedder budget per
/// batch — exposing the in-progress state is what enables the search
/// handler's graceful-degradation guarantee (BM25 lane queryable while
/// HNSW is still warming up).
///
/// `corpus_total_chunks` (issue #879): the **total** chunk count in the
/// corpus after this reindex (obtained from the durable corpus store), NOT
/// the per-run counter from `ReindexProgress::total_chunks`. The per-run
/// counter is 0 on a no-change incremental pass (all files hash-skipped),
/// which caused `stages.lexical.chunks` to report 0 while the top-level
/// `chunk_count` field correctly reported the cumulative total. Using the
/// corpus total keeps the two fields consistent.
///
/// Why: BM25 is always fully built by the time the batch loop drains; the
/// lexical lane should be exposed as Ready immediately so text-exact queries
/// work while embedding finishes.
/// What: write-locks stages and updates `lexical` to Ready + `semantic` to
/// InProgress (for full-pipeline indexes).
/// Test: covered by reindex integration tests.
pub(crate) async fn mark_lexical_ready_semantic_in_progress(
    handle: &Arc<IndexHandle>,
    files: usize,
    corpus_total_chunks: usize,
    total_chunks_for_embed: usize,
) {
    let mut stages = handle.stages.write().await;
    stages.lexical.status = StageStatus::Ready;
    stages.lexical.completed_at = Some(now_rfc3339());
    stages.lexical.files = Some(files);
    // Issue #879: report the total corpus chunk count, not just the
    // per-reindex-pass count. On a no-change incremental reindex the
    // per-pass count is 0 (all files hash-skipped), but the corpus still
    // holds the full set of chunks from prior runs. Using the corpus total
    // keeps `stages.lexical.chunks` consistent with the top-level
    // `chunk_count` field in the status response.
    stages.lexical.chunks = Some(corpus_total_chunks);
    // On lexical-only indexes the semantic + graph slots stay `Skipped` —
    // the reset hook pre-populated them. Don't overwrite the terminal
    // state. For full-pipeline indexes the semantic stage has been running
    // alongside the producer (the embed step is part of every batch); flip
    // it to `InProgress` so callers see it as queryable-soon.
    if !handle.lexical_only && stages.semantic.status == StageStatus::Pending {
        stages.semantic.status = StageStatus::InProgress;
        stages.semantic.started_at = Some(now_rfc3339());
        stages.semantic.total = Some(total_chunks_for_embed);
    }
}

/// Flip the semantic stage to `Ready` and stamp `embedded` counter. Stage
/// 3 (graph) is set to `InProgress` since the post-batch KG rebuild always
/// follows immediately.
///
/// Why: after embed phase completes the HNSW lane is queryable; exposing
/// InProgress on the graph stage lets searches use KG expansion as soon as
/// it lands.
/// What: write-locks stages and updates `semantic` to Ready + `graph` to
/// InProgress (unless skip_kg or lexical_only).
/// Test: covered by reindex integration tests.
pub(crate) async fn mark_semantic_ready_graph_in_progress(
    handle: &Arc<IndexHandle>,
    embedded: usize,
    total: usize,
) {
    let mut stages = handle.stages.write().await;
    if handle.lexical_only {
        // No work for semantic / graph on a lexical-only index. Leave the
        // skipped state alone.
        return;
    }
    stages.semantic.status = StageStatus::Ready;
    stages.semantic.completed_at = Some(now_rfc3339());
    stages.semantic.embedded = Some(embedded);
    stages.semantic.total = Some(total);
    // Issue #313: skip_kg holds graph in Skipped — do not flip to InProgress.
    if !handle.skip_kg && stages.graph.status == StageStatus::Pending {
        stages.graph.status = StageStatus::InProgress;
        stages.graph.started_at = Some(now_rfc3339());
    }
}

/// Mark the index reindex-failed (issue #601).
///
/// Why: a full-pipeline index that walked files but embedded zero vectors is
/// broken — the embedder silently failed for every batch. Before this gate the
/// reindex flipped semantic + graph to `Ready` regardless, so `/health` served
/// a dead index as green. This transition flips the semantic stage to `Failed`
/// (carrying the reason) so `lifecycle_status` reports `"failed"` and the
/// failure is LOUD. The lexical stage is left `Ready` because the BM25 lane was
/// genuinely built and is still queryable.
/// What: write-locks the stages and sets `semantic = StageState::failed(reason)`
/// and `graph = StageState::failed(reason)` (the graph lane never built either).
/// A `lexical_only` index can never reach this path (it has no semantic stage),
/// so we never clobber a legitimate skipped state.
/// Test: `reindex_marks_failed_on_zero_vectors` (daemon-gated end-to-end) and
/// the pure `validate::reindex_outcome` unit tests drive the decision.
pub(super) async fn mark_reindex_failed(handle: &Arc<IndexHandle>, reason: &str) {
    let mut stages = handle.stages.write().await;
    // Lexical lane was genuinely built — keep it Ready/queryable.
    stages.semantic = StageState::failed(reason);
    stages.graph = StageState::failed(reason);
}

/// Flip the graph stage to `Ready`. After this transition the search
/// handler treats `kg` as a queryable lane and the legacy top-level
/// `status` field reports `"ready"`.
///
/// Why: KG rebuild is the last phase; marking graph Ready signals that all
/// three lanes are now available.
/// What: write-locks stages and sets `graph.status = Ready` (no-op for
/// lexical_only or skip_kg indexes whose graph stays Skipped).
/// Test: covered by reindex integration tests.
pub(crate) async fn mark_graph_ready(handle: &Arc<IndexHandle>) {
    let mut stages = handle.stages.write().await;
    // Both lexical_only and skip_kg keep the graph stage Skipped — nothing to do.
    if handle.lexical_only || handle.skip_kg {
        return;
    }
    stages.graph.status = StageStatus::Ready;
    stages.graph.completed_at = Some(now_rfc3339());
}

/// Schedule deferred GC of the `reindex_progress` map entry for this index.
///
/// Why: issue #75 — bounds long-running daemon memory by GC'ing stale progress
/// entries while still letting late SSE subscribers read the final
/// `complete` / `error` event for `REINDEX_PROGRESS_TTL_SECS`.
/// What: spawns a `tokio::time::sleep` task that removes the entry after the TTL.
/// Test: covered indirectly by the reindex integration tests.
pub(super) fn schedule_progress_cleanup(
    cleanup_map: Option<Arc<DashMap<IndexId, Arc<ReindexProgress>>>>,
    cleanup_id: IndexId,
) {
    let Some(map) = cleanup_map else {
        return;
    };
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(REINDEX_PROGRESS_TTL_SECS)).await;
        map.remove(&cleanup_id);
    });
}

/// Refresh `handle.context_embedding` and `handle.context_summary` from the
/// root-level metadata files (issue #112).
///
/// Why: cross-index fan-out routing in `POST /search` weights each index by
/// cosine similarity between the query embedding and the index's stored
/// context embedding. The embedding is regenerated at the end of every
/// reindex so it tracks changes to README / CLAUDE.md / manifest files.
/// What: scrapes metadata via `context_inference::scrape_metadata_summary`,
/// embeds the resulting string with the indexer's embedder, and writes the
/// result into the handle's `RwLock`-guarded slots. Failure (no metadata,
/// no embedder, embed error) leaves the slots as `None` so the router
/// treats this index with a neutral 1.0 weight.
/// Test: `context_embedding_populated_after_reindex` in this module.
pub(super) async fn refresh_context_embedding(handle: &Arc<IndexHandle>) {
    use crate::service::context_inference::{make_display_summary, scrape_metadata_summary};

    let Some(summary) = scrape_metadata_summary(&handle.root_path) else {
        tracing::debug!(
            "context_inference: no recognised metadata files under {} for index {}",
            handle.root_path.display(),
            handle.id.0
        );
        *handle.context_embedding.write().await = None;
        *handle.context_summary.write().await = None;
        return;
    };

    let display = make_display_summary(&summary);

    let indexer = handle.indexer.read().await;
    let embed_result = indexer.embed_text(&summary).await;
    drop(indexer);

    match embed_result {
        Ok(Some(vec)) => {
            *handle.context_embedding.write().await = Some(vec);
            *handle.context_summary.write().await = Some(display);
            tracing::info!(
                "context_inference: refreshed context embedding for index {}",
                handle.id.0
            );
        }
        Ok(None) => {
            tracing::debug!(
                "context_inference: no embedder wired on index {} — skipping context embedding",
                handle.id.0
            );
            *handle.context_embedding.write().await = None;
            *handle.context_summary.write().await = Some(display);
        }
        Err(e) => {
            tracing::warn!(
                "context_inference: embed failed for index {}: {e}",
                handle.id.0
            );
            *handle.context_embedding.write().await = None;
            *handle.context_summary.write().await = Some(display);
        }
    }
}
