//! Reindex orchestrator: top-level task spawn + pipelined batch loop.
//!
//! Why: the daemon spawns one background `tokio::task` per reindex request.
//! This module contains the entry points (`spawn_reindex`,
//! `spawn_reindex_with_cleanup`) and the helper that walks + filters the
//! source tree (`collect_files_to_index`) and the two RSS pollers
//! (`spawn_memory_poller`, `spawn_embedderd_rss_poller`).
//!
//! What: the orchestrator drives the three reindex phases:
//!   1. Walk source tree → collect file list.
//!   2. Pipelined parse/embed/commit loop (producer + consumer tasks).
//!   3. Symbol-graph rebuild + terminal SSE event + context-embedding refresh.
//!
//! Test: `reindex_walks_directory_and_emits_events` is the primary end-to-end
//! coverage. Semaphore prioritisation is covered by
//! `interactive_reindex_not_starved_by_background`.

use crate::core::memguard::{current_rss_mb, current_rss_mb_for_pid, index_memory_limit_mb};
use crate::core::registry::{IndexHandle, IndexId};
use crate::service::walker::{walk_source_files_with_options, WalkOptions};
use dashmap::DashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering as AtomicOrdering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

use super::batch::{
    commit_parsed_and_finalize, prepare_and_parse_batch, BatchCtx, REINDEX_BATCH_SIZE,
};
use super::completion::{
    emit_complete_event, rebuild_symbol_graph_for_reindex, KgRebuildOutcome, RunTotals,
};
use super::corpus_swap::{
    abort_staged_corpus_swap, begin_staged_corpus_swap, commit_staged_corpus_swap,
};
use super::defer_embed::spawn_deferred_embed_pass;
use super::guard::ReindexTerminationGuard;
use super::hash::hashes_for;
use super::hash_cache;
use super::progress::{ReindexProgress, ReindexStatus};
use super::quarantine::ReindexQuarantine;
use super::semaphore::{reindex_semaphore_for, BACKGROUND_QUEUE_DEPTH};
use super::stages::{
    mark_graph_ready, mark_lexical_ready_semantic_in_progress, mark_reindex_failed,
    mark_semantic_ready_graph_in_progress, now_rfc3339, refresh_context_embedding,
    schedule_progress_cleanup,
};
use super::staging;
use super::validate;

/// Spawn a background tokio task that walks `handle.root_path`, indexes each
/// source file, and emits progress events into `progress`.
///
/// Why: thin wrapper for callers that don't need GC, aborted-map tracking, or
/// the embedderd RSS poller. Always treated as interactive (priority=true).
/// What: delegates to `spawn_reindex_with_cleanup` with all optional maps as
/// `None` and `priority=true`.
/// Test: covered indirectly by the integration tests via `reindex_handler`.
pub fn spawn_reindex(handle: Arc<IndexHandle>, progress: Arc<ReindexProgress>, force: bool) {
    spawn_reindex_with_cleanup(handle, progress, force, None, None, None, true, None);
}

/// Walk every configured subtree under `handle.root_path`, apply repo-config
/// filters (`exclude_globs`, `extensions`), and de-duplicate.
///
/// Why: extracted from `spawn_reindex_with_cleanup` (issue #98) so the
/// orchestrator body is dominated by control flow rather than walker plumbing.
/// `include_paths` empty → walk the whole `root_path`; otherwise walk each
/// configured subtree and concatenate (this is how `trusty-search.yaml` slices
/// a polyrepo into independent indexes).
/// What: returns the merged `WalkResult` whose `files` are sorted and unique.
/// Test: covered by `reindex_honours_include_paths_filter` below.
fn collect_files_to_index(handle: &IndexHandle) -> crate::service::walker::WalkResult {
    let include_paths: Vec<PathBuf> = if handle.include_paths.is_empty() {
        vec![handle.root_path.clone()]
    } else {
        handle.include_paths.clone()
    };
    let mut walked_files: Vec<PathBuf> = Vec::new();
    let mut total_skipped_dirs: usize = 0;
    let walk_opts = WalkOptions {
        include_docs: handle.include_docs,
        respect_gitignore: handle.respect_gitignore,
    };
    for subtree in &include_paths {
        let w = walk_source_files_with_options(subtree, walk_opts);
        walked_files.extend(w.files);
        total_skipped_dirs = total_skipped_dirs.saturating_add(w.skipped_dirs);
    }

    // Apply repo-config filters (AND-composed on top of walker's built-in ignores).
    if !handle.exclude_globs.is_empty() {
        let excludes = handle.exclude_globs.clone();
        walked_files.retain(|p| !crate::core::repo_config::path_matches_any_glob(p, &excludes));
    }
    if !handle.extensions.is_empty() {
        let allowed = handle.extensions.clone();
        walked_files.retain(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .map(|e| allowed.iter().any(|x| x.eq_ignore_ascii_case(e)))
                .unwrap_or(false)
        });
    }

    // Issue #111: `path_filter` restricts indexing to files under immediate
    // subdirectories of `root_path` matching one of the configured glob patterns.
    if !handle.path_filter.is_empty() {
        let patterns = handle.path_filter.clone();
        let root =
            std::fs::canonicalize(&handle.root_path).unwrap_or_else(|_| handle.root_path.clone());
        walked_files.retain(|p| crate::core::registry::path_matches_filter(p, &root, &patterns));
    }

    // De-duplicate when multiple `include_paths` overlap.
    walked_files.sort();
    walked_files.dedup();

    crate::service::walker::WalkResult {
        files: walked_files,
        skipped_dirs: total_skipped_dirs,
    }
}

