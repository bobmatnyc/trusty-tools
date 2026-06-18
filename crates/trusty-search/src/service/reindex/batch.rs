//! Per-batch reindex pipeline: read → filter → parse/embed → commit.
//!
//! Why: the reindex orchestrator processes files in fixed-size batches to bound
//! peak memory. Each batch goes through two sequential stages (separated so
//! stage N+1's parse can overlap with stage N's commit):
//!   1. `prepare_and_parse_batch` — file reads + hash-skip + parse/embed
//!      (NO write lock; safe to run concurrently with a commit).
//!   2. `commit_parsed_and_finalize` — write-locked commit to HNSW/BM25/redb
//!      + counter updates + memory check.
//!
//! What: `BatchCtx` (shared per-run context), `BatchPayload`, `ParsedReadyBatch`,
//! `BatchOutcome`, and the four stage helpers plus two SSE event emitters.
//!
//! Test: batch helpers covered by `reindex_walks_directory_and_emits_events`
//! and the stall/memory-abort tests.

use crate::core::indexer::{CommitTimings, ParsedBatch};
use crate::core::registry::{IndexHandle, IndexId};
use crate::service::walker::should_skip_content;
use dashmap::DashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering as AtomicOrdering};
use std::sync::Arc;
use std::time::Instant;

use super::hash::{hash_content, shrink_hashes_if_needed, MAX_FILE_HASHES_PER_INDEX};
use super::hash_cache;
use super::progress::ReindexProgress;
use super::prune::to_corpus_relative_path;

/// Files per parallel batch.
///
/// Why: 128 files bounds peak memory during reindex. On a 595-file repo with ~8
/// chunks/file, a 512-file batch held ~4k chunks of source content plus their
/// 384-dim f32 embeddings plus ONNX intermediate activation tensors retained
/// across the embed loop — pushing RSS to 33–50 GB and triggering macOS Jetsam
/// kill. With 128 files per batch, the working set caps at ~1k chunks worth of
/// memory, and the ONNX session arena gets multiple opportunities to release
/// transient buffers between commits. SSE progress events fire per batch, so a
/// smaller batch size also gives more granular progress updates.
/// What: constant consumed by the orchestrator's batch-split logic.
/// Test: indirectly covered — large-project reindexes complete without OOM.
pub(super) const REINDEX_BATCH_SIZE: usize = 128;

/// Process-global flag: has the in-process (non-sidecar) embedder completed
/// at least one batch successfully?
///
/// Why (issue #827): the `needs_embedder_init` heuristic fires on
/// `first_batch_ever` (indexed == 0) for in-process embed mode, which is
/// correct for the very first reindex after daemon start (model is loading).
/// But it incorrectly fires on every subsequent reindex's first batch too,
/// causing the CLI to flash "Loading model…" on a daemon whose model is
/// already warm. Checking this flag lets us suppress the init events on
/// warm daemons.
///
/// What: flipped from `false` → `true` once after the first in-process
/// `embedder_ready` event is emitted. Set on the first successful
/// `parse_and_embed_files` call in in-process mode.
///
/// Test: `needs_embedder_init` unit tests in this module verify the flag
/// suppresses re-emission on the second reindex. Use
/// `reset_inprocess_embedder_flag_for_tests` in test code to isolate tests
/// from each other (issue #1179).
pub(super) static INPROCESS_EMBEDDER_EVER_READY: AtomicBool = AtomicBool::new(false);

/// Reset `INPROCESS_EMBEDDER_EVER_READY` to `false` for test isolation.
///
/// Why (issue #1179): `INPROCESS_EMBEDDER_EVER_READY` is a process-global
/// `AtomicBool`. Without an explicit reset, once any test in the binary sets
/// it to `true`, every subsequent test that checks it sees `true` regardless
/// of construction order, making tests order-dependent and non-deterministic.
/// What: stores `false` under `SeqCst` so all later loads in the same test see
/// a clean initial state identical to a freshly-started daemon.
/// Test: called at the top of each unit test that exercises this flag so tests
/// are isolated regardless of execution order.
#[cfg(test)]
pub(crate) fn reset_inprocess_embedder_flag_for_tests() {
    INPROCESS_EMBEDDER_EVER_READY.store(false, AtomicOrdering::SeqCst);
}

