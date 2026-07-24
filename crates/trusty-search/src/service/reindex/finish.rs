//! Post-batch-loop completion: stage swap, KG rebuild, and terminal event.
//!
//! Why: extracted from `runner.rs` (issue #1175 SLOC cap) to keep each file
//! under the 500-SLOC production limit. Contains everything that happens after
//! the pipelined parse/embed/commit loop finishes.
//!
//! What: exports `FinishCtx` (bundles accumulated per-run state) and
//! `finish_reindex`, a `pub(super)` async function that drives:
//!   - prune pass (stale-chunk deletion)
//!   - staged corpus swap commit or rollback
//!   - hard embed-failure error path
//!   - KG rebuild (Phase 3)
//!   - poller teardown + final RSS sample
//!   - status update and `complete` SSE event emission
//!   - quarantine, context-embedding refresh, deferred-embed spawn, GC
//!
//! Test: covered by `reindex_walks_directory_and_emits_events` and the
//! integration tests in `tests.rs`.

use crate::core::memguard::{current_rss_mb, current_rss_mb_for_pid};
use crate::core::registry::{IndexHandle, IndexId};
use dashmap::DashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering as AtomicOrdering};
use std::sync::Arc;
use std::time::Instant;

use super::completion::{
    emit_complete_event, rebuild_symbol_graph_for_reindex, KgRebuildOutcome, RunTotals,
};
use super::corpus_swap::{abort_staged_corpus_swap, commit_staged_corpus_swap};
use super::defer_embed::spawn_deferred_embed_pass;
use super::guard::ReindexTerminationGuard;
use super::progress::{ReindexProgress, ReindexStatus};
use super::quarantine::ReindexQuarantine;
use super::stages::{
    mark_graph_ready, mark_lexical_ready_semantic_in_progress, mark_reindex_failed,
    mark_semantic_ready_graph_in_progress, now_rfc3339, refresh_context_embedding,
    schedule_progress_cleanup,
};
use super::staging;
use super::validate;

/// The hash-cache type used by the reindex pipeline.
///
/// Why: avoids repeating the verbose `Arc<DashMap<PathBuf, String>>` across
/// module boundaries.
/// What: type alias matching the return type of `hash::hashes_for`.
/// Test: used via `FinishCtx::hashes`.
pub(super) type FileHashes = Arc<DashMap<PathBuf, String>>;

/// Accumulated per-run counters passed from the batch loop to `finish_reindex`.
///
/// Why: avoids a long positional parameter list.
/// What: plain data struct — no logic, only carries values out of the batch loop.
/// Test: constructed inline in `runner::run_reindex`; checked indirectly by
/// the full-run integration tests.
pub(super) struct BatchTotals {
    pub(super) walk_ms: u64,
    pub(super) parse_ms: u64,
    pub(super) embed_ms: u64,
    pub(super) bm25_ms: u64,
    pub(super) vector_upsert_ms: u64,
    pub(super) vector_count: usize,
    pub(super) chunks_dropped_by_cap: usize,
    pub(super) mem_limit_hit: bool,
}

/// Context bundle for `finish_reindex`.
///
/// Why: the post-loop path needs ~15 distinct values; passing them as a single
/// struct is cleaner and avoids any future "which argument is which" confusion.
/// What: holds all outputs from the batch loop plus fixed-lifetime handles that
/// were initialised before the loop started.
/// Test: constructed inline in `runner::run_reindex`.
#[allow(clippy::too_many_arguments)]
pub(super) struct FinishCtx {
    pub(super) handle: Arc<IndexHandle>,
    pub(super) progress: Arc<ReindexProgress>,
    pub(super) index_id: IndexId,
    pub(super) canonical_root: PathBuf,
    pub(super) walked_files: Vec<PathBuf>,
    pub(super) hashes: FileHashes,
    pub(super) total: usize,
    pub(super) started: Instant,
    pub(super) defer_embed: bool,
    pub(super) corpus_swap_tmp: Option<PathBuf>,
    pub(super) mem_abort: Arc<AtomicBool>,
    pub(super) peak_rss_atomic: Arc<AtomicU64>,
    pub(super) peak_embedderd_rss_atomic: Arc<AtomicU64>,
    pub(super) embedderd_pid_slot: Option<Arc<AtomicU32>>,
    pub(super) poller_handle: tokio::task::JoinHandle<()>,
    pub(super) poller_stop: Arc<AtomicBool>,
    pub(super) embedderd_poller_handle: Option<tokio::task::JoinHandle<()>>,
    pub(super) embedderd_poller_stop: Option<Arc<AtomicBool>>,
    pub(super) term_guard: ReindexTerminationGuard,
    pub(super) cleanup_map: Option<Arc<DashMap<IndexId, Arc<ReindexProgress>>>>,
    pub(super) cleanup_id: IndexId,
    pub(super) aborted_map: Option<Arc<DashMap<IndexId, Instant>>>,
    pub(super) quarantine: Option<ReindexQuarantine>,
    pub(super) mem_limit: Option<u64>,
    pub(super) force: bool,
}

