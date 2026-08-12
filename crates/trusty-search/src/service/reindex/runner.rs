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
use super::checkpoint;
use super::corpus_swap::begin_staged_corpus_swap;
use super::finish::{BatchTotals, FileHashes, FinishCtx};
use super::guard::ReindexTerminationGuard;
use super::hash::hashes_for;
use super::hash_cache;
use super::hnsw_swap::begin_staged_hnsw_swap;
use super::progress::{ReindexProgress, ReindexStatus};
use super::quarantine::ReindexQuarantine;
use super::root_gate;
use super::semaphore::{
    acquire_index_teardown_read, index_cancel_flag, index_semaphore, reindex_semaphore_for,
    BACKGROUND_QUEUE_DEPTH,
};
use super::stage_timings::StageTimings;
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
    // Issue #2984 Phase 1 CRITICAL finding 2: also acquire this index's
    // per-index mutual-exclusion permit, held for the FULL reindex duration
    // (dropped when this function returns). This is the same semaphore the
    // `PATCH /indexes/:id/config` component-toggle handler `try_acquire`s, so
    // a reindex and a component catch-up on the SAME index can never race
    // each other regardless of which reindex priority track is in play —
    // closing the gap where an interactive reindex (a separate 2-permit
    // semaphore never touched by the old guard) could still race a catch-up.
    let _index_permit = index_semaphore(&handle.id).acquire_owned().await.expect(
        "per-index semaphore is never closed — it is a fresh Semaphore per IndexId, never dropped",
    );
    // #3049: fetched AFTER the permit so a delete that already completed cannot
    // leave a stale `true` here — that delete evicted the flag, so this call
    // allocates a fresh `false` one. Polled at the producer/consumer batch
    // boundaries below.
    let cancel = index_cancel_flag(&handle.id);
    // #3049: the reindex is the longest-running writer — hold the teardown
    // lock's shared side for its whole duration. The cancel flag above is what
    // keeps a waiting DELETE bounded to one batch rather than one corpus.
    let _teardown_guard = acquire_index_teardown_read(&handle.id).await;

    // Arm the termination guard. Any early exit — panic, early return, or
    // `.await` cancellation — fires `ReindexTerminationGuard::drop`, which logs
    // the cause at `error!` (issue #1428: never silent), broadcasts an error
    // event, and marks the status `Failed`. The guard is disarmed just after
    // `emit_complete_event` confirms the normal terminal event has been sent.
    // Stamping the index id makes the stderr line greppable per-index; the
    // failure-reason slot lets us hand a specific cause (e.g. a captured
    // producer-task panic) to `Drop`.
    // `mut` so the #4356 budget-refusal path can `disarm()` after emitting its
    // own terminal frame; every other early return still relies on `Drop`.
    let mut term_guard =
        ReindexTerminationGuard::new(Arc::clone(&progress)).with_index_id(handle.id.0.clone());
    let failure_slot = term_guard.failure_reason_slot();

    let started = Instant::now();
    // Issue #602 — portable paths.
    let root = handle.root_path.clone();
    let canonical_root = validate::canonical_walk_root(&root);
    let index_id: IndexId = handle.id.clone();

    // Issue #602/#1073 + #2178 (P0 data-risk): detect a root move against the
    // corpus's own persisted `indexed_root` (the redb `_meta` value, i.e. the
    // root the CURRENT corpus's chunk paths are actually relative to), and
    // refuse to walk — and, later, prune — against a candidate the durably
    // persisted `indexes.toml` entry contradicts. The gate runs BEFORE Phase 1,
    // so a refused candidate never reaches the filesystem walk or the
    // finish-phase prune pass and the existing corpus is left untouched.
    //
    // #5357: the decision itself lives in `root_gate` so this path and
    // `reindex_handler`'s request-time mirror of it can never drift, and so a
    // FAILED read of either input refuses instead of degrading to "trusted".
    let gate = root_gate::evaluate_root_move(
        &index_id.0,
        handle.read_indexed_root().await,
        &canonical_root,
        crate::service::persistence::load_index_registry,
    );
    let (root_moved, prior_indexed_root) = match gate {
        Ok(trusted) => {
            if trusted.moved {
                // #5357: the indexer moves HERE, off one read of the registry,
                // rather than at request time where a later refusal could strand
                // it on a root the corpus never matched.
                root_gate::sync_indexer_root_after_trusted_move(&handle).await;
            }
            (trusted.moved, trusted.indexed_root)
        }
        Err(refusal) => {
            tracing::warn!("reindex[{}]: {}", index_id.0, refusal.reason);
            // #5357: the handler may already have pointed the indexer at the
            // refused root; leave it there and every chunk resolves against a
            // tree the corpus was never relativized against.
            root_gate::restore_indexer_root_after_refusal(&handle, &refusal).await;
            mark_reindex_failed(&handle, &refusal.reason).await;
            progress.status.store(ReindexStatus::Failed);
            progress
                .push(serde_json::json!({
                    "event": "error",
                    "index_id": index_id.0,
                    "message": refusal.reason,
                    "fatal": true,
                }))
                .await;
            // term_guard drops here → broadcasts error event via Drop (mirrors
            // the carryover-copy-failure abort path below).
            drop(term_guard);
            schedule_progress_cleanup(cleanup_map, cleanup_id);
            if let Some(ref q) = quarantine {
                q.record_failure(&index_id);
            }
            return;
        }
    };

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
    //
    // #4356 (review): the walk and the budget check both run `std::fs::metadata`
    // over the same paths — `walker::should_skip_path`'s size guard stats every
    // candidate, then `IndexBudget::check` sums the survivors. On a network
    // mount a stalled `stat()` blocks for as long as the kernel takes, so
    // neither belongs on a tokio worker thread; `spawn_blocking` puts both on
    // the blocking pool, where a stall costs a pool thread and the runtime keeps
    // scheduling. One hop covers both because they are adjacent and both purely
    // synchronous.
    //
    // No wall-clock deadline, unlike `warm_boot::probe`: that one only needs a
    // yes/no about a volume, so it can abandon a frozen thread and answer
    // "inaccessible". This path needs the file list itself — there is nothing to
    // return on a timeout, and refusing on one would turn a slow mount into a
    // failed reindex.
    let walk_task = {
        let handle = Arc::clone(&handle);
        tokio::task::spawn_blocking(move || {
            let walk = super::orchestrator::collect_files_to_index(&handle);
            // #4356: refuse a tree too large to index completely, rather than
            // letting `TRUSTY_MAX_CHUNKS` truncate it into a corpus that
            // reports success. Checked on the POST-FILTER list, so narrowing
            // the index (`exclude_globs`, `extra_skip_dirs`, `include_paths`,
            // `extensions`) is what clears it; the env ceilings are the blunt
            // override.
            let budget = crate::service::index_budget::IndexBudget::from_env().check(&walk.files);
            (walk, budget)
        })
    };
    let (walk, budget) = match walk_task.await {
        Ok(pair) => pair,
        Err(join_err) => {
            // Before the walk moved onto the blocking pool a panic in it unwound
            // `run_reindex` directly and fired `ReindexTerminationGuard::drop`.
            // Re-raise so that stays exactly true.
            if join_err.is_panic() {
                std::panic::resume_unwind(join_err.into_panic());
            }
            // Runtime shutdown. Returning drops `term_guard`, which emits the
            // terminal frame — the same path a cancelled `.await` took before.
            return;
        }
    };
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

    // #4356: act on the budget verdict computed alongside the walk above. This
    // is the last point where nothing has been committed — no staging corpus is
    // open, no chunk is written — so a refusal leaves the existing index
    // byte-identical.
    if let Err(over) = budget {
        let reason = over.to_string();
        tracing::warn!("reindex[{}]: {}", index_id.0, reason);
        handle.walk_diagnostics.write().await.last_walk_error = Some(reason.clone());
        // NOT `mark_reindex_failed`: that one leaves `lexical` alone because
        // the BM25 lane was genuinely built. Here nothing was, and leaving it
        // at the `InProgress` that `reset_stages_for_reindex` just set would
        // strand the index mid-walk with no reindex in flight.
        super::stages::mark_reindex_failed_before_lexical(&handle, &reason).await;
        progress.status.store(ReindexStatus::Failed);
        progress
            .push(serde_json::json!({
                "event": "error",
                "index_id": index_id.0,
                "message": reason,
                "fatal": true,
            }))
            .await;
        // #4356 (review): the frame above is this run's ONE terminal event, and
        // `push` also wrote it to the replay buffer, which `Drop` cannot reach.
        // Leaving the guard armed broadcasts a SECOND `fatal` frame reading
        // "exited unexpectedly (panic or cancellation)" — the CLI prints every
        // error frame it receives (`commands::reindex_engine::events`), so an
        // operator who narrowed their index correctly would be sent hunting a
        // panic backtrace for a refusal working as designed.
        term_guard.disarm();
        drop(term_guard);
        schedule_progress_cleanup(cleanup_map, cleanup_id);
        if let Some(ref q) = quarantine {
            q.record_failure(&index_id);
        }
        return;
    }

    progress.total_files.store(total, Ordering::Release);

    // Issue #3979: before touching the hash cache, decide whether an
    // interrupted run left an adoptable staging corpus. This has to happen
    // here — ahead of the hash-cache branch below — because resuming changes
    // WHERE the done-set comes from: the staged corpus rather than the live
    // one. Gated on `has_corpus_store()`, which by the #4122 load-bearing
    // invariant (`corpus_open_failed ⇒ corpus == None`) also guarantees a
    // write-quarantined index never reaches this path.
    //
    // A `force` run neither writes nor consumes a checkpoint: it stages an
    // EMPTY corpus and skips the prune pass, so a resumed force run could keep
    // chunks for files deleted between the crash and the resume — drift a
    // clean force run would not have. See `checkpoint`'s module docs.
    let has_durable_corpus = handle.indexer.read().await.has_corpus_store();
    let checkpoint_for_run = (!force && staging::should_stage(has_durable_corpus))
        .then(|| checkpoint::ReindexCheckpoint::for_run(&handle, &index_id, &canonical_root));
    let resume = match checkpoint_for_run.as_ref() {
        Some(cp) => checkpoint::probe_resume(&handle, &index_id, &canonical_root, cp).await,
        None => None,
    };
    // #4721: `resume` now OWNS the probe's open staging-corpus handle and is
    // moved into `begin_staged_corpus_swap` below, so capture the two facts the
    // later stages need before it goes.
    let resumed = resume.is_some();
    let resumed_chunks = resume.as_ref().map(|r| r.staged_chunks).unwrap_or(0);

    // Issues #840 / #662: load the persisted hash cache BEFORE emitting
    // the `start` event so `hashes_loaded` is available for the event payload.
    let hashes: FileHashes = hashes_for(&index_id);
    // Issue #602: `root_moved` (computed above, before the walk, and gated by
    // the #2178 trust check) decides, for legacy (non-colocated) indexes,
    // whether to clear the hash cache so every file is re-written relative to
    // the new canonical root.
    //
    // Issue #1073: for colocated indexes the chunk and hash keys are
    // ROOT-RELATIVE (#402), so they are valid at any root location — a pure
    // move changes the root prefix only. Do NOT clear the hash cache on a
    // root move for colocated indexes.
    let is_colocated = crate::service::colocated_storage::has_colocated_storage(&canonical_root);
    // #5024: the hash-cache load is a full redb table scan on a warm index —
    // measure it rather than leaving it in the residual.
    let mut stage_timings = StageTimings::default();
    let hash_cache_started = Instant::now();
    let hashes_loaded: usize = if let Some(state) = resume.as_ref() {
        // #3979: on a resume the STAGED corpus is the authority for what is
        // already done, so the cache is replaced by its hash table rather than
        // merged with the live one. A live-cache entry with no matching rows in
        // the staging corpus would let the batch loop skip a file whose chunks
        // are absent from the corpus about to be promoted.
        checkpoint::seed_hash_cache_from_staging(&hashes, &state.staged_hashes)
    } else if force {
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
    stage_timings.hash_cache_ms = hash_cache_started.elapsed().as_millis() as u64;

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
            // #3979: tell the CLI/operator this run is continuing an
            // interrupted one, and how much work it inherited.
            "resumed": resumed,
            "resumed_chunks": resumed_chunks,
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
    // #5024: this is where an incremental run copies every live-corpus row into
    // the fresh staging store — the single largest non-embed cost on a warm
    // reindex, and the stage a corpus-reuse scheme would have to beat.
    let carryover_started = Instant::now();
    let corpus_swap_tmp: Option<PathBuf> =
        if staging::should_stage(handle.indexer.read().await.has_corpus_store()) {
            match begin_staged_corpus_swap(
                &handle,
                &index_id,
                force,
                // #4721: moved, not borrowed — adoption takes ownership of the
                // probe's open staging handle.
                resume,
                checkpoint_for_run.as_ref(),
            )
            .await
            {
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
            // #4721: staging was skipped after all — release the probe's open
            // staging handle now rather than holding the redb file for the rest
            // of the run.
            drop(resume);
            None
        };
    stage_timings.carryover_ms = carryover_started.elapsed().as_millis() as u64;

    // Issue #3970: stage the periodic HNSW snapshot too, mirroring the redb
    // corpus staging above. Unlike the corpus (staged only when a durable
    // corpus store exists), this is unconditional — cheap (path resolution +
    // one atomic flag flip) and safe even for a BM25-only indexer with no
    // vector store, where the periodic persister's HNSW save is already a
    // harmless no-op. A `None` here (path unresolvable) is a degraded-but-
    // not-worse fallback: `spawn_incremental_persist` never learns
    // `reindexing` was set, so it keeps writing straight to the live path
    // exactly as it did before this fix.
    let hnsw_swap_paths = begin_staged_hnsw_swap(&handle, &index_id).await;

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
        skip_vector: handle.skip_vector,
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
    // #5024: wall time of the whole pipeline, so the gap against the summed
    // subsystem accumulators exposes embedder warm-up and channel stalls.
    let pipeline_started = Instant::now();
    let producer_ctx = ctx.clone();
    let producer_mem_abort = mem_abort.clone();
    let producer_index_id = index_id.0.clone();
    let total_batches = batches.len();
    let producer_cancel = cancel.clone();
    let producer = tokio::spawn(async move {
        for (batch_idx, batch) in batches.into_iter().enumerate() {
            // #3049: a DELETE for this index sets the cancel flag and then waits
            // on the permit this reindex holds. Stopping at the batch boundary
            // bounds that wait to one batch instead of the whole corpus.
            if producer_cancel.load(AtomicOrdering::Acquire) {
                tracing::warn!(
                    "reindex: index {} was deleted mid-run — producer halting at batch \
                     {}/{} (issue #3049)",
                    producer_index_id,
                    batch_idx,
                    total_batches,
                );
                break;
            }
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
            // RUST_LOG=debug visibility into batch flush cadence — pinpoints the
            // exact batch index where a deterministic mid-run halt occurs
            // (issue #1428). Cheap: a single debug-gated log per batch.
            tracing::debug!(
                index_id = %producer_index_id,
                batch_idx,
                total_batches,
                batch_files = batch.len(),
                "reindex: producer preparing batch"
            );
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
        // #3049: same checkpoint on the commit side. Dropping the batch rather
        // than committing it means the delete's `remove_dir_all` never races a
        // redb/HNSW write that started after the cancel was signalled.
        if cancel.load(Ordering::Acquire) {
            tracing::warn!(
                "reindex: index {} was deleted mid-run — consumer discarding the \
                 in-flight batch and halting (issue #3049)",
                index_id.0,
            );
            rx.close();
            while rx.recv().await.is_some() {}
            break;
        }
        tracing::debug!(
            index_id = %index_id.0,
            indexed = progress.indexed_count(),
            "reindex: consumer committing batch"
        );
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
    // Issue #1428: the producer's `JoinHandle` result was previously discarded
    // (`let _ = producer.await`), so a PANIC inside the parse/embed producer
    // task (e.g. an `unwrap` deep in the embed path, or an allocation failure
    // under GPU/memory pressure) unwound silently — the consumer loop just saw
    // the channel close and fell through to a "successful" finish. Capture the
    // `JoinError` here: log it at `error!` (stderr) and record it in the guard's
    // failure-reason slot so that if the run ends up failing, the operator sees
    // the real cause rather than the generic "exited unexpectedly" message.
    //
    // Limitation: this handles a panic/cancellation *inside* the spawned producer
    // task (surfaced as a `JoinError`). It does NOT cover `producer.await` itself
    // panicking (e.g. the tokio runtime shutting down out from under the await):
    // in that case the failure_slot is never written and the termination guard's
    // `Drop` falls back to its generic "exited unexpectedly" message. That path
    // is still non-silent (the guard logs at `error!`), just less specific.
    //
    // #1451: recording the cause in the guard's slot only covers the paths that
    // exit through `Drop`. The normal path disarms the guard, so a producer
    // panic whose counters looked healthy reached `emit_complete_event` and the
    // run reported `complete`. The reason is now also returned to `finish` as a
    // value, where it overrides the counter verdict.
    let mut producer_failure: Option<String> = None;
    if let Err(join_err) = producer.await {
        // Index id lives in the `reindex[{}]:` message prefix only (no
        // duplicate structured `index_id` field) to avoid double emission
        // in JSON log backends; `reindex[...]: ... PANICKED` greps still
        // match (issue #1428 review follow-up).
        let reason = if join_err.is_panic() {
            tracing::error!(
                "reindex[{}]: parse/embed producer task PANICKED — the reindex \
                 is incomplete; this usually indicates an embedder fault (e.g. \
                 GPU OOM / sidecar stall). JoinError: {join_err}",
                index_id.0,
            );
            format!(
                "parse/embed producer task panicked ({join_err}) — reindex \
                 incomplete; check the daemon log for the panic backtrace \
                 and the embedder (GPU OOM / sidecar stall is the common cause)"
            )
        } else {
            // Cancellation (e.g. runtime shutdown) — still non-silent.
            tracing::error!(
                "reindex[{}]: parse/embed producer task was cancelled — reindex \
                 incomplete. JoinError: {join_err}",
                index_id.0,
            );
            format!("parse/embed producer task cancelled ({join_err}) — reindex incomplete")
        };
        ReindexTerminationGuard::set_failure_reason(&failure_slot, reason.clone());
        producer_failure = Some(reason);
    }

    stage_timings.pipeline_ms = pipeline_started.elapsed().as_millis() as u64;

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
        hnsw_swap_paths,
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
        // #3979: a resumed run inherits chunks whose vectors only ever reached
        // the staged HNSW snapshot, so `finish` must run a vector catch-up.
        resumed,
        // #1451: a panicked/cancelled producer means the pipeline never ran to
        // the end of the file list, whatever the counters say.
        producer_failure,
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
    super::finish::finish_reindex(finish_ctx, batch_totals, stage_timings).await;
}