/// Read `INPROCESS_EMBEDDER_EVER_READY` for test assertions.
///
/// Why (issue #1179): the static is `pub(super)` scoped to the `reindex`
/// module. These test-only accessors are `pub(crate)` so that `mod.rs` can
/// re-export them as `pub(crate)` for `tests.rs` to use without widening the
/// production static itself.
/// What: loads the current value with `SeqCst` ordering and returns it.
/// Test: used in `inprocess_embedder_flag_reset_restores_false` and
/// `inprocess_embedder_flag_isolated_across_scenarios`.
#[cfg(test)]
pub(crate) fn inprocess_embedder_ever_ready_for_tests() -> bool {
    INPROCESS_EMBEDDER_EVER_READY.load(AtomicOrdering::SeqCst)
}

/// Shared context threaded into `process_one_batch` so the per-batch helper
/// doesn't take a dozen arguments. Cheap to clone (everything is `Arc` /
/// `PathBuf` / scalar).
///
/// Why: extracted from `spawn_reindex_with_cleanup` (issue #98) so the
/// orchestrator's per-batch body is testable and bounded in size.
/// What: holds references to the index handle, progress tracker, memory
/// abort flag, hash cache, and scalar run parameters.
/// Test: `BatchCtx` is constructed directly in batch unit tests.
#[derive(Clone)]
pub(super) struct BatchCtx {
    pub handle: Arc<IndexHandle>,
    pub progress: Arc<ReindexProgress>,
    pub root: PathBuf,
    pub index_id: IndexId,
    pub hashes: Arc<DashMap<PathBuf, String>>,
    pub mem_limit: Option<u64>,
    pub mem_abort: Arc<AtomicBool>,
    pub peak_rss_atomic: Arc<AtomicU64>,
    pub started: Instant,
    pub total: usize,
    /// Issue #109, Phase 1: skip the embed step entirely when the index
    /// was created with `lexical_only: true`.
    pub lexical_only: bool,
    /// Issue #923: skip embedding during the fast pass when `defer_embed=true`.
    pub defer_embed: bool,
    /// PID slot for the trusty-embedderd sidecar (issue #315 lazy-spawn).
    ///
    /// Why: `LazyEmbedderHandle` defers spawning `trusty-embedderd` until
    /// the first embed request. The subprocess spawn + ONNX model load
    /// takes 30–60 s; during this time the progress UI was completely
    /// frozen. Emitting `embedder_init` just before the first embed call and
    /// `embedder_ready` when the sidecar responds allows the CLI to
    /// transition the header to "Loading model…".
    ///
    /// `None` when no PID slot is available (non-sidecar embed mode).
    pub embedder_pid_slot: Option<Arc<AtomicU32>>,
}

/// What a single batch contributed to the run-level totals.
///
/// Why: the orchestrator folds each `BatchOutcome` into its accumulators and
/// breaks the batch loop when `mem_limit_hit` is set.
/// What: per-batch timing counters plus two boolean sentinel fields.
/// Test: verified by inspecting the `complete` SSE event's timing fields.
#[derive(Default)]
pub(super) struct BatchOutcome {
    pub parse_ms: u64,
    pub embed_ms: u64,
    pub bm25_ms: u64,
    pub vector_upsert_ms: u64,
    pub vector_count: usize,
    /// True when the post-commit RSS check tripped the abort flag — caller
    /// must break out of the batch loop.
    pub mem_limit_hit: bool,
    /// Issue #100: chunks dropped by the `TRUSTY_MAX_CHUNKS` cap in this
    /// batch.
    pub chunks_dropped_by_cap: usize,
}