/// Post-batch-loop completion: prune, KG rebuild, poller teardown, and events.
///
/// Why: extracted from `runner.rs` to bring it under the 500-SLOC cap.
/// What: runs all work that follows the pipelined batch loop and emits the
/// terminal SSE `complete` event. Returns after scheduling progress GC.
/// Test: `reindex_walks_directory_and_emits_events` (primary integration test).
pub(super) async fn finish_reindex(ctx: FinishCtx, totals: BatchTotals) {
    use std::sync::atomic::Ordering;

    let FinishCtx {
        handle,
        progress,
        index_id,
        canonical_root,
        walked_files,
        hashes,
        total,
        started,
        defer_embed,
        corpus_swap_tmp,
        mem_abort,
        peak_rss_atomic,
        peak_embedderd_rss_atomic,
        embedderd_pid_slot,
        poller_handle,
        poller_stop,
        embedderd_poller_handle,
        embedderd_poller_stop,
        mut term_guard,
        cleanup_map,
        cleanup_id,
        aborted_map,
        quarantine,
        mem_limit,
        force,
    } = ctx;

    let BatchTotals {
        walk_ms,
        parse_ms: total_parse_ms,
        embed_ms: total_embed_ms,
        bm25_ms: total_bm25_ms,
        vector_upsert_ms: total_vector_upsert_ms,
        vector_count: total_vector_count,
        chunks_dropped_by_cap: total_chunks_dropped_by_cap,
        mem_limit_hit,
    } = totals;

    let memory_aborted = mem_limit_hit || mem_abort.load(AtomicOrdering::Acquire);

    // Issue #848 — prune pass: remove stale chunks from files deleted on disk.
    if corpus_swap_tmp.is_some() && !force && !memory_aborted {
        super::prune::prune_deleted_files_from_staging(
            &handle,
            &walked_files,
            &canonical_root,
            &hashes,
            &index_id,
        )
        .await;
    }

    let embedder_present = handle.indexer.read().await.has_embedder();
    // Issue #2984 Phase 1: `skip_vector` never embeds, exactly like
    // `lexical_only` — zero vectors is the expected, healthy outcome for the
    // #601 zero-vector-embed-failure gate, not a crash. Combining the two
    // flags here (rather than threading a new parameter through
    // `validate::reindex_outcome`) keeps that pure function's signature
    // focused on "does this index ever embed" without conflating it with
    // `skip_vector`'s KG-independence, which `reindex_outcome` doesn't touch.
    let never_embeds = handle.lexical_only || handle.skip_vector;
    let reindex_outcome = validate::reindex_outcome(
        never_embeds,
        defer_embed,
        embedder_present,
        total,
        progress.skipped.load(AtomicOrdering::Acquire),
        total_vector_count,
    );
    let staging_resolution = staging::resolve_staging(memory_aborted, &reindex_outcome);

    // Issue #109, Phase 1: flip the lexical stage to Ready.
    {
        let files_done = progress.indexed.load(AtomicOrdering::Acquire);
        // Issue #879: pass the corpus TOTAL chunk count, not the per-run counter.
        let corpus_total_chunks = {
            let indexer = handle.indexer.read().await;
            indexer
                .corpus_arc()
                .and_then(|c| c.chunk_count().ok())
                .unwrap_or_else(|| {
                    let in_mem = indexer.chunk_count();
                    if in_mem > 0 {
                        in_mem
                    } else {
                        progress.total_chunks.load(AtomicOrdering::Acquire)
                    }
                })
        };
        mark_lexical_ready_semantic_in_progress(
            &handle,
            files_done,
            corpus_total_chunks,
            total_vector_count,
        )
        .await;
    }

    // Issue #29: force a final HNSW snapshot so tail batches are durable.
    {
        let indexer = handle.indexer.read().await;
        indexer.force_incremental_persist();
    }

    // Issue #603: resolve the atomic corpus swap.
    resolve_corpus_swap(
        &handle,
        &index_id,
        &canonical_root,
        corpus_swap_tmp.as_deref(),
        &staging_resolution,
        &reindex_outcome,
        memory_aborted,
    )
    .await;

    // Issue #601: a zero-vector embed failure on a full-pipeline index is a HARD failure.
    if let Some(reason) = reindex_outcome.failure_reason() {
        let embed_failure_count = progress.errors.load(AtomicOrdering::Acquire);
        tracing::error!(
            "reindex[{}]: FAILED — {reason} (walked_files={}, vectors=0, \
             embed_failure_count={})",
            index_id.0,
            total,
            embed_failure_count,
        );
        mark_reindex_failed(&handle, reason).await;
        progress.status.store(ReindexStatus::Failed);
        progress
            .push(serde_json::json!({
                "event": "error",
                "index_id": index_id.0,
                "message": reason,
                "embed_failure_count": embed_failure_count,
                "walked_files": total,
                "vector_count": 0,
                "fatal": true,
            }))
            .await;
        term_guard.disarm();
        stop_pollers(
            poller_stop,
            poller_handle,
            embedderd_poller_stop,
            embedderd_poller_handle,
        )
        .await;
        if let Some(ref q) = quarantine {
            q.record_failure(&index_id);
        }
        schedule_progress_cleanup(cleanup_map, cleanup_id);
        return;
    }

    // Issue #109, Phase 1 / #2211: flip the semantic stage to `Ready` — but
    // ONLY when embedding actually ran to completion as part of this pass.
    // When `defer_embed` is active with an embedder wired, the real
    // embedding happens in the background pass spawned below (after this
    // function returns); marking semantic Ready here reported a false
    // "ready" signal for the entire duration of that background pass — see
    // `validate::semantic_ready_now` for the full rationale. In that case
    // semantic stays `InProgress` (already set by
    // `mark_lexical_ready_semantic_in_progress` above) until
    // `spawn_deferred_embed_pass` itself marks it `Ready`.
    if validate::semantic_ready_now(defer_embed, embedder_present) {
        mark_semantic_ready_graph_in_progress(
            &handle,
            total_vector_count,
            progress.total_chunks.load(AtomicOrdering::Acquire),
        )
        .await;
    }

    // Phase 3: rebuild the symbol graph.
    let kg = rebuild_kg(
        &handle,
        &progress,
        &index_id,
        &peak_rss_atomic,
        mem_limit,
        mem_limit_hit,
        &mem_abort,
    )
    .await;

    // Stop the background pollers.
    stop_pollers(
        poller_stop,
        poller_handle,
        embedderd_poller_stop,
        embedderd_poller_handle,
    )
    .await;

    // Final synchronous sample for the sidecar.
    if let Some(pid_slot) = embedderd_pid_slot.as_ref() {
        let pid = pid_slot.load(AtomicOrdering::Acquire);
        if let Some(rss) = current_rss_mb_for_pid(pid) {
            let prev = peak_embedderd_rss_atomic.load(AtomicOrdering::Acquire);
            if rss > prev {
                peak_embedderd_rss_atomic.store(rss, AtomicOrdering::Release);
            }
        }
    }
    let embedderd_peak_rss_mb: Option<u64> = if embedderd_pid_slot.is_some() {
        let v = peak_embedderd_rss_atomic.load(AtomicOrdering::Acquire);
        if v > 0 {
            Some(v)
        } else {
            None
        }
    } else {
        None
    };

    // Issue #120: distinguish memory-abort from clean completion.
    let aborted_memory = mem_limit_hit || mem_abort.load(AtomicOrdering::Acquire);
    if aborted_memory {
        progress.status.store(ReindexStatus::AbortedMemory);
        if let Some(map) = aborted_map.as_ref() {
            map.insert(index_id.clone(), Instant::now());
        }
    } else {
        progress.status.store(ReindexStatus::Complete);
        // Issue #75: refresh the captured HEAD SHA.
        let new_sha = crate::core::git::head_sha(&handle.root_path);
        *handle.indexed_head_sha.write().await = new_sha;
        // Issue #878: stamp the authoritative last-indexed timestamp.
        *handle.last_indexed_at.write().await = Some(now_rfc3339());
    }

    // Final synchronous RSS poll so the peak reflects post-KG memory.
    if let Some(rss) = current_rss_mb() {
        let prev = peak_rss_atomic.load(AtomicOrdering::Acquire);
        if rss > prev {
            peak_rss_atomic.store(rss, AtomicOrdering::Release);
        }
    }
    let peak_rss_mb = peak_rss_atomic.load(AtomicOrdering::Acquire);
    let indexed_final = progress.indexed.load(Ordering::Acquire);
    let total_chunks = progress.total_chunks.load(Ordering::Acquire);
    let skipped_final = progress.skipped.load(Ordering::Acquire);
    let elapsed_ms = started.elapsed().as_millis() as u64;
    let indexed_new = indexed_final.saturating_sub(skipped_final);
    tracing::info!(
        "reindex complete: index={} files={} indexed_new={} skipped={} chunks={} \
         elapsed_ms={} peak_rss_mb={} memory_limit_hit={}",
        index_id.0,
        indexed_final,
        indexed_new,
        skipped_final,
        total_chunks,
        elapsed_ms,
        peak_rss_mb,
        mem_limit_hit,
    );
    // Issue #744: emit a concise per-phase timing summary.
    let model_load_approx_ms = elapsed_ms
        .saturating_sub(walk_ms)
        .saturating_sub(total_parse_ms)
        .saturating_sub(total_embed_ms)
        .saturating_sub(total_bm25_ms)
        .saturating_sub(total_vector_upsert_ms)
        .saturating_sub(kg.kg_ms);
    // Issue #1174: include `defer_embed` in the timing log so operators can
    // distinguish "vector_upsert=0ms because deferred" from "0ms because the
    // embedder failed silently". On the defer path, vector_upsert_ms is always
    // 0 for the C1 fast pass — the real upsert happens in the background C2
    // pass logged separately by `spawn_deferred_embed_pass`.
    tracing::info!(
        "reindex phase timings: index={} walk={}ms parse={}ms \
         model_load_approx={}ms embed={}ms bm25={}ms vector_upsert={}ms \
         kg={}ms total={}ms defer_embed={}",
        index_id.0,
        walk_ms,
        total_parse_ms,
        model_load_approx_ms,
        total_embed_ms,
        total_bm25_ms,
        total_vector_upsert_ms,
        kg.kg_ms,
        elapsed_ms,
        defer_embed,
    );

    let run_totals = RunTotals {
        walk_ms,
        parse_ms: total_parse_ms,
        embed_ms: total_embed_ms,
        bm25_ms: total_bm25_ms,
        vector_upsert_ms: total_vector_upsert_ms,
        vector_count: total_vector_count,
        mem_limit_hit,
        chunks_dropped_by_cap: total_chunks_dropped_by_cap,
    };
    emit_complete_event(
        &progress,
        started,
        peak_rss_mb,
        embedderd_peak_rss_mb,
        &run_totals,
        &kg,
    )
    .await;

    // The terminal event has been emitted — disarm the guard.
    term_guard.disarm();

    // Issue #764: record success in the quarantine registry.
    if let Some(ref q) = quarantine {
        q.record_success(&index_id);
    }

    // Issue #112: refresh the per-index context embedding.
    refresh_context_embedding(&handle).await;

    // Issue #923: spawn the background embedding job if deferred.
    // Issue #2984 Phase 1: never spawn it for a skip_vector index — the
    // embedder must never run, not just be deferred.
    let has_embedder = handle.indexer.read().await.has_embedder();
    if defer_embed && !aborted_memory && has_embedder && !handle.skip_vector {
        // Issue #3748 slice A review finding 2: key the size-ordered
        // deferred-embed queue on the PENDING (un-embedded) chunk delta, not
        // `chunk_count()`'s total corpus size. `finish_reindex` runs after
        // EVERY reindex, including incremental ones — an incremental pass
        // over a 94k-chunk repo with one changed chunk has near-instant real
        // embed cost (`embed_deferred_chunks` only embeds what
        // `VectorStore::contains_many` says is missing), so sorting it by
        // the TOTAL corpus size would wrongly queue it behind small repos it
        // could have beaten. Only computed on the branch that actually
        // spawns — the `contains_many` bulk check costs nothing on the
        // common cold-index path (nothing embedded yet, so it returns the
        // full count) but is still real I/O-shaped work not worth paying
        // when defer_embed/skip_vector/aborted_memory already ruled out
        // spawning.
        let pending_chunks = handle.indexer.read().await.pending_embed_count().await;
        spawn_deferred_embed_pass(handle, progress.clone(), pending_chunks);
    }

    // Issue #75: GC the progress entry after a short delay.
    schedule_progress_cleanup(cleanup_map, cleanup_id);
}

