//! Core async reindex runner extracted from `orchestrator.rs`.
//!
//! Why: `orchestrator.rs` exceeded the 500-SLOC production cap (issue #1175 follow-up).
//! This module holds Phase 1 (walk) and Phase 2 (pipelined parse/embed/commit).
//! Post-loop completion (prune, KG rebuild, corpus swap, terminal event) lives
//! in `finish.rs`; the RSS pollers live in `pollers.rs`.
//!
//! What: exports `run_reindex`, a `pub(super)` async function called by
//! `orchestrator::spawn_reindex_with_cleanup`.
//!
//! Test: covered by `reindex_walks_directory_and_emits_events` and the
//! integration tests in `tests.rs`.

use crate::core::memguard::{current_rss_mb, current_rss_mb_for_pid, index_memory_limit_mb};
use crate::core::registry::{IndexHandle, IndexId};
use dashmap::DashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering as AtomicOrdering};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;

use super::batch::{
    commit_parsed_and_finalize, prepare_and_parse_batch, BatchCtx, REINDEX_BATCH_SIZE,
};
use super::corpus_swap::begin_staged_corpus_swap;
use super::finish::{BatchTotals, FileHashes, FinishCtx};
use super::guard::ReindexTerminationGuard;
use super::hash::hashes_for;
use super::hash_cache;
use super::progress::{ReindexProgress, ReindexStatus};
use super::quarantine::ReindexQuarantine;
use super::semaphore::{reindex_semaphore_for, BACKGROUND_QUEUE_DEPTH};
use super::stages::{mark_reindex_failed, now_rfc3339, schedule_progress_cleanup};
use super::staging;
use super::validate;