/// One in-flight batch ready for the commit (write-lock) stage.
///
/// Why: pipelining batch N+1's read+parse with batch N's commit (issue #20)
/// requires shipping a self-contained unit between tasks. `ParsedBatch` owns
/// its chunks/embeddings (no borrows), so this struct can be sent across an
/// mpsc channel freely.
/// What: owns the `ParsedBatch` plus the bookkeeping needed by the commit phase.
/// Test: verified end-to-end by the pipelined reindex integration tests.
pub(super) struct ParsedReadyBatch {
    pub parsed: ParsedBatch,
    pub new_hashes: Vec<(PathBuf, String)>,
    /// Files actually submitted to the indexer (post hash/minified filtering).
    pub batch_files: usize,
    /// Corpus-relative paths of every file being re-indexed in this batch
    /// (i.e. the files that were NOT hash-skipped). Used by
    /// `commit_parsed_and_finalize` to remove stale chunks for changed files
    /// BEFORE inserting the new chunks (fix for issue #855: delete-then-insert
    /// semantics to prevent orphan chunk IDs when a file shrinks).
    pub changed_corpus_paths: Vec<String>,
}

/// Sanitised contents of one batch after read + filter passes.
///
/// Why: separating the payload from the batch slice keeps `prepare_batch_payload`
/// pure and makes it testable without driving the full parse/embed step.
/// What: carries the filtered file list plus auxiliary bookkeeping slices.
/// Test: `prepare_and_parse_batch` unit tests verify the payload fields.
pub(super) struct BatchPayload {
    pub to_index: Vec<(String, String)>,
    pub to_index_paths: Vec<PathBuf>,
    pub new_hashes: Vec<(PathBuf, String)>,
    /// Corpus-relative paths of files being re-indexed (same strings as the
    /// first element of each `to_index` entry).
    pub changed_corpus_paths: Vec<String>,
}

/// Process a single batch end-to-end (sequential; not pipelined).
///
/// Why: thin wrapper for callers that don't need the producer/consumer pipeline.
/// Retained as `#[allow(dead_code)]` for tests and non-pipelined callers that
/// prefer a simpler one-shot API.
/// What: delegates to `prepare_and_parse_batch` + `commit_parsed_and_finalize`.
/// Test: covered indirectly by integration tests via `reindex_handler`.
#[allow(dead_code)]
pub(super) async fn process_one_batch(ctx: &BatchCtx, batch: &[PathBuf]) -> BatchOutcome {
    let Some(parsed) = prepare_and_parse_batch(ctx, batch).await else {
        return BatchOutcome::default();
    };
    commit_parsed_and_finalize(ctx, parsed).await
}