/// Stop both RSS pollers and await their join handles.
///
/// Why: the same teardown sequence is needed on two code paths (hard failure
/// exit and the normal success path). Extracted to avoid duplication.
/// What: sets the stop flag, awaits the join handle. No-ops for `None` handles.
/// Test: exercised on every `finish_reindex` code path.
async fn stop_pollers(
    poller_stop: Arc<AtomicBool>,
    poller_handle: tokio::task::JoinHandle<()>,
    embedderd_stop: Option<Arc<AtomicBool>>,
    embedderd_handle: Option<tokio::task::JoinHandle<()>>,
) {
    poller_stop.store(true, AtomicOrdering::Release);
    let _ = poller_handle.await;
    if let Some(stop) = embedderd_stop {
        stop.store(true, AtomicOrdering::Release);
    }
    if let Some(h) = embedderd_handle {
        let _ = h.await;
    }
}

/// Resolve the atomic corpus swap: commit, rollback, or write indexed_root only.
///
/// Why: extracted from `finish_reindex` to keep line count manageable.
/// What: if `corpus_swap_tmp` is `Some`, commits or aborts the staged swap;
/// otherwise writes `indexed_root` when the reindex succeeded without staging.
/// Test: `reindex_walks_directory_and_emits_events` exercises the commit path.
async fn resolve_corpus_swap(
    handle: &IndexHandle,
    index_id: &IndexId,
    canonical_root: &Path,
    corpus_swap_tmp: Option<&Path>,
    staging_resolution: &staging::StagingResolution,
    reindex_outcome: &validate::ReindexOutcome,
    memory_aborted: bool,
) {
    if let Some(tmp_path) = corpus_swap_tmp {
        if staging_resolution.is_commit() {
            commit_staged_corpus_swap(handle, index_id, tmp_path).await;
            if let Err(e) = handle.write_indexed_root(canonical_root).await {
                tracing::warn!(
                    "reindex[{}]: failed to persist indexed_root {} ({e}) — \
                     a future root-move may not re-relativize paths",
                    index_id.0,
                    canonical_root.display(),
                );
            }
        } else {
            if let staging::StagingResolution::Rollback { reason } = staging_resolution {
                tracing::warn!(
                    "reindex[{}]: rolling back staged corpus — {reason}",
                    index_id.0,
                );
            }
            abort_staged_corpus_swap(handle, index_id, tmp_path).await;
        }
    } else if reindex_outcome.is_ready() && !memory_aborted {
        if let Err(e) = handle.write_indexed_root(canonical_root).await {
            tracing::debug!(
                "reindex[{}]: indexed_root not persisted (no durable corpus): {e}",
                index_id.0,
            );
        }
    }
}