/// Spawn the background RSS poller that watches for `TRUSTY_MEMORY_LIMIT_MB`
/// breaches.
///
/// Why: extracted from `spawn_reindex_with_cleanup` (issue #98) so the
/// memory-protection plumbing is isolated from the batch loop. Always run
/// even when no `mem_limit` is configured so `peak_rss_mb` is accurate for
/// the final log line.
/// What: ticks every `MEM_POLL_INTERVAL`, updates `peak_rss` monotonically,
/// and trips `mem_abort` the first time RSS crosses `mem_limit`. Returns the
/// join handle plus a stop-flag the caller flips when the reindex finishes.
/// Test: `memory_limit_aborts_reindex_mid_batch` (memory-abort integration test).
fn spawn_memory_poller(
    mem_limit: Option<u64>,
    mem_abort: Arc<AtomicBool>,
    peak_rss: Arc<AtomicU64>,
    index_id: String,
) -> (tokio::task::JoinHandle<()>, Arc<AtomicBool>) {
    /// How often the background poller samples RSS.
    const MEM_POLL_INTERVAL: Duration = Duration::from_secs(1);

    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = stop.clone();
    let handle = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(MEM_POLL_INTERVAL);
        // Drop the immediate first tick so we don't double-sample with the
        // synchronous `current_rss_mb()` already done before spawning.
        ticker.tick().await;
        loop {
            if stop_clone.load(AtomicOrdering::Acquire) {
                break;
            }
            if let Some(rss) = current_rss_mb() {
                // Update peak monotonically (CAS loop).
                let mut prev = peak_rss.load(AtomicOrdering::Acquire);
                while rss > prev {
                    match peak_rss.compare_exchange_weak(
                        prev,
                        rss,
                        AtomicOrdering::AcqRel,
                        AtomicOrdering::Acquire,
                    ) {
                        Ok(_) => break,
                        Err(cur) => prev = cur,
                    }
                }
                if let Some(limit) = mem_limit {
                    if rss >= limit && !mem_abort.load(AtomicOrdering::Acquire) {
                        tracing::warn!(
                            "reindex memory poller: rss={}MB >= limit={}MB \
                             — tripping abort flag for index {}",
                            rss,
                            limit,
                            index_id,
                        );
                        mem_abort.store(true, AtomicOrdering::Release);
                    }
                }
            }
            ticker.tick().await;
        }
    });
    (handle, stop)
}

