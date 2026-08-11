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
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering as AtomicOrdering};
use std::sync::Arc;
use std::time::Instant;

use super::completion::{emit_complete_event, RunTotals};
use super::defer_embed::spawn_deferred_embed_pass;
use super::finish_teardown::{rebuild_kg, resolve_corpus_swap, resolve_hnsw_swap, stop_pollers};
use super::guard::ReindexTerminationGuard;
use super::hnsw_swap::HnswSwapPaths;
use super::progress::{ReindexProgress, ReindexStatus};
use super::quarantine::ReindexQuarantine;
use super::stage_timings::StageTimings;
use super::stages::{
    mark_lexical_ready_semantic_in_progress, mark_reindex_failed,
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
    pub(super) hnsw_swap_paths: Option<HnswSwapPaths>,
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
    /// Issue #3979: this run adopted an interrupted run's staging corpus.
    ///
    /// Why: the chunks it inherited were embedded into the previous run's
    /// STAGED HNSW snapshot, which was never promoted — so the live snapshot
    /// this daemon booted from does not contain their vectors. The staged
    /// snapshot is deliberately not adopted (it is written by the periodic
    /// persister on its own schedule, so it is not transactionally tied to the
    /// staged corpus); instead this flag forces the vector catch-up pass that
    /// already exists for `defer_embed`, which embeds exactly the chunks the
    /// vector store is missing.
    pub(super) resumed: bool,
}