/// Stage 1 of the pipelined per-batch flow: read every file in `batch`,
/// filter out hash-matches/minified/errors, then parse + embed under the
/// indexer's READ lock.
///
/// Why: split from the commit stage so the producer task can race ahead of
/// the consumer's write-lock work. This is the half that does NOT take the
/// indexer write lock, so multiple invocations are safe to overlap with an
/// in-progress commit on the same handle.
/// What: returns `None` when no files in the batch needed indexing (caller
/// skips the commit). Otherwise returns `ParsedReadyBatch` with the parsed
/// chunks and bookkeeping metadata.
/// Test: covered by `reindex_walks_directory_and_emits_events`.
pub(super) async fn prepare_and_parse_batch(
    ctx: &BatchCtx,
    batch: &[PathBuf],
) -> Option<ParsedReadyBatch> {
    let payload = prepare_batch_payload(ctx, batch).await;
    if payload.to_index.is_empty() {
        return None;
    }
    let batch_files = payload.to_index.len();
    let to_index = payload.to_index;

    // Problem 1 UX fix: detect whether the embedder (sidecar OR in-process)
    // is about to be used for the first time (cold-start model load, 30-60 s).
    //
    // Sidecar path: The PID slot reads `0` before the first lazy spawn. If we
    // see `0` on a non-lexical-only, non-defer-embed batch we know the upcoming
    // `parse_and_embed_files` call will block for model initialization.
    //
    // In-process path: `embedder_pid_slot` is `None` (no sidecar), but ONNX
    // model load still happens on the first embed call. We detect this by
    // checking whether ANY embedding has completed yet — the `indexed` counter
    // is still 0 on the very first batch, so `needs_embedder_init=true` for
    // the first call in either mode.
    //
    // Issue #823 Bug 3: the old code used `.unwrap_or(false)` which silently
    // disabled both events for the in-process embedder. The fix uses a
    // first-batch guard that fires regardless of embedder mode.
    //
    // `lexical_only` and `defer_embed` indexes never embed during the fast
    // pass, so they never stall here — skip the init event for them.
    //
    // We only emit once: after the first embedding call returns successfully,
    // we emit `embedder_ready`. Subsequent batches have indexed > 0 so this
    // branch is skipped. `lexical_only` indexes never embed.
    //
    // Issue #827: for the in-process path also gate on
    // `INPROCESS_EMBEDDER_EVER_READY`. On the very first reindex after daemon
    // start the flag is false → we emit init/ready so the operator sees
    // "Loading model…". Once `embedder_ready` has been emitted, the flag is
    // flipped to true below; on all subsequent reindexes `first_batch_ever`
    // would otherwise re-fire this event even though the model is already warm,
    // causing a spurious "Loading model…" flash. The flag prevents that.
    let first_batch_ever = ctx.progress.indexed.load(AtomicOrdering::Acquire) == 0;
    let needs_embedder_init = !ctx.lexical_only
        && !ctx.defer_embed
        && if let Some(slot) = ctx.embedder_pid_slot.as_ref() {
            // Sidecar mode: PID 0 = not yet spawned.
            slot.load(AtomicOrdering::Acquire) == 0
        } else {
            // In-process (or any other non-sidecar) mode: fire on the very
            // first batch so the CLI gets an embedder_ready signal after the
            // first embed, even if model load is fast.
            // Issue #827: suppress if the model has already been used at least
            // once in this process lifetime (warm daemon, second+ reindex).
            first_batch_ever && !INPROCESS_EMBEDDER_EVER_READY.load(AtomicOrdering::Acquire)
        };

    if needs_embedder_init {
        ctx.progress
            .push(serde_json::json!({
                "event": "embedder_init",
                "index_id": ctx.index_id.0,
            }))
            .await;
    }

    // Issue #109, Phase 1: `lexical_only` indexes skip the embedder
    // entirely. `parse_files_only` returns a `ParsedBatch` whose
    // `embeddings` slot is all `None`, which `commit_parsed_batch`
    // already handles as the BM25-only path.
    //
    // Issue #923: `defer_embed` likewise skips embedding in the fast pass —
    // the same `parse_files_only` path is used.
    //
    // For full-pipeline indexes, use `parse_and_embed_files_tracked` so
    // that per-wave `chunk_progress` SSE events fire at ~32-chunk granularity.
    let parsed = {
        let indexer = ctx.handle.indexer.read().await;
        let result = if ctx.lexical_only || ctx.defer_embed {
            indexer.parse_files_only(to_index).await
        } else {
            use crate::core::indexer::PROGRESS_CHUNK_INTERVAL;
            use std::sync::atomic::Ordering;
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(usize, u64)>();
            let parse_result = indexer.parse_and_embed_files_tracked(to_index, tx).await;
            // Drain per-wave notifications and emit chunk_progress SSE events.
            while let Ok((wave_chunks, wave_ms)) = rx.try_recv() {
                if wave_chunks >= PROGRESS_CHUNK_INTERVAL {
                    let cps = (wave_chunks as u64 * 1000)
                        .checked_div(wave_ms.max(1))
                        .unwrap_or(0);
                    ctx.progress
                        .push(serde_json::json!({
                            "event": "chunk_progress",
                            "chunks_done": wave_chunks as u64,
                            "chunks_per_sec": cps,
                            "embed_ms": wave_ms,
                            "indexed": ctx.progress.indexed.load(Ordering::Acquire),
                            "total_files": ctx.total,
                        }))
                        .await;
                }
            }
            parse_result
        };
        match result {
            Ok(p) => p,
            Err(e) => {
                drop(indexer);
                emit_batch_error(ctx, &payload.to_index_paths, e).await;
                return None;
            }
        }
    };

    // If we emitted `embedder_init` above, follow up with `embedder_ready` now
    // that the sidecar has initialised and the first batch's embeddings are
    // available.
    if needs_embedder_init {
        ctx.progress
            .push(serde_json::json!({
                "event": "embedder_ready",
                "index_id": ctx.index_id.0,
            }))
            .await;
        // Issue #827: for in-process mode, flip the global flag so subsequent
        // reindexes on this daemon don't show the "Loading model…" flash again.
        if ctx.embedder_pid_slot.is_none() {
            INPROCESS_EMBEDDER_EVER_READY.store(true, AtomicOrdering::Release);
        }
    }

    // Problem 2 UX fix: emit a lightweight `chunk_progress` event immediately
    // after the ONNX embedding step finishes but before the commit write-lock
    // is acquired. This lets the CLI ticker update throughput numbers and ETA
    // within seconds of the embedding completing.
    // `defer_embed` passes produce zero vectors in the fast pass; skip to avoid
    // misleading "0 vectors/s" noise.
    if !ctx.lexical_only && !ctx.defer_embed && parsed.vector_count > 0 {
        use std::sync::atomic::Ordering;
        let batch_chunks = parsed.chunks.len() as u64;
        let chunks_per_sec = (batch_chunks * 1000)
            .checked_div(parsed.embed_ms.max(1))
            .unwrap_or(0);
        ctx.progress
            .push(serde_json::json!({
                "event": "chunk_progress",
                "chunks_done": batch_chunks,
                "chunks_per_sec": chunks_per_sec,
                "embed_ms": parsed.embed_ms,
                "indexed": ctx.progress.indexed.load(Ordering::Acquire),
                "total_files": ctx.total,
            }))
            .await;
    }

    Some(ParsedReadyBatch {
        parsed,
        new_hashes: payload.new_hashes,
        batch_files,
        changed_corpus_paths: payload.changed_corpus_paths,
    })
}