/// Spawn a background poller that tracks the peak RSS of the embedderd sidecar
/// during a reindex run (issue #282).
///
/// Why: the daemon's own RSS poller covers only the daemon parent process. The
/// embedderd sidecar process owns the ONNX arena and routinely uses 2–3 GB
/// more than the daemon during active embedding; omitting it leaves operators
/// with an incomplete picture for capacity planning and regression testing.
/// What: reads the current sidecar PID from `embedderd_pid_slot` on each tick.
/// A PID of 0 (no sidecar, or sidecar exited mid-run) causes the sample to be
/// skipped gracefully. Stops when `stop` is set to `true` by the orchestrator.
/// Test: `embedderd_peak_rss_captured_on_complete` (marked `#[ignore]`).
fn spawn_embedderd_rss_poller(
    embedderd_pid_slot: Arc<AtomicU32>,
    peak_embedderd_rss: Arc<AtomicU64>,
) -> (tokio::task::JoinHandle<()>, Arc<AtomicBool>) {
    /// Polling cadence for the embedderd RSS sampler.
    const EMBEDDERD_POLL_INTERVAL: Duration = Duration::from_millis(500);

    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = stop.clone();
    let handle = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(EMBEDDERD_POLL_INTERVAL);
        ticker.tick().await;
        loop {
            if stop_clone.load(AtomicOrdering::Acquire) {
                break;
            }
            let pid = embedderd_pid_slot.load(AtomicOrdering::Acquire);
            if let Some(rss) = current_rss_mb_for_pid(pid) {
                // Monotonic peak update (same CAS loop as the main poller).
                let mut prev = peak_embedderd_rss.load(AtomicOrdering::Acquire);
                while rss > prev {
                    match peak_embedderd_rss.compare_exchange_weak(
                        prev,
                        rss,
                        AtomicOrdering::AcqRel,
                        AtomicOrdering::Acquire,
                    ) {
                        Ok(_) => break,
                        Err(cur) => prev = cur,
                    }
                }
            }
            ticker.tick().await;
        }
    });
    (handle, stop)
}