/// Post-batch-loop completion: prune, KG rebuild, poller teardown, and events.
///
/// Why: extracted from `runner.rs` to bring it under the 500-SLOC cap.
/// What: runs all work that follows the pipelined batch loop and emits the
/// terminal SSE `complete` event. Returns after scheduling progress GC.
/// Test: `reindex_walks_directory_and_emits_events` (primary integration test).
///
/// `stage_timings` arrives pre-populated by the runner (hash cache, carryover,
/// pipeline) and is completed here with the prune and swap-commit costs before
/// being logged and attached to the `complete` event — see #5024.
pub(super) async fn finish_reindex(
    ctx: FinishCtx,
    totals: BatchTotals,
    mut stage_timings: StageTimings,
) {
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
        hnsw_swap_paths,
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
        resumed,
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
    // #5024: scales with the walked-file set, so it is measured separately.
    if corpus_swap_tmp.is_some() && !force && !memory_aborted {
        let prune_started = Instant::now();
        super::prune::prune_deleted_files_from_staging(
            &handle,
            &walked_files,
            &canonical_root,
            &hashes,
            &index_id,
        )
        .await;
        stage_timings.prune_ms = prune_started.elapsed().as_millis() as u64;
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

    // Issue #29 / #3970: force a final HNSW snapshot so tail batches are
    // durable, and resolve the staged HNSW swap. When `hnsw_swap_paths` is
    // `Some` (the common case), `resolve_hnsw_swap` below performs an AWAITED
    // final save straight to the staging path and — only on a `Commit`
    // resolution — atomically publishes it to the live path; that awaited
    // save subsumes the old fire-and-forget `force_incremental_persist()`
    // call for this codepath. When staging was never begun (path
    // unresolvable at reindex start), fall back to the original detached
    // forced save direct-to-live, exactly as before this fix.
    if hnsw_swap_paths.is_none() {
        let indexer = handle.indexer.read().await;
        indexer.force_incremental_persist();
    }
    // #5024: the HNSW resolve subsumes an AWAITED full snapshot save, so it is
    // real per-run cost, not just a rename.
    let hnsw_commit_started = Instant::now();
    resolve_hnsw_swap(
        &handle,
        &index_id,
        hnsw_swap_paths.as_ref(),
        &staging_resolution,
    )
    .await;
    stage_timings.hnsw_commit_ms = hnsw_commit_started.elapsed().as_millis() as u64;

    // Issue #603: resolve the atomic corpus swap.
    let corpus_commit_started = Instant::now();
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
    stage_timings.corpus_commit_ms = corpus_commit_started.elapsed().as_millis() as u64;

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
    // #5024: the join waits out the remainder of the poller's current tick, so
    // this is a size-independent tax — measured, not left in the residual.
    let poller_stop_started = Instant::now();
    stop_pollers(
        poller_stop,
        poller_handle,
        embedderd_poller_stop,
        embedderd_poller_handle,
    )
    .await;
    stage_timings.poller_stop_ms = poller_stop_started.elapsed().as_millis() as u64;

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
        // #4391: the in-memory stamp dies with the handle — persist it so the
        // next boot compares against what was actually indexed, not live HEAD.
        crate::service::boot_markers::persist_indexed_head_sha(&handle.id.0, new_sha.as_deref());
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
    // #5024: the old `model_load_approx_ms` was `elapsed` minus the subsystem
    // accumulators, which lumped the hash-cache load, the carryover copy, the
    // prune pass, and both swap commits into one number an operator could not
    // act on. Those five now carry their own clocks; `other_ms` is what is
    // genuinely left over.
    let other_ms = stage_timings.other_ms(elapsed_ms, walk_ms, kg.kg_ms);
    // Issue #1174: include `defer_embed` in the timing log so operators can
    // distinguish "vector_upsert=0ms because deferred" from "0ms because the
    // embedder failed silently". On the defer path, vector_upsert_ms is always
    // 0 for the C1 fast pass — the real upsert happens in the background C2
    // pass logged separately by `spawn_deferred_embed_pass`.
    tracing::info!(
        "reindex phase timings: index={} walk={}ms hash_cache={}ms \
         carryover={}ms pipeline={}ms prune={}ms hnsw_commit={}ms \
         corpus_commit={}ms kg={}ms poller_stop={}ms other={}ms total={}ms | \
         within pipeline: parse={}ms embed={}ms bm25={}ms vector_upsert={}ms \
         defer_embed={}",
        index_id.0,
        walk_ms,
        stage_timings.hash_cache_ms,
        stage_timings.carryover_ms,
        stage_timings.pipeline_ms,
        stage_timings.prune_ms,
        stage_timings.hnsw_commit_ms,
        stage_timings.corpus_commit_ms,
        kg.kg_ms,
        stage_timings.poller_stop_ms,
        other_ms,
        elapsed_ms,
        total_parse_ms,
        total_embed_ms,
        total_bm25_ms,
        total_vector_upsert_ms,
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
        // #5024: same breakdown the log line above prints, so a client polling
        // the SSE stream sees it without scraping stderr.
        stages: stage_timings,
        other_ms,
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
    // #3979: `|| resumed` — a resumed run's inherited chunks have no vector in
    // the live HNSW snapshot (see `FinishCtx::resumed`), so a non-defer_embed
    // index must also run the catch-up or its semantic lane would be short
    // exactly those chunks until the next full reindex. The pass embeds only
    // what `VectorStore::contains_many` reports missing, so on a non-resumed
    // run this condition is unchanged and on a resumed one it is near-free
    // when nothing is actually missing.
    let has_embedder = handle.indexer.read().await.has_embedder();
    if (defer_embed || resumed) && !aborted_memory && has_embedder && !handle.skip_vector {
        // Issue #3748 slice A review finding 2: key the size-ordered
        // deferred-embed queue on the PENDING (un-embedded) chunk delta, not
        // `chunk_count()`'s total corpus size. `finish_reindex` runs after
        // EVERY reindex, including incremental ones — an incremental pass
        // over a 94k-chunk repo with one changed chunk has near-instant real
        // embed cost (`embed_deferred_chunks` only embeds what
        // `VectorStore::contains_many` says is missing), so sorting it by
        // the TOTAL corpus size would wrongly queue it behind small repos it
        // could have beaten. Only computed on the branch that actually
        // spawns — `pending_embed_count` itself skips the chunk-id
        // collection and `contains_many` call entirely on the no-embedder/
        // no-store path (see its docs), so the common cold-index path pays
        // only a cheap `chunk_count()`.
        //
        // Lock-span note (review round 2): `handle.indexer.read().await`'s
        // guard is a temporary that lives for the full
        // `pending_embed_count().await` call, including its internal
        // `VectorStore::contains_many` lookup — `pending_embed_count` takes
        // `&self`, so Rust keeps the caller's borrow alive for the entire
        // async call regardless of what runs inside it; narrowing further
        // would mean splitting id-collection (needs `&self`) from the store
        // lookup (doesn't) across two indexer accessor methods purely for
        // this one caller. Not done: `contains_many` is an in-process
        // vector-store membership check (hash/id lookups against an
        // already-resident index, not network or disk I/O), so the extra
        // hold time on `handle.indexer`'s read lock is small and bounded by
        // corpus size, not worth the added public-API surface.
        let pending_chunks = handle.indexer.read().await.pending_embed_count().await;
        // #4390: record the pass as outstanding BEFORE it is queued. C2 commits
        // its vectors once at the very end, so a stop anywhere inside it
        // persists none of them; this marker is the only thing that survives to
        // tell the next boot the pass is still owed.
        crate::service::boot_markers::persist_deferred_embed_pending(&handle.id.0, true);
        spawn_deferred_embed_pass(handle, progress.clone(), pending_chunks);
    }

    // Issue #75: GC the progress entry after a short delay.
    schedule_progress_cleanup(cleanup_map, cleanup_id);
}