/// Full three-phase async reindex body, spawned by `spawn_reindex_with_cleanup`.
///
/// Why: extracted from `orchestrator.rs` so each file stays under the 500-SLOC
/// production cap (issue #1175). Holds the semaphore acquisition, Phase 1 (walk
/// + filter), and Phase 2 (pipelined parse/embed/commit).
///
/// What: acquires the correct semaphore, runs Phases 1 and 2, then delegates
/// to `finish::finish_reindex` for KG rebuild, corpus swap, event emission, and GC.
///
/// Test: `reindex_walks_directory_and_emits_events` (primary integration test);
/// `interactive_reindex_not_starved_by_background` covers semaphore prioritisation.
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_reindex(
    handle: Arc<IndexHandle>,
    progress: Arc<ReindexProgress>,
    force: bool,
    cleanup_map: Option<Arc<DashMap<IndexId, Arc<ReindexProgress>>>>,
    aborted_map: Option<Arc<DashMap<IndexId, Instant>>>,
    embedderd_pid_slot: Option<Arc<AtomicU32>>,
    priority: bool,
    quarantine: Option<ReindexQuarantine>,
) {
    use std::sync::atomic::Ordering;

    let cleanup_id = handle.id.clone();

    // Issue #458: route to the correct semaphore based on priority.
    let _permit = reindex_semaphore_for(priority)
        .acquire()
        .await
        .expect("reindex semaphore is never closed");
    // Decrement the background queue counter once the permit is held.
    if !priority {
        BACKGROUND_QUEUE_DEPTH.fetch_sub(1, AtomicOrdering::Relaxed);
    }

    // Arm the termination guard. Any early exit — panic, early return, or
    // `.await` cancellation — fires `ReindexTerminationGuard::drop`, which
    // broadcasts an error event and marks the status `Failed`. The guard is
    // disarmed just after `emit_complete_event` confirms the normal terminal
    // event has been sent.
    let term_guard = ReindexTerminationGuard::new(Arc::clone(&progress));

    let started = Instant::now();
    // Issue #602 — portable paths.
    let root = handle.root_path.clone();
    let canonical_root = validate::canonical_walk_root(&root);
    let index_id: IndexId = handle.id.clone();

    // Issue #109, Phase 1: reset the staged-pipeline status surface.
    super::stages::reset_stages_for_reindex(&handle).await;

    // Phase 1: walk + filter the source tree.
    {
        let mut diag = handle.walk_diagnostics.write().await;
        diag.last_walk_started_at = Some(now_rfc3339());
        diag.last_walk_files_seen = 0;
        diag.last_walk_files_skipped = 0;
        diag.last_walk_error = None;
    }
    // Issue #744: stamp the walk end time.
    let walk = super::orchestrator::collect_files_to_index(&handle);
    let walk_ms = started.elapsed().as_millis() as u64;
    let total = walk.files.len();
    {
        let mut diag = handle.walk_diagnostics.write().await;
        diag.last_walk_files_seen = total as u64;
        diag.last_walk_files_skipped = walk.skipped_dirs as u64;
        if total == 0 {
            let reason = if !handle.root_path.exists() {
                format!("root path does not exist: {}", handle.root_path.display())
            } else {
                format!(
                    "walk produced zero files under {}; check gitignore rules, \
                     path_filter, and extension allow-list",
                    handle.root_path.display()
                )
            };
            diag.last_walk_error = Some(reason);
        }
    }
    progress.total_files.store(total, Ordering::Release);

    // Issues #840 / #662: load the persisted hash cache BEFORE emitting
    // the `start` event so `hashes_loaded` is available for the event payload.
    let hashes: FileHashes = hashes_for(&index_id);
    // Issue #602: detect a root move against the corpus's persisted
    // `indexed_root` and, for legacy (non-colocated) indexes, clear the
    // hash cache so every file is re-written relative to the new canonical root.
    //
    // Issue #1073: for colocated indexes the chunk and hash keys are
    // ROOT-RELATIVE (#402), so they are valid at any root location — a pure
    // move changes the root prefix only. Do NOT clear the hash cache on a
    // root move for colocated indexes.
    let prior_indexed_root = handle.read_indexed_root().await.unwrap_or(None);
    let root_moved =
        validate::needs_path_relativization(prior_indexed_root.as_deref(), &canonical_root);
    let is_colocated = crate::service::colocated_storage::has_colocated_storage(&canonical_root);
    let hashes_loaded: usize = if force {
        hashes.clear();
        hash_cache::clear_persisted(&handle).await;
        0
    } else if root_moved && !is_colocated {
        tracing::warn!(
            "reindex[{}]: legacy index root moved from {:?} to {} — clearing hash \
             cache to re-relativize all chunk paths against the new root",
            index_id.0,
            prior_indexed_root,
            canonical_root.display(),
        );
        hashes.clear();
        hash_cache::clear_persisted(&handle).await;
        0
    } else if root_moved {
        // Issue #1073: colocated index moved — keys are root-relative and
        // survive the move.
        tracing::info!(
            "reindex[{}]: colocated index root moved from {:?} to {} — \
             keys are root-relative (#402); preserving hash cache (no re-embed)",
            index_id.0,
            prior_indexed_root,
            canonical_root.display(),
        );
        if let Err(e) = handle.write_indexed_root(&canonical_root).await {
            tracing::warn!(
                "reindex[{}]: failed to update indexed_root after colocated \
                 root move ({e}) — next reindex will re-detect the move",
                index_id.0,
            );
        }
        hash_cache::load_into_cache(&handle, &hashes).await
    } else {
        hash_cache::load_into_cache(&handle, &hashes).await
    };

    // Issue #317: emit `walk_complete` BEFORE `start`.
    progress
        .push(serde_json::json!({
            "event": "walk_complete",
            "total_files": total,
            "index_id": index_id.0,
        }))
        .await;
    // Issue #840 Part 2: `hashes_loaded` shows whether warm-skip is primed.
    // Issue #929: `defer_embed` tells the CLI whether embedding will run
    // in the background.
    let effective_defer_embed = handle.defer_embed && !handle.lexical_only;
    progress
        .push(serde_json::json!({
            "event": "start",
            "total_files": total,
            "index_id": index_id.0,
            "root_path": root,
            "force": force,
            "lexical_only": handle.lexical_only,
            "hashes_loaded": hashes_loaded,
            "defer_embed": effective_defer_embed,
        }))
        .await;

    // Issue #744 — concurrent embedder warm-up.
    if !handle.lexical_only {
        let warm_indexer = Arc::clone(&handle.indexer);
        let warm_index_id = index_id.0.clone();
        let warm_ms = started;
        tokio::spawn(async move {
            tracing::debug!("reindex[{warm_index_id}]: starting concurrent embedder warm-up");
            let t0 = std::time::Instant::now();
            warm_indexer.read().await.warm_embedder().await;
            tracing::info!(
                "reindex[{warm_index_id}]: embedder warm-up complete in {}ms \
                 (started {}ms after reindex began)",
                t0.elapsed().as_millis(),
                warm_ms.elapsed().as_millis(),
            );
        });
    }

    // Issue #28, Phase 4 + #603: stage the rebuilt corpus.
    let corpus_swap_tmp: Option<PathBuf> =
        if staging::should_stage(handle.indexer.read().await.has_corpus_store()) {
            match begin_staged_corpus_swap(&handle, &index_id, force).await {
                Ok(path) => path,
                Err(e) => {
                    tracing::error!(
                        "reindex[{}]: ABORTING incremental reindex — carryover copy \
                         from live corpus failed ({e}); live corpus is intact",
                        index_id.0
                    );
                    mark_reindex_failed(&handle, "carryover copy failed — live corpus intact")
                        .await;
                    progress.status.store(ReindexStatus::Failed);
                    progress
                        .push(serde_json::json!({
                            "event": "error",
                            "index_id": index_id.0,
                            "message": format!(
                                "incremental reindex aborted: failed to copy live corpus \
                                 into staging store ({e}) — live corpus is intact"
                            ),
                            "fatal": true,
                        }))
                        .await;
                    // term_guard drops here → broadcasts error event via Drop.
                    drop(term_guard);
                    schedule_progress_cleanup(cleanup_map, cleanup_id);
                    if let Some(ref q) = quarantine {
                        q.record_failure(&index_id);
                    }
                    return;
                }
            }
        } else {
            None
        };

    // Per-subsystem timing accumulators.
    let mut total_parse_ms: u64 = 0;
    let mut total_embed_ms: u64 = 0;
    let mut total_bm25_ms: u64 = 0;
    let mut total_vector_upsert_ms: u64 = 0;
    let mut total_vector_count: usize = 0;
    let mut total_chunks_dropped_by_cap: usize = 0;

    // Memory-protection state (issues #76, #82).
    let mem_limit = index_memory_limit_mb();
    let mem_abort = Arc::new(AtomicBool::new(false));
    let peak_rss_atomic = Arc::new(AtomicU64::new(current_rss_mb().unwrap_or(0)));
    let mut mem_limit_hit: bool = false;

    // Spawn the background poller.
    let (poller_handle, poller_stop) = super::pollers::spawn_memory_poller(
        mem_limit,
        mem_abort.clone(),
        peak_rss_atomic.clone(),
        index_id.0.clone(),
    );

    // Issue #282: spawn the embedderd RSS poller if available.
    let peak_embedderd_rss_atomic = Arc::new(AtomicU64::new(0));
    let (embedderd_poller_handle, embedderd_poller_stop) =
        if let Some(pid_slot) = embedderd_pid_slot.as_ref() {
            let initial_pid = pid_slot.load(AtomicOrdering::Acquire);
            if let Some(rss) = current_rss_mb_for_pid(initial_pid) {
                peak_embedderd_rss_atomic.store(rss, AtomicOrdering::Release);
            }
            let (h, s) = super::pollers::spawn_embedderd_rss_poller(
                Arc::clone(pid_slot),
                Arc::clone(&peak_embedderd_rss_atomic),
            );
            (Some(h), Some(s))
        } else {
            (None, None)
        };

    // Phase 2: pipelined parse/embed/commit (issue #20).
    let ctx = BatchCtx {
        handle: handle.clone(),
        progress: progress.clone(),
        root: canonical_root.clone(),
        index_id: index_id.clone(),
        hashes: hashes.clone(),
        mem_limit,
        mem_abort: mem_abort.clone(),
        peak_rss_atomic: peak_rss_atomic.clone(),
        started,
        total,
        lexical_only: handle.lexical_only,
        defer_embed: handle.defer_embed && !handle.lexical_only,
        embedder_pid_slot: embedderd_pid_slot.clone(),
    };

    let batches: Vec<Vec<PathBuf>> = walk
        .files
        .chunks(REINDEX_BATCH_SIZE)
        .map(|b| b.to_vec())
        .collect();

    // Bounded channel — capacity 1 keeps memory in the same envelope as
    // the prior sequential loop (one batch in transit, one being committed).
    let (tx, mut rx) = mpsc::channel::<super::batch::ParsedReadyBatch>(1);
    let producer_ctx = ctx.clone();
    let producer_mem_abort = mem_abort.clone();
    let producer_index_id = index_id.0.clone();
    let producer = tokio::spawn(async move {
        for batch in batches {
            if producer_mem_abort.load(AtomicOrdering::Acquire) {
                let rss = current_rss_mb().unwrap_or(0);
                tracing::warn!(
                    "reindex: memory limit hit before batch (rss={}MB, \
                     limit={:?}MB) — producer halting for index {}",
                    rss,
                    producer_ctx.mem_limit,
                    producer_index_id
                );
                break;
            }
            let Some(ready) = prepare_and_parse_batch(&producer_ctx, &batch).await else {
                continue;
            };
            if tx.send(ready).await.is_err() {
                break;
            }
        }
    });

    // Consumer loop: commits batches sequentially.
    while let Some(ready) = rx.recv().await {
        let outcome = commit_parsed_and_finalize(&ctx, ready).await;
        total_parse_ms = total_parse_ms.saturating_add(outcome.parse_ms);
        total_embed_ms = total_embed_ms.saturating_add(outcome.embed_ms);
        total_bm25_ms = total_bm25_ms.saturating_add(outcome.bm25_ms);
        total_vector_upsert_ms = total_vector_upsert_ms.saturating_add(outcome.vector_upsert_ms);
        total_vector_count = total_vector_count.saturating_add(outcome.vector_count);
        total_chunks_dropped_by_cap =
            total_chunks_dropped_by_cap.saturating_add(outcome.chunks_dropped_by_cap);
        if outcome.chunks_dropped_by_cap > 0 {
            progress
                .chunks_dropped_by_cap
                .fetch_add(outcome.chunks_dropped_by_cap, Ordering::Release);
        }
        if outcome.mem_limit_hit {
            mem_limit_hit = true;
            rx.close();
            while rx.recv().await.is_some() {}
            break;
        }
    }
    let _ = producer.await;

    // Delegate post-loop work: prune, KG rebuild, corpus swap, terminal event, GC.
    let finish_ctx = FinishCtx {
        handle,
        progress,
        index_id,
        canonical_root,
        walked_files: walk.files,
        hashes,
        total,
        started,
        defer_embed: ctx.defer_embed,
        corpus_swap_tmp,
        mem_abort,
        peak_rss_atomic,
        peak_embedderd_rss_atomic,
        embedderd_pid_slot,
        poller_handle,
        poller_stop,
        embedderd_poller_handle,
        embedderd_poller_stop,
        term_guard,
        cleanup_map,
        cleanup_id,
        aborted_map,
        quarantine,
        mem_limit,
        force,
    };
    let batch_totals = BatchTotals {
        walk_ms,
        parse_ms: total_parse_ms,
        embed_ms: total_embed_ms,
        bm25_ms: total_bm25_ms,
        vector_upsert_ms: total_vector_upsert_ms,
        vector_count: total_vector_count,
        chunks_dropped_by_cap: total_chunks_dropped_by_cap,
        mem_limit_hit,
    };
    super::finish::finish_reindex(finish_ctx, batch_totals).await;
}