/// Variant of `spawn_reindex` that GC's the progress map after completion
/// and supports background/interactive prioritisation (issue #458).
///
/// Why: issue #458 — startup auto-discover can queue 40+ reindex tasks, all
/// competing for the same semaphore and starving user-initiated requests. The
/// `priority` flag routes the task to one of two separate semaphores:
///
///   - `priority=true`  → `reindex_semaphore()` (2 permits, interactive path)
///   - `priority=false` → `background_reindex_semaphore()` (1 permit, bulk path)
///
/// `embedderd_pid_slot` — when `Some`, the orchestrator spawns a concurrent
/// RSS poller for the embedderd sidecar (issue #282).
///
/// What: spawns a `tokio::task` that acquires the appropriate semaphore permit,
/// runs the three reindex phases, emits the terminal SSE event, and GC's the
/// progress entry.
/// Test: `interactive_reindex_not_starved_by_background` verifies that a
/// background task holding the background semaphore does not block a concurrent
/// interactive request.
#[allow(clippy::too_many_arguments)]
pub fn spawn_reindex_with_cleanup(
    handle: Arc<IndexHandle>,
    progress: Arc<ReindexProgress>,
    force: bool,
    cleanup_map: Option<Arc<DashMap<IndexId, Arc<ReindexProgress>>>>,
    aborted_map: Option<Arc<DashMap<IndexId, Instant>>>,
    embedderd_pid_slot: Option<Arc<AtomicU32>>,
    priority: bool,
    quarantine: Option<ReindexQuarantine>,
) {
    use std::sync::atomic::Ordering as AtomicOrd;
    // Track background queue depth so /health can expose it.
    if !priority {
        BACKGROUND_QUEUE_DEPTH.fetch_add(1, AtomicOrd::Relaxed);
    }
    let cleanup_id = handle.id.clone();
    tokio::spawn(async move {
        use std::sync::atomic::Ordering;

        // Issue #458: route to the correct semaphore based on priority.
        let _permit = reindex_semaphore_for(priority)
            .acquire()
            .await
            .expect("reindex semaphore is never closed");
        // Decrement the background queue counter once the permit is held.
        if !priority {
            BACKGROUND_QUEUE_DEPTH.fetch_sub(1, AtomicOrd::Relaxed);
        }

        // Arm the termination guard. Any early exit — panic, early return, or
        // `.await` cancellation — fires `ReindexTerminationGuard::drop`, which
        // broadcasts an error event and marks the status `Failed`. The guard is
        // disarmed just after `emit_complete_event` confirms the normal terminal
        // event has been sent.
        let mut term_guard = ReindexTerminationGuard::new(Arc::clone(&progress));

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
        let walk = collect_files_to_index(&handle);
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
        let hashes = hashes_for(&index_id);
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
        let is_colocated =
            crate::service::colocated_storage::has_colocated_storage(&canonical_root);
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
                        term_guard.disarm();
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
        let (poller_handle, poller_stop) = spawn_memory_poller(
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
                let (h, s) = spawn_embedderd_rss_poller(
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
            total_vector_upsert_ms =
                total_vector_upsert_ms.saturating_add(outcome.vector_upsert_ms);
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

        let memory_aborted = mem_limit_hit || mem_abort.load(AtomicOrdering::Acquire);

        // Issue #848 — prune pass: remove stale chunks from files deleted on disk.
        if corpus_swap_tmp.is_some() && !force && !memory_aborted {
            super::prune::prune_deleted_files_from_staging(
                &handle,
                &walk.files,
                &canonical_root,
                &hashes,
                &index_id,
            )
            .await;
        }

        let embedder_present = handle.indexer.read().await.has_embedder();
        let reindex_outcome = validate::reindex_outcome(
            handle.lexical_only,
            ctx.defer_embed,
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
        if let Some(tmp_path) = &corpus_swap_tmp {
            if staging_resolution.is_commit() {
                commit_staged_corpus_swap(&handle, &index_id, tmp_path).await;
                if let Err(e) = handle.write_indexed_root(&canonical_root).await {
                    tracing::warn!(
                        "reindex[{}]: failed to persist indexed_root {} ({e}) — \
                         a future root-move may not re-relativize paths",
                        index_id.0,
                        canonical_root.display(),
                    );
                }
            } else {
                if let staging::StagingResolution::Rollback { reason } = &staging_resolution {
                    tracing::warn!(
                        "reindex[{}]: rolling back staged corpus — {reason}",
                        index_id.0,
                    );
                }
                abort_staged_corpus_swap(&handle, &index_id, tmp_path).await;
            }
        } else if reindex_outcome.is_ready() && !memory_aborted {
            if let Err(e) = handle.write_indexed_root(&canonical_root).await {
                tracing::debug!(
                    "reindex[{}]: indexed_root not persisted (no durable corpus): {e}",
                    index_id.0,
                );
            }
        }

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
            poller_stop.store(true, AtomicOrdering::Release);
            let _ = poller_handle.await;
            if let Some(stop) = embedderd_poller_stop {
                stop.store(true, AtomicOrdering::Release);
            }
            if let Some(h) = embedderd_poller_handle {
                let _ = h.await;
            }
            if let Some(ref q) = quarantine {
                q.record_failure(&index_id);
            }
            schedule_progress_cleanup(cleanup_map, cleanup_id);
            return;
        }

        // Issue #109, Phase 1: flip the semantic stage to `Ready`.
        mark_semantic_ready_graph_in_progress(
            &handle,
            total_vector_count,
            progress.total_chunks.load(AtomicOrdering::Acquire),
        )
        .await;

        // Phase 3: rebuild the symbol graph.
        let kg = if handle.skip_kg {
            tracing::info!(
                "reindex[{}]: KG construction skipped (skip_kg=true)",
                index_id.0,
            );
            KgRebuildOutcome {
                symbol_count: 0,
                edge_count: 0,
                kg_ms: 0,
                kg_skipped: true,
            }
        } else {
            // Emit `kg_start` so the CLI activates the KG progress bar (issue #401).
            progress
                .push(serde_json::json!({
                    "event": "kg_start",
                    "index_id": index_id.0,
                }))
                .await;

            let outcome = rebuild_symbol_graph_for_reindex(&handle).await;

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

            mark_graph_ready(&handle).await;
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
        };

        // Stop the background pollers.
        poller_stop.store(true, AtomicOrdering::Release);
        let _ = poller_handle.await;

        if let Some(stop) = embedderd_poller_stop {
            stop.store(true, AtomicOrdering::Release);
        }
        if let Some(h) = embedderd_poller_handle {
            let _ = h.await;
        }
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
        tracing::info!(
            "reindex phase timings: index={} walk={}ms parse={}ms \
             model_load_approx={}ms embed={}ms bm25={}ms vector_upsert={}ms \
             kg={}ms total={}ms",
            index_id.0,
            walk_ms,
            total_parse_ms,
            model_load_approx_ms,
            total_embed_ms,
            total_bm25_ms,
            total_vector_upsert_ms,
            kg.kg_ms,
            elapsed_ms,
        );

        let totals = RunTotals {
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
            &totals,
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
        let has_embedder = handle.indexer.read().await.has_embedder();
        if ctx.defer_embed && !aborted_memory && has_embedder {
            spawn_deferred_embed_pass(handle, progress.clone());
        }

        // Issue #75: GC the progress entry after a short delay.
        schedule_progress_cleanup(cleanup_map, cleanup_id);
    });
}