/// Stage 2 of the pipelined per-batch flow: commit parsed/embedded chunks
/// under the indexer's WRITE lock, apply success bookkeeping, and run the
/// post-commit memory check.
///
/// Why: commits must remain sequential (one write lock at a time), but the
/// producer can already be reading + parsing the next batch in parallel with
/// this work — that's the whole point of the pipeline (issue #20).
///
/// Issue #855 (delete-then-insert for changed files): before writing the new
/// chunks, remove every prior chunk belonging to each CHANGED file. This
/// prevents orphan chunk IDs when a file re-chunks to FEWER chunks.
///
/// What: acquires the write lock, removes stale chunks for changed files,
/// commits the new `ParsedBatch`, updates counters and hash cache, checks
/// memory, and returns a `BatchOutcome`.
/// Test: `reindex_walks_directory_and_emits_events` verifies correct chunk
/// counts and no duplicates after a re-run.
pub(super) async fn commit_parsed_and_finalize(
    ctx: &BatchCtx,
    ready: ParsedReadyBatch,
) -> BatchOutcome {
    let ParsedReadyBatch {
        parsed,
        new_hashes,
        batch_files,
        changed_corpus_paths,
    } = ready;
    let parse_ms = parsed.parse_ms;
    let embed_ms = parsed.embed_ms;
    let vector_count = parsed.vector_count;

    let commit = {
        let indexer = ctx.handle.indexer.write().await;

        // Issue #855: delete-then-insert for changed files. For every file
        // being re-indexed in this batch, remove its previous chunks from ALL
        // stores before writing the new chunks.
        //
        // Issue #1002: track files whose pre-commit remove failed. Inserting
        // new chunks for those files on top of surviving old chunks would
        // produce duplicate results — guard the batch by filtering them.
        let mut remove_failed_files: std::collections::HashSet<&str> =
            std::collections::HashSet::new();
        let mut remove_failures: usize = 0;
        for file_path in &changed_corpus_paths {
            if let Err(e) = indexer.remove_file_no_kg_rebuild(file_path).await {
                remove_failed_files.insert(file_path.as_str());
                remove_failures += 1;
                tracing::warn!(
                    index_id = %ctx.index_id.0,
                    file = %file_path,
                    error = %e,
                    remove_failures,
                    "reindex: #855 pre-commit remove failed — skipping insert for \
                     this file to avoid duplicate chunks (issue #1002); stale \
                     chunks will persist until next --force reindex"
                );
            }
        }
        // Filter the parsed batch to exclude files whose remove failed.
        let parsed = if remove_failures > 0 {
            parsed.retain_files(|f| !remove_failed_files.contains(f))
        } else {
            parsed
        };

        match indexer.commit_parsed_batch(parsed, true).await {
            Ok(c) => c,
            Err(e) => {
                drop(indexer);
                let placeholder_paths: Vec<PathBuf> =
                    new_hashes.iter().map(|(p, _)| p.clone()).collect();
                emit_batch_error(ctx, &placeholder_paths, e).await;
                return BatchOutcome::default();
            }
        }
    };

    apply_successful_commit(ctx, new_hashes, batch_files, &commit).await;
    let mem_limit_hit = check_post_commit_memory(ctx);

    BatchOutcome {
        parse_ms,
        embed_ms,
        bm25_ms: commit.bm25_ms,
        vector_upsert_ms: commit.vector_upsert_ms,
        vector_count,
        mem_limit_hit,
        chunks_dropped_by_cap: commit.chunks_dropped_by_cap,
    }
}