/// Run Phase 3: rebuild the symbol graph and flip the graph stage to Ready.
///
/// Why: extracted from `finish_reindex` to reduce function length.
/// What: emits `kg_start` / `kg_complete` SSE events; rebuilds the KG; flips
/// the graph stage. Returns the `KgRebuildOutcome` for logging.
/// Test: `reindex_walks_directory_and_emits_events` exercises KG rebuild.
#[allow(clippy::too_many_arguments)]
async fn rebuild_kg(
    handle: &Arc<IndexHandle>,
    progress: &Arc<ReindexProgress>,
    index_id: &IndexId,
    peak_rss_atomic: &Arc<AtomicU64>,
    mem_limit: Option<u64>,
    mem_limit_hit: bool,
    mem_abort: &Arc<AtomicBool>,
) -> KgRebuildOutcome {
    if handle.skip_kg {
        tracing::info!(
            "reindex[{}]: KG construction skipped (skip_kg=true)",
            index_id.0,
        );
        return KgRebuildOutcome {
            symbol_count: 0,
            edge_count: 0,
            kg_ms: 0,
            kg_skipped: true,
        };
    }

    // Emit `kg_start` so the CLI activates the KG progress bar (issue #401).
    progress
        .push(serde_json::json!({
            "event": "kg_start",
            "index_id": index_id.0,
        }))
        .await;

    let outcome = rebuild_symbol_graph_for_reindex(handle).await;

    // Issue #401: emit `kg_complete` with timing + graph stats.
    progress
        .push(serde_json::json!({
            "event": "kg_complete",
            "index_id": index_id.0,
            "kg_ms": outcome.kg_ms,
            "symbol_count": outcome.symbol_count,
            "edge_count": outcome.edge_count,
        }))
        .await;

    mark_graph_ready(handle).await;
    if mem_limit_hit || mem_abort.load(AtomicOrdering::Acquire) {
        tracing::warn!(
            "reindex: memory limit was breached during batch processing for \
             index {} (peak_rss={}MB, limit={:?}MB) — KG was still rebuilt \
             (symbols={}, edges={}) because graph construction is bounded by \
             TRUSTY_MAX_KG_NODES and independent of the embedding spike",
            index_id.0,
            peak_rss_atomic.load(AtomicOrdering::Acquire),
            mem_limit,
            outcome.symbol_count,
            outcome.edge_count,
        );
    }
    outcome
}