/// Read every file in `batch` concurrently, then drop read errors, minified
/// content, and hash-matches.
///
/// Why: extracted from the batch stage so filtering logic is independently
/// readable and testable.
/// What: returns a `BatchPayload` containing only the files that need indexing.
/// Test: `prepare_and_parse_batch` integration tests check that hash-matches
/// are excluded from `to_index`.
pub(super) async fn prepare_batch_payload(ctx: &BatchCtx, batch: &[PathBuf]) -> BatchPayload {
    use std::sync::atomic::Ordering;

    // Read every file in the batch concurrently.
    let read_futs = batch.iter().map(|path| {
        let path = path.clone();
        async move {
            let content = tokio::fs::read_to_string(&path).await;
            (path, content)
        }
    });
    let read_results = futures::future::join_all(read_futs).await;

    let mut to_index: Vec<(String, String)> = Vec::with_capacity(batch.len());
    let mut to_index_paths: Vec<PathBuf> = Vec::with_capacity(batch.len());
    let mut new_hashes: Vec<(PathBuf, String)> = Vec::with_capacity(batch.len());
    // Issue #855: track the corpus-relative path of every file being
    // re-indexed so `commit_parsed_and_finalize` can remove the prior
    // chunks before inserting the new set (delete-then-insert per changed
    // file).
    let mut changed_corpus_paths: Vec<String> = Vec::with_capacity(batch.len());
    for (path, content_res) in read_results {
        let rel = to_corpus_relative_path(&ctx.root, &path);
        let content = match content_res {
            Ok(c) => c,
            Err(e) => {
                ctx.progress.errors.fetch_add(1, Ordering::Release);
                ctx.progress
                    .push(serde_json::json!({
                        "event": "error",
                        "file": rel,
                        "message": format!("read: {e}"),
                        "indexed": ctx.progress.indexed.load(Ordering::Acquire),
                        "total_files": ctx.total,
                    }))
                    .await;
                continue;
            }
        };
        if should_skip_content(&path, &content) {
            tracing::debug!("reindex: skipping minified content in {}", path.display());
            emit_skip(ctx, &rel, Some("minified")).await;
            continue;
        }
        let h = hash_content(&content);
        // Issue #1073: use the corpus-RELATIVE PathBuf as the DashMap key so
        // the in-process cache and the redb-persisted cache use the SAME key space.
        let rel_path = PathBuf::from(&rel);
        if ctx
            .hashes
            .get(&rel_path)
            .map(|prev| *prev == h)
            .unwrap_or(false)
        {
            emit_skip(ctx, &rel, None).await;
            continue;
        }
        // Issue #402 — relocation resilience: store file paths RELATIVE to the
        // index root so the corpus is portable when `root_path` is updated.
        let path_str = rel.clone();
        to_index.push((path_str, content));
        to_index_paths.push(path.clone());
        // Issue #1073: use the relative path as the hash-map key.
        new_hashes.push((rel_path, h));
        // Issue #855: record the corpus-relative path so the commit phase
        // can remove prior (stale) chunks for this file before inserting the
        // new set.
        changed_corpus_paths.push(rel);
    }

    BatchPayload {
        to_index,
        to_index_paths,
        new_hashes,
        changed_corpus_paths,
    }
}

/// Push a `skip` SSE event, bumping the per-progress skipped/indexed counters.
///
/// Why: hash-skipped and minified-skipped files still consume a file slot in
/// the progress bar; skipping them silently would make the bar stall.
/// What: increments `skipped` and `indexed`, then pushes a `skip` event.
/// Test: covered by the skip-counting assertions in reindex integration tests.
pub(super) async fn emit_skip(ctx: &BatchCtx, rel: &str, reason: Option<&str>) {
    use std::sync::atomic::Ordering;
    ctx.progress.skipped.fetch_add(1, Ordering::Release);
    let indexed = ctx.progress.indexed.fetch_add(1, Ordering::Release) + 1;
    let mut event = serde_json::json!({
        "event": "skip",
        "file": rel,
        "indexed": indexed,
        "total_files": ctx.total,
    });
    if let Some(r) = reason {
        event["reason"] = serde_json::Value::String(r.to_string());
    }
    ctx.progress.push(event).await;
}

/// Emit one `error` SSE event covering every file in a failed batch.
///
/// Why: a batch-level parse/embed failure covers multiple files; attributing
/// the error to each file individually would require re-reading the parsed
/// output after a failure, which is not available. A single batch-level event
/// is the pragmatic choice.
/// What: increments `errors` by the batch size and pushes an `error` event
/// carrying the list of files.
/// Test: covered by error-path integration tests.
pub(super) async fn emit_batch_error(
    ctx: &BatchCtx,
    to_index_paths: &[PathBuf],
    err: anyhow::Error,
) {
    use std::sync::atomic::Ordering;
    let files_in_batch: Vec<String> = to_index_paths
        .iter()
        .map(|p| to_corpus_relative_path(&ctx.root, p))
        .collect();
    // Issue #1428: a batch parse/embed/commit failure was previously surfaced
    // ONLY as an SSE `error` frame — nothing reached the daemon log, which is
    // exactly the "no error logged" symptom on a GPU OOM / sidecar stall. Log
    // the underlying cause at `error!` to stderr so the operator can always see
    // WHY a batch failed (the `{:#}` alternate form expands the anyhow cause
    // chain, e.g. "batch embed_batch failed: embed call timed out").
    tracing::error!(
        index_id = %ctx.index_id.0,
        batch_files = to_index_paths.len(),
        indexed = ctx.progress.indexed.load(Ordering::Acquire),
        total_files = ctx.total,
        "reindex[{}]: batch failed — {err:#}",
        ctx.index_id.0,
    );
    ctx.progress
        .errors
        .fetch_add(to_index_paths.len(), Ordering::Release);
    ctx.progress
        .push(serde_json::json!({
            "event": "error",
            "files": files_in_batch,
            "message": format!("batch index: {err}"),
            "indexed": ctx.progress.indexed.load(Ordering::Acquire),
            "total_files": ctx.total,
        }))
        .await;
}

/// Apply a successful commit: update progress counters, persist hashes,
/// shrink the hash cache if oversize, and emit the per-batch SSE event.
///
/// Why: factors out the bookkeeping that follows every successful
/// `commit_parsed_batch` call so `commit_parsed_and_finalize` stays focused
/// on the error paths.
/// What: updates `total_chunks` and `indexed`, mirrors new hashes to the
/// redb store, and pushes a `batch` event with throughput stats.
/// Test: verified by the `batch_chunks` / `elapsed_ms` assertions in
/// integration tests.
pub(super) async fn apply_successful_commit(
    ctx: &BatchCtx,
    new_hashes: Vec<(PathBuf, String)>,
    batch_files: usize,
    commit: &CommitTimings,
) {
    use std::sync::atomic::Ordering;
    let new_chunks = commit.chunks;
    ctx.progress
        .total_chunks
        .fetch_add(new_chunks, Ordering::Release);
    let indexed = ctx
        .progress
        .indexed
        .fetch_add(batch_files, Ordering::Release)
        + batch_files;
    let elapsed_ms = ctx.started.elapsed().as_millis() as u64;
    let chunks_per_sec = (ctx.progress.total_chunks.load(Ordering::Acquire) as u64 * 1000)
        .checked_div(elapsed_ms)
        .unwrap_or(0);
    for (path, h) in &new_hashes {
        ctx.hashes.insert(path.clone(), h.clone());
    }
    // Issue #75: cap per-index hash-cache size.
    shrink_hashes_if_needed(&ctx.hashes);
    // Issue #662: mirror the newly-committed hashes to the redb corpus store
    // so they survive daemon restarts.
    hash_cache::persist_batch(
        &ctx.handle,
        &new_hashes,
        MAX_FILE_HASHES_PER_INDEX,
        ctx.hashes.len(),
    )
    .await;
    ctx.progress
        .push(serde_json::json!({
            "event": "batch",
            "batch_files": batch_files,
            "batch_chunks": new_chunks,
            "indexed": indexed,
            "total_files": ctx.total,
            "elapsed_ms": elapsed_ms,
            "chunks_per_sec": chunks_per_sec,
        }))
        .await;
}

/// Sample RSS after the commit phase and trip the abort flag if the limit was
/// hit. Returns `true` when the caller must break out of the batch loop.
///
/// Why: issue #82 — the commit phase (HNSW insert + redb write + BM25 update)
/// is the single largest in-batch allocator. Sampling RSS here, in addition
/// to the pre-batch abort-flag check, means a runaway batch can only push RSS
/// one batch over the limit before being noticed.
/// What: reads current RSS, updates `peak_rss_atomic`, and stores `true` into
/// `mem_abort` when the limit is breached.
/// Test: covered by memory-abort integration tests.
pub(super) fn check_post_commit_memory(ctx: &BatchCtx) -> bool {
    use crate::core::memguard::current_rss_mb;
    let Some(limit) = ctx.mem_limit else {
        return false;
    };
    let Some(rss) = current_rss_mb() else {
        return false;
    };
    let prev_peak = ctx.peak_rss_atomic.load(AtomicOrdering::Acquire);
    if rss > prev_peak {
        ctx.peak_rss_atomic.store(rss, AtomicOrdering::Release);
    }
    if rss >= limit {
        tracing::warn!(
            "reindex: memory limit hit after commit \
             (rss={}MB >= limit={}MB) — skipping \
             remaining batches for index {}",
            rss,
            limit,
            ctx.index_id.0
        );
        ctx.mem_abort.store(true, AtomicOrdering::Release);
        return true;
    }
    false
}
