//! Reindex orchestration with SSE progress tracking.
//!
//! Why: A full reindex of a project may touch hundreds or thousands of files.
//! The CLI wants to render a progress bar; the daemon wants to fire-and-forget.
//! This module bridges the two via `tokio::sync::broadcast` channels and a
//! per-index `ReindexProgress` snapshot stored on `SearchAppState`.
//!
//! What:
//! - `ReindexProgress` — current state of a reindex (status counters + replay
//!   buffer + broadcast sender).
//! - `spawn_reindex` — kick off a background task that walks `root_path`,
//!   indexes each file, and emits progress events.
//!
//! Test: see `crates/trusty-search-service/src/reindex.rs#tests`.

use crate::core::indexer::{CommitTimings, ParsedBatch};
use crate::core::memguard::{current_rss_mb, index_memory_limit_mb};
use crate::core::registry::{IndexHandle, IndexId};
use crate::service::walker::{should_skip_content, walk_source_files_with_options, WalkOptions};
use anyhow::Context;
use crossbeam_utils::atomic::AtomicCell;
use dashmap::DashMap;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, mpsc, Mutex, Semaphore};

/// Shared handle to the daemon's per-index `GraphScorer` cache (issue #81).
///
/// Why: community detection runs on a detached task that finishes after the
/// reindex handler's queue-time cache invalidation, so the reindex tail needs
/// a way to re-invalidate the cache once the partition is persisted. Aliasing
/// the `DashMap` type keeps the long signature readable and avoids pulling the
/// whole `SearchAppState` into this module.
/// What: the same `DashMap<IndexId, Arc<GraphScorer>>` that `SearchAppState`
/// owns; `remove` evicts a stale scorer so the next search rebuilds it.
/// Test: `tests::community_detection_invalidates_scorer_cache`.
pub type GraphScorerCache =
    Arc<DashMap<IndexId, Arc<crate::core::indexer::graph_score::GraphScorer>>>;

/// Machine-wide reindex serializer.
///
/// Why: Historically a 1-permit semaphore here protected the daemon from
/// OOM when multiple reindexes raced (each ONNX session arena held 7–10 GB
/// of working memory). As of issue #41 Phase 1 this is supplanted by the
/// `EmbedPool`'s bounded priority channels + the HTTP-level concurrency
/// limiter — both bound concurrent embedding work without serializing all
/// reindex bookkeeping unnecessarily. The semaphore is retained at width
/// `MAX_PARALLEL_REINDEXES` so a misconfigured caller can't fan out an
/// unbounded number of reindex tasks (which would still race the redb
/// + HNSW write locks even though embedding itself is bounded).
///
/// What: A small N-permit semaphore (default 2) sized to allow a few
/// concurrent reindexes while still bounding peak memory. Waiting
/// reindexes queue (the SSE stream is already connected and will start
/// emitting events once the permit is held).
fn reindex_semaphore() -> &'static Semaphore {
    static SEM: OnceLock<Semaphore> = OnceLock::new();
    SEM.get_or_init(|| Semaphore::new(MAX_PARALLEL_REINDEXES))
}

/// Maximum number of concurrent reindex tasks (issue #41 Phase 1). Keeps a
/// little headroom for an interactive reindex to start while a background
/// one is running. The embedder pool already gates the actual embedding
/// work, so the value here just bounds the redb + HNSW lock contention.
const MAX_PARALLEL_REINDEXES: usize = 2;

/// Files per parallel batch. Each batch is parsed in parallel via rayon and
/// embedded in ONNX batches (`EMBED_BATCH_SIZE` chunks at a time inside the
/// batch). The full `ParsedBatch` (chunk content + embeddings + entities for
/// every file in the batch) is held in memory until the commit phase finishes.
///
/// 128 files bounds peak memory during reindex. On a 595-file repo with ~8
/// chunks/file, a 512-file batch held ~4k chunks of source content plus their
/// 384-dim f32 embeddings plus ONNX intermediate activation tensors retained
/// across the embed loop — pushing RSS to 33–50 GB and triggering macOS Jetsam
/// kill. With 128 files per batch, the working set caps at ~1k chunks worth of
/// memory, and the ONNX session arena gets multiple opportunities to release
/// transient buffers between commits. SSE progress events fire per batch, so a
/// smaller batch size also gives more granular progress updates — the downside
/// is slightly more lock-acquisition overhead, which is negligible vs. the
/// per-batch parse+embed cost.
const REINDEX_BATCH_SIZE: usize = 128;

/// Per-index, per-process content-hash cache. Used to skip reindexing files
/// whose content hasn't changed since the last reindex in this daemon's
/// lifetime. Survives across `POST /indexes/:id/reindex` calls but not daemon
/// restarts (acceptable: cold start re-embeds everything anyway, and on warm
/// daemons the user expects "skip unchanged" behaviour).
fn file_hashes() -> &'static DashMap<IndexId, Arc<DashMap<PathBuf, String>>> {
    static FILE_HASHES: OnceLock<DashMap<IndexId, Arc<DashMap<PathBuf, String>>>> = OnceLock::new();
    FILE_HASHES.get_or_init(DashMap::new)
}

fn hashes_for(id: &IndexId) -> Arc<DashMap<PathBuf, String>> {
    file_hashes()
        .entry(id.clone())
        .or_insert_with(|| Arc::new(DashMap::new()))
        .clone()
}

/// Per-index ceiling on the content-hash cache (issue #75). Each entry holds
/// a `PathBuf` + 64-char hex SHA-256 string, so 200k entries ≈ ~30–60 MB.
/// When exceeded we drain ~10% of the entries (DashMap has no ordering, so
/// the eviction set is arbitrary — those files are simply re-hashed on the
/// next reindex, which is the safe, correct fallback).
const MAX_FILE_HASHES_PER_INDEX: usize = 200_000;

/// Drop ~10% of entries from `map` when above `MAX_FILE_HASHES_PER_INDEX`.
///
/// Why: prevents an unbounded growth in the per-daemon content-hash cache
/// when a project gets ever-larger or files are renamed many times. The
/// hash cache is a pure speed optimisation (skip re-embed for unchanged
/// files), so evicting entries is always safe — affected files just get
/// re-hashed and re-embedded on the next reindex.
/// What: collects an arbitrary subset of keys and removes them. DashMap has
/// no insertion-order metadata so we can't do "true" LRU; arbitrary eviction
/// is acceptable for a cache whose miss penalty is just extra work.
/// Test: covered indirectly by the reindex test (oversizing not exercised).
fn shrink_hashes_if_needed(map: &DashMap<PathBuf, String>) {
    let len = map.len();
    if len <= MAX_FILE_HASHES_PER_INDEX {
        return;
    }
    let target = MAX_FILE_HASHES_PER_INDEX * 9 / 10;
    let to_remove = len.saturating_sub(target);
    let keys: Vec<PathBuf> = map
        .iter()
        .take(to_remove)
        .map(|e| e.key().clone())
        .collect();
    for k in keys {
        map.remove(&k);
    }
    tracing::info!(
        "file-hash cache exceeded {} entries — dropped {} to bound memory",
        MAX_FILE_HASHES_PER_INDEX,
        to_remove
    );
}

/// Max replay events buffered on a `ReindexProgress`. A full reindex emits
/// ~100 events for a 14k-file repo (one per batch + start/complete), but
/// pathological cases (per-file errors) could otherwise grow the vector
/// without bound. Late SSE subscribers still see the most recent 500 events,
/// which is more than enough to replay context.
const MAX_REPLAY_EVENTS: usize = 500;

/// How long to keep a completed (`Complete` / `Failed`) `ReindexProgress`
/// on `SearchAppState::reindex_progress` before garbage-collecting it.
/// 60 s is enough for late SSE subscribers to attach and read the final
/// state but short enough that long-running daemons don't accumulate
/// thousands of stale progress entries.
const REINDEX_PROGRESS_TTL_SECS: u64 = 60;

/// Stable content fingerprint for the "skip unchanged file" optimization.
///
/// Why: SHA-256 is collision-resistant and stable across processes, builds,
/// and Rust versions. `DefaultHasher` (SipHash) is randomized per build and
/// has weaker collision properties — fine for `HashMap` keys but unsafe for
/// content fingerprinting where a false negative silently skips a real edit.
/// What: SHA-256 of the file's UTF-8 bytes, hex-encoded.
/// Test: see `reindex_walks_directory_and_emits_events` — a re-run of the
/// reindex with unchanged files must mark them as skipped (proves the hash
/// is stable across two invocations within the same process).
fn hash_content(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Capacity of the per-reindex broadcast channel. Lagged subscribers will
/// drop events older than this — the SSE handler also replays from the buffer
/// stored in `events`, so late subscribers still see the full history.
const BROADCAST_CAPACITY: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ReindexStatus {
    Running,
    Complete,
    /// Issue #120: the reindex aborted because the soft RSS ceiling
    /// (`TRUSTY_MEMORY_LIMIT_MB`) was breached. Distinguished from `Complete`
    /// so external callers can apply a cooldown before retrying — re-running
    /// immediately would just hit the limit again, producing an infinite
    /// reindex loop.
    AbortedMemory,
    Failed,
}

/// Live state of a reindex. Wrapped in `Arc` and stored on
/// `SearchAppState::reindex_progress` so concurrent SSE subscribers can read
/// the same snapshot without coordinating.
pub struct ReindexProgress {
    pub status: AtomicCell<ReindexStatus>,
    pub total_files: std::sync::atomic::AtomicUsize,
    pub indexed: std::sync::atomic::AtomicUsize,
    pub total_chunks: std::sync::atomic::AtomicUsize,
    pub errors: std::sync::atomic::AtomicUsize,
    /// Files skipped because their content hash matched the previous reindex.
    pub skipped: std::sync::atomic::AtomicUsize,
    /// Append-only log of JSON-encoded events. Replayed to late SSE
    /// subscribers so they don't miss earlier `start` / `progress` events.
    pub events: Arc<Mutex<Vec<String>>>,
    /// Live event broadcaster. Subscribers receive new events as they're sent.
    pub sender: broadcast::Sender<String>,
}

impl ReindexProgress {
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(BROADCAST_CAPACITY);
        Self {
            status: AtomicCell::new(ReindexStatus::Running),
            total_files: Default::default(),
            indexed: Default::default(),
            total_chunks: Default::default(),
            errors: Default::default(),
            skipped: Default::default(),
            events: Arc::new(Mutex::new(Vec::new())),
            sender,
        }
    }

    /// Push an event onto the replay buffer and broadcast it to live subscribers.
    /// Caps the replay buffer at `MAX_REPLAY_EVENTS` to bound memory under
    /// pathological reindexes (e.g. one error event per file).
    pub async fn push(&self, event: serde_json::Value) {
        let line = event.to_string();
        {
            let mut buf = self.events.lock().await;
            if buf.len() >= MAX_REPLAY_EVENTS {
                // Drop the oldest event. `remove(0)` is O(n) but n ≤ 500.
                buf.remove(0);
            }
            buf.push(line.clone());
        }
        // Broadcast errors (no receivers) are fine — replay buffer still has it.
        let _ = self.sender.send(line);
    }
}

impl Default for ReindexProgress {
    fn default() -> Self {
        Self::new()
    }
}

/// Spawn a background tokio task that walks `handle.root_path`, indexes each
/// source file, and emits progress events into `progress`.
///
/// Returns immediately; the caller (the HTTP handler) drops its reference and
/// the task runs to completion. When `cleanup_map` is `Some`, the entry for
/// `handle.id` is removed from the map `REINDEX_PROGRESS_TTL_SECS` after
/// completion (issue #75: bounds long-running daemon memory by GC'ing stale
/// progress entries while still letting late SSE subscribers read the final
/// state for a short window).
pub fn spawn_reindex(handle: Arc<IndexHandle>, progress: Arc<ReindexProgress>, force: bool) {
    spawn_reindex_with_cleanup(handle, progress, force, None, None, None);
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
    };
    for subtree in &include_paths {
        let w = walk_source_files_with_options(subtree, walk_opts);
        walked_files.extend(w.files);
        total_skipped_dirs = total_skipped_dirs.saturating_add(w.skipped_dirs);
    }

    // Apply repo-config filters. These are AND-composed on top of the
    // walker's built-in ignores (`SKIP_DIRS`, `should_skip_path`).
    //
    // 1. `exclude_globs`: drop any file whose path matches one of the
    //    user-supplied glob patterns.
    // 2. `extensions`: when non-empty, keep only files whose extension
    //    appears in the allow-list (caller writes them without the leading
    //    dot, e.g. `["rs", "py"]`).
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
    // subdirectories of `root_path` matching one of the configured glob
    // patterns. Distinct from `include_paths` (absolute subtrees) — this
    // operates on the *basename* of the first component under `root_path`,
    // so callers can write `["common-*", "duetto-common*"]` without enumerating
    // every repo path manually.
    //
    // Walker output is canonicalised (issue: indexed-paths-mismatch) so the
    // `strip_prefix` inside `path_matches_filter` needs the canonical root
    // too — otherwise a non-validated handle (e.g. the test harness) whose
    // `root_path` still carries a symlink alias would mismatch every walked
    // file. Best-effort canonicalisation with a fallback to the stored value.
    if !handle.path_filter.is_empty() {
        let patterns = handle.path_filter.clone();
        let root =
            std::fs::canonicalize(&handle.root_path).unwrap_or_else(|_| handle.root_path.clone());
        walked_files.retain(|p| crate::core::registry::path_matches_filter(p, &root, &patterns));
    }

    // De-duplicate when multiple `include_paths` overlap (e.g. `["."]` plus
    // `["src"]`). `walk_source_files` returns canonicalised paths inside each
    // subtree (issue: indexed-paths-mismatch) but doesn't dedupe across
    // subtrees.
    walked_files.sort();
    walked_files.dedup();

    crate::service::walker::WalkResult {
        files: walked_files,
        skipped_dirs: total_skipped_dirs,
    }
}

/// Spawn the background RSS poller that watches for `TRUSTY_MEMORY_LIMIT_MB`
/// breaches. Returns the join handle plus a stop-flag the caller flips when
/// the reindex finishes.
///
/// Why: extracted from `spawn_reindex_with_cleanup` (issue #98) so the
/// memory-protection plumbing is isolated from the batch loop. Always run
/// even when no `mem_limit` is configured so `peak_rss_mb` is accurate for
/// the final log line — the overhead is one sysinfo refresh per second.
/// What: ticks every `MEM_POLL_INTERVAL`, updates `peak_rss` monotonically,
/// and trips `mem_abort` the first time RSS crosses `mem_limit`.
fn spawn_memory_poller(
    mem_limit: Option<u64>,
    mem_abort: Arc<AtomicBool>,
    peak_rss: Arc<AtomicU64>,
    index_id: String,
) -> (tokio::task::JoinHandle<()>, Arc<AtomicBool>) {
    /// How often the background poller samples RSS. 1 s strikes a balance
    /// between catching mid-batch spikes and the cost of
    /// `sysinfo::refresh_processes_specifics` (~1–3 ms on macOS).
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
                // Update peak monotonically (CAS loop: the post-commit RSS
                // check on the main task can race this poller).
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
                        // Keep polling so peak_rss continues to track until
                        // the main loop notices the flag.
                    }
                }
            }
            ticker.tick().await;
        }
    });
    (handle, stop)
}

/// Schedule deferred GC of the `reindex_progress` map entry for this index.
///
/// Why: issue #75 — bounds long-running daemon memory by GC'ing stale progress
/// entries while still letting late SSE subscribers read the final
/// `complete` / `error` event for `REINDEX_PROGRESS_TTL_SECS`.
fn schedule_progress_cleanup(
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
async fn refresh_context_embedding(handle: &Arc<IndexHandle>) {
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

/// Shared context threaded into `process_one_batch` so the per-batch helper
/// doesn't take a dozen arguments. Cheap to clone (everything is `Arc` /
/// `PathBuf` / scalar).
///
/// Why: extracted from `spawn_reindex_with_cleanup` (issue #98) so the
/// orchestrator's per-batch body is testable and bounded in size.
#[derive(Clone)]
struct BatchCtx {
    handle: Arc<IndexHandle>,
    progress: Arc<ReindexProgress>,
    root: PathBuf,
    index_id: IndexId,
    hashes: Arc<DashMap<PathBuf, String>>,
    mem_limit: Option<u64>,
    mem_abort: Arc<AtomicBool>,
    peak_rss_atomic: Arc<AtomicU64>,
    started: Instant,
    total: usize,
}

/// What a single batch contributed to the run-level totals. The orchestrator
/// folds each `BatchOutcome` into its accumulators and breaks the batch loop
/// when `mem_limit_hit` is set.
#[derive(Default)]
struct BatchOutcome {
    parse_ms: u64,
    embed_ms: u64,
    bm25_ms: u64,
    vector_upsert_ms: u64,
    vector_count: usize,
    /// True when the post-commit RSS check tripped the abort flag — caller
    /// must break out of the batch loop.
    mem_limit_hit: bool,
}

/// Process a single batch end-to-end: read files, filter (hash-skip,
/// minified), parse+embed, commit, emit SSE events, and run the post-commit
/// memory check.
///
/// Why: extracted from `spawn_reindex_with_cleanup` (issue #98) to bring the
/// orchestrator's cyclomatic complexity below the threshold. Combines the
/// stages that share read/filter state into one function whose responsibility
/// is "advance the index by one batch".
///
/// Note: this monolithic path is retained for callers/tests that prefer a
/// single sequential per-batch helper. The pipelined orchestrator in
/// `spawn_reindex_with_cleanup` uses [`prepare_and_parse_batch`] +
/// [`commit_parsed_and_finalize`] to overlap batch N's commit (write lock)
/// with batch N+1's read+parse (no write lock).
#[allow(dead_code)]
async fn process_one_batch(ctx: &BatchCtx, batch: &[PathBuf]) -> BatchOutcome {
    let Some(parsed) = prepare_and_parse_batch(ctx, batch).await else {
        return BatchOutcome::default();
    };
    commit_parsed_and_finalize(ctx, parsed).await
}

/// One in-flight batch ready for the commit (write-lock) stage. Produced by
/// the read+parse producer task and consumed sequentially by the orchestrator
/// commit loop.
///
/// Why: pipelining batch N+1's read+parse with batch N's commit (issue #20)
/// requires shipping a self-contained unit between tasks. `ParsedBatch` owns
/// its chunks/embeddings (no borrows), so this struct can be sent across an
/// mpsc channel freely.
struct ParsedReadyBatch {
    parsed: ParsedBatch,
    new_hashes: Vec<(PathBuf, String)>,
    /// Files actually submitted to the indexer (post hash/minified filtering).
    /// Used to size the per-batch commit event.
    batch_files: usize,
}

/// Stage 1 of the pipelined per-batch flow: read every file in `batch`,
/// filter out hash-matches/minified/errors, then parse + embed under the
/// indexer's READ lock. Returns `None` when no files in the batch needed
/// indexing (so the orchestrator can skip the commit altogether).
///
/// Why: split from the commit stage so the producer task can race ahead of
/// the consumer's write-lock work. This is the half that does NOT take the
/// indexer write lock, so multiple invocations are safe to overlap with an
/// in-progress commit on the same handle.
///
/// Errors from `parse_and_embed_files` are surfaced via an SSE `error` event
/// and converted into `None` (skip), matching the previous
/// `process_one_batch` semantics.
async fn prepare_and_parse_batch(ctx: &BatchCtx, batch: &[PathBuf]) -> Option<ParsedReadyBatch> {
    let payload = prepare_batch_payload(ctx, batch).await;
    if payload.to_index.is_empty() {
        return None;
    }
    let batch_files = payload.to_index.len();
    let to_index = payload.to_index;
    let parsed = {
        let indexer = ctx.handle.indexer.read().await;
        match indexer.parse_and_embed_files(to_index).await {
            Ok(p) => p,
            Err(e) => {
                drop(indexer);
                emit_batch_error(ctx, &payload.to_index_paths, e).await;
                return None;
            }
        }
    };
    Some(ParsedReadyBatch {
        parsed,
        new_hashes: payload.new_hashes,
        batch_files,
    })
}

/// Stage 2 of the pipelined per-batch flow: take the parsed/embedded chunks
/// produced by [`prepare_and_parse_batch`], commit them under the indexer's
/// WRITE lock, apply success bookkeeping, and run the post-commit memory
/// check.
///
/// Why: commits must remain sequential (one write lock at a time), but the
/// producer can already be reading + parsing the next batch in parallel with
/// this work — that's the whole point of the pipeline (issue #20).
async fn commit_parsed_and_finalize(ctx: &BatchCtx, ready: ParsedReadyBatch) -> BatchOutcome {
    let ParsedReadyBatch {
        parsed,
        new_hashes,
        batch_files,
    } = ready;
    let parse_ms = parsed.parse_ms;
    let embed_ms = parsed.embed_ms;
    let vector_count = parsed.vector_count;

    let commit = {
        let indexer = ctx.handle.indexer.write().await;
        match indexer.commit_parsed_batch(parsed, true).await {
            Ok(c) => c,
            Err(e) => {
                drop(indexer);
                // We no longer hold the original paths here, but the prior
                // behaviour was to attribute the error to the whole batch.
                // Best-effort: emit a generic batch error covering the files
                // that would have been committed.
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
    }
}

/// Sanitised contents of one batch after read + filter passes.
struct BatchPayload {
    to_index: Vec<(String, String)>,
    to_index_paths: Vec<PathBuf>,
    new_hashes: Vec<(PathBuf, String)>,
}

/// Read every file in `batch` concurrently, then drop read errors, minified
/// content, and hash-matches. Each drop emits the right SSE event so the UI
/// progress bar advances even for skipped/error files.
async fn prepare_batch_payload(ctx: &BatchCtx, batch: &[PathBuf]) -> BatchPayload {
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
    for (path, content_res) in read_results {
        let rel = path
            .strip_prefix(&ctx.root)
            .unwrap_or(&path)
            .display()
            .to_string();
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
        if ctx
            .hashes
            .get(&path)
            .map(|prev| *prev == h)
            .unwrap_or(false)
        {
            emit_skip(ctx, &rel, None).await;
            continue;
        }
        let path_str = path.to_string_lossy().to_string();
        to_index.push((path_str, content));
        to_index_paths.push(path.clone());
        new_hashes.push((path, h));
    }

    BatchPayload {
        to_index,
        to_index_paths,
        new_hashes,
    }
}

/// Push a `skip` SSE event, bumping the per-progress skipped/indexed
/// counters. `reason` is included when supplied (e.g. `"minified"`).
async fn emit_skip(ctx: &BatchCtx, rel: &str, reason: Option<&str>) {
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

/// Emit one `error` SSE event covering every file in a failed batch. Caller
/// can retry the failing files individually via `index_file`.
async fn emit_batch_error(ctx: &BatchCtx, to_index_paths: &[PathBuf], err: anyhow::Error) {
    use std::sync::atomic::Ordering;
    let files_in_batch: Vec<String> = to_index_paths
        .iter()
        .map(|p| p.strip_prefix(&ctx.root).unwrap_or(p).display().to_string())
        .collect();
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
async fn apply_successful_commit(
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
    for (path, h) in new_hashes {
        ctx.hashes.insert(path, h);
    }
    // Issue #75: cap per-index hash-cache size. Pure speed cache, so arbitrary
    // eviction is always safe.
    shrink_hashes_if_needed(&ctx.hashes);
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
fn check_post_commit_memory(ctx: &BatchCtx) -> bool {
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

/// Result of the final symbol-graph rebuild.
struct KgRebuildOutcome {
    symbol_count: usize,
    edge_count: usize,
    kg_ms: u64,
    kg_skipped: bool,
}

/// Rebuild the symbol graph once for the whole reindex.
///
/// Why: deferred from per-batch rebuilds because each rebuild is O(N + E) over
/// the entire corpus and would scale quadratically with file count. Issue #90:
/// always run even after a memory abort — the persisted chunks carry
/// `function_name` and `calls`, graph construction is bounded by
/// `TRUSTY_MAX_KG_NODES`, and it is independent of the embedding pipeline
/// that caused the abort.
async fn rebuild_symbol_graph_for_reindex(handle: &IndexHandle) -> KgRebuildOutcome {
    let kg_start = Instant::now();
    let indexer = handle.indexer.read().await;
    indexer.rebuild_symbol_graph_now().await;
    let g = indexer.symbol_graph().await;
    KgRebuildOutcome {
        symbol_count: g.node_count(),
        edge_count: g.edge_count(),
        kg_ms: kg_start.elapsed().as_millis() as u64,
        kg_skipped: false,
    }
}

/// Spawn Louvain community detection on a blocking thread (issue #41 phase 3).
///
/// Why: detection on a 100k-node graph can take several seconds. Running it
/// inline in the reindex tail would delay the `complete` SSE event for every
/// reindex; running it inside the async runtime would block the executor.
/// `tokio::task::spawn_blocking` keeps both surfaces responsive.
/// What: snapshots the in-memory `SymbolGraph` + the corpus store, then runs
/// `detect_and_save_communities` on a blocking thread. Logs the resulting
/// community count + modularity on success; a failure is `warn`-logged so the
/// reindex still completes cleanly.
/// Test: covered indirectly via the persistence round-trip in
/// `symbol_graph::tests::test_detect_and_save_communities_round_trip`.
fn spawn_community_detection(handle: &IndexHandle, scorer_cache: Option<GraphScorerCache>) {
    let indexer_arc = Arc::clone(&handle.indexer);
    tokio::spawn(async move {
        let (graph, corpus, index_id) = {
            let indexer = indexer_arc.read().await;
            (
                indexer.snapshot_symbol_graph().await,
                indexer.corpus_store(),
                indexer.index_id.clone(),
            )
        };
        let Some(corpus) = corpus else {
            tracing::debug!(
                "index '{index_id}': skipping community detection — no corpus store wired"
            );
            return;
        };
        if graph.node_count() == 0 {
            tracing::debug!(
                "index '{index_id}': skipping community detection — empty symbol graph"
            );
            return;
        }
        let join =
            tokio::task::spawn_blocking(move || graph.detect_and_save_communities(&corpus)).await;
        match join {
            Ok(Ok(communities)) => {
                tracing::info!(
                    index = %index_id,
                    communities = communities.community_count,
                    modularity = communities.modularity,
                    "community detection complete"
                );
                // Issue #81: communities are detected and persisted *after* the
                // KG rebuild, on this detached task — strictly later than the
                // queue-time `invalidate_graph_scorer` call in the reindex
                // handler. A `GraphScorer` built between queue-time and now
                // (e.g. a search that raced the reindex) would have been cached
                // with an empty `community_map`, so `community_cohesion` would
                // be stuck at 0.0 forever. Invalidate the cache here, once the
                // partition is durably written, so the next search rebuilds the
                // scorer against the freshly-persisted communities.
                if let Some(cache) = scorer_cache {
                    cache.remove(&IndexId::new(index_id.clone()));
                    tracing::debug!(
                        index = %index_id,
                        "invalidated cached GraphScorer after community detection"
                    );
                }
            }
            Ok(Err(e)) => {
                tracing::warn!("index '{index_id}': community detection failed: {e:#}")
            }
            Err(e) => tracing::warn!("index '{index_id}': community detection task panicked: {e}"),
        }
    });
}

/// Run-level timing + memory totals collected across every batch.
struct RunTotals {
    parse_ms: u64,
    embed_ms: u64,
    bm25_ms: u64,
    vector_upsert_ms: u64,
    vector_count: usize,
    mem_limit_hit: bool,
}

/// Emit the terminal `complete` SSE event with run-level timings + counters.
///
/// Why: extracted from `spawn_reindex_with_cleanup` (issue #98) so the
/// orchestrator doesn't carry a ~30-line JSON literal at its tail.
async fn emit_complete_event(
    progress: &ReindexProgress,
    started: Instant,
    peak_rss_mb: u64,
    totals: &RunTotals,
    kg: &KgRebuildOutcome,
) {
    use std::sync::atomic::Ordering;
    let total_chunks = progress.total_chunks.load(Ordering::Acquire);
    let elapsed_ms = started.elapsed().as_millis() as u64;
    let chunks_per_sec = (total_chunks as u64 * 1000)
        .checked_div(elapsed_ms)
        .unwrap_or(0);
    let indexed_final = progress.indexed.load(Ordering::Acquire);
    // Issue #120: surface the terminal status string in the SSE payload so
    // external callers (CLI, dashboard, open-mpm) can distinguish a clean
    // completion from a memory-abort without reading the daemon log.
    let status_str = if totals.mem_limit_hit {
        "aborted_memory"
    } else {
        "complete"
    };
    progress
        .push(serde_json::json!({
            "event": "complete",
            "status": status_str,
            "indexed": indexed_final,
            "total_chunks": total_chunks,
            "skipped": progress.skipped.load(Ordering::Acquire),
            "errors": progress.errors.load(Ordering::Acquire),
            "elapsed_ms": elapsed_ms,
            "chunks_per_sec": chunks_per_sec,
            "peak_rss_mb": peak_rss_mb,
            "memory_limit_hit": totals.mem_limit_hit,
            "kg_skipped": kg.kg_skipped,
            "timings": {
                "parse_ms": totals.parse_ms,
                "embed_ms": totals.embed_ms,
                "bm25_ms": totals.bm25_ms,
                "vector_upsert_ms": totals.vector_upsert_ms,
                "kg_ms": kg.kg_ms,
                "vector_count": totals.vector_count,
                "symbol_count": kg.symbol_count,
                "edge_count": kg.edge_count,
            },
        }))
        .await;
}

/// Begin the atomic-swap corpus staging for a `--force` reindex (issue #28,
/// Phase 4).
///
/// Why: a `--force` reindex rebuilds the entire corpus. Committing those
/// chunks straight into the live `index.redb` would expose a half-rebuilt
/// corpus to concurrent searches and, on a crash mid-reindex, leave the
/// serving corpus permanently torn. Staging the new corpus in a sibling
/// `index.redb.tmp` and atomically renaming it into place only on success
/// keeps the live corpus intact until the rebuild is complete.
/// What: when the index has a durable corpus store, opens a fresh
/// `index.redb.tmp` and swaps it onto the indexer (so every
/// `commit_parsed_batch` writes the new corpus to the temp file). Returns the
/// staging store's path so the caller can finalize or discard it. Returns
/// `None` (skip staging) when the index has no corpus store (BM25-only / test
/// indexers) or the temp path can't be resolved.
async fn begin_force_corpus_swap(handle: &IndexHandle, index_id: &IndexId) -> Option<PathBuf> {
    {
        // Quick read-lock probe: nothing to stage if no durable corpus.
        if !handle.indexer.read().await.has_corpus_store() {
            return None;
        }
    }
    let tmp_path = match crate::service::persistence::corpus_redb_tmp_path(&index_id.0) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(
                "force reindex: cannot resolve staging corpus path for '{}' ({e}) — \
                 reindex will write directly to the live corpus",
                index_id.0
            );
            return None;
        }
    };
    // Open the staging store on a blocking worker (redb's API is sync).
    let tmp_for_open = tmp_path.clone();
    let staged = tokio::task::spawn_blocking(move || {
        crate::core::corpus::CorpusStore::open_fresh(&tmp_for_open)
    })
    .await;
    let staged = match staged {
        Ok(Ok(store)) => store,
        Ok(Err(e)) => {
            tracing::warn!(
                "force reindex: could not open staging corpus for '{}' ({e}) — \
                 reindex will write directly to the live corpus",
                index_id.0
            );
            return None;
        }
        Err(e) => {
            tracing::warn!(
                "force reindex: staging corpus open task panicked for '{}': {e}",
                index_id.0
            );
            return None;
        }
    };
    // Swap the staging store onto the indexer. The prior live store's `Arc` is
    // dropped here (the indexer held the only daemon-side clone); reads during
    // the reindex are served from the in-memory `chunks` HashMap, so dropping
    // the durable handle does not affect search.
    let mut indexer = handle.indexer.write().await;
    let _prev = indexer.swap_corpus_store(Arc::new(staged));
    drop(indexer);
    tracing::info!(
        "force reindex: staging rebuilt corpus for '{}' in {}",
        index_id.0,
        tmp_path.display()
    );
    Some(tmp_path)
}

/// Finalize (commit) the atomic corpus swap after a successful `--force`
/// reindex (issue #28, Phase 4).
///
/// Why: once the reindex has committed every batch to `index.redb.tmp`, the
/// temp file holds the complete rebuilt corpus. Renaming it over the live
/// `index.redb` makes the swap atomic — a search either sees the entire old
/// corpus or the entire new one, never a mix.
/// What: takes the staging store out of the indexer and drops its last `Arc`
/// (redb keeps the file mapped while any handle is alive, so the handle MUST
/// be dropped before the rename), renames `index.redb.tmp` → `index.redb`,
/// re-opens a `CorpusStore` on the swapped-in file, and installs it on the
/// indexer. Any failure leaves the previous live corpus in place and logs at
/// `warn` — a botched swap must not crash the daemon.
async fn commit_force_corpus_swap(handle: &IndexHandle, index_id: &IndexId, tmp_path: &Path) {
    let live_path = match crate::service::persistence::corpus_redb_path(&index_id.0) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(
                "force reindex: cannot resolve live corpus path for '{}' ({e}) — \
                 staged corpus left at {}",
                index_id.0,
                tmp_path.display()
            );
            return;
        }
    };
    // Drop the staging store's last Arc so redb releases the temp file before
    // the rename.
    {
        let mut indexer = handle.indexer.write().await;
        let _ = indexer.take_corpus_store();
    }
    let tmp = tmp_path.to_path_buf();
    let live = live_path.clone();
    let index_id_inner = index_id.0.clone();
    // rename + re-open on a blocking worker (filesystem + redb sync calls).
    let reopened = tokio::task::spawn_blocking(
        move || -> anyhow::Result<crate::core::corpus::CorpusStore> {
            std::fs::rename(&tmp, &live).with_context(|| {
                format!(
                    "atomic-swap rename {} -> {} for '{index_id_inner}'",
                    tmp.display(),
                    live.display()
                )
            })?;
            crate::core::corpus::CorpusStore::open(&live)
                .with_context(|| format!("re-open swapped corpus for '{index_id_inner}'"))
        },
    )
    .await;
    match reopened {
        Ok(Ok(store)) => {
            handle
                .indexer
                .write()
                .await
                .set_corpus_store(Arc::new(store));
            tracing::info!(
                "force reindex: atomically swapped rebuilt corpus into {} for '{}'",
                live_path.display(),
                index_id.0
            );
        }
        Ok(Err(e)) => tracing::warn!(
            "force reindex: atomic corpus swap failed for '{}' ({e}) — \
             previous corpus preserved; in-memory state is the rebuilt one",
            index_id.0
        ),
        Err(e) => tracing::warn!(
            "force reindex: atomic corpus swap task panicked for '{}': {e}",
            index_id.0
        ),
    }
}

/// Discard the staging corpus after an aborted / failed `--force` reindex
/// (issue #28, Phase 4).
///
/// Why: if the reindex aborts (memory limit) or fails, the partially-written
/// `index.redb.tmp` must not survive — the next `--force` reindex's
/// `open_fresh` would clear it anyway, but leaving multi-GB stale temp files
/// on disk between reindexes wastes space. The live `index.redb` is untouched
/// by an aborted reindex, so reverting just means deleting the temp and
/// re-opening the original live store.
/// What: takes the staging store out of the indexer, drops its `Arc`, deletes
/// `index.redb.tmp`, then re-opens and re-installs the live `index.redb` store
/// so the indexer's durable corpus points back at the untouched original.
async fn abort_force_corpus_swap(handle: &IndexHandle, index_id: &IndexId, tmp_path: &Path) {
    {
        let mut indexer = handle.indexer.write().await;
        let _ = indexer.take_corpus_store();
    }
    let live_path = crate::service::persistence::corpus_redb_path(&index_id.0);
    let tmp = tmp_path.to_path_buf();
    let index_id_inner = index_id.0.clone();
    let restored = tokio::task::spawn_blocking(
        move || -> anyhow::Result<Option<crate::core::corpus::CorpusStore>> {
            match std::fs::remove_file(&tmp) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => tracing::warn!(
                    "force reindex: could not delete staging corpus {} for '{index_id_inner}': {e}",
                    tmp.display()
                ),
            }
            match live_path {
                Ok(live) => Ok(Some(crate::core::corpus::CorpusStore::open(&live)?)),
                Err(e) => {
                    tracing::warn!(
                        "force reindex: cannot resolve live corpus path for '{index_id_inner}' \
                         ({e}) — index left without a durable corpus until next restart"
                    );
                    Ok(None)
                }
            }
        },
    )
    .await;
    match restored {
        Ok(Ok(Some(store))) => {
            handle
                .indexer
                .write()
                .await
                .set_corpus_store(Arc::new(store));
            tracing::warn!(
                "force reindex: aborted — discarded staging corpus and restored the \
                 original durable corpus for '{}'",
                index_id.0
            );
        }
        Ok(Ok(None)) => {}
        Ok(Err(e)) => tracing::warn!(
            "force reindex: could not restore the original corpus for '{}' after abort ({e})",
            index_id.0
        ),
        Err(e) => tracing::warn!(
            "force reindex: corpus-restore task panicked for '{}': {e}",
            index_id.0
        ),
    }
}

/// Variant of `spawn_reindex` that GC's the progress map after completion.
/// See `spawn_reindex` for the rationale.
pub fn spawn_reindex_with_cleanup(
    handle: Arc<IndexHandle>,
    progress: Arc<ReindexProgress>,
    force: bool,
    cleanup_map: Option<Arc<DashMap<IndexId, Arc<ReindexProgress>>>>,
    aborted_map: Option<Arc<DashMap<IndexId, Instant>>>,
    scorer_cache: Option<GraphScorerCache>,
) {
    let cleanup_id = handle.id.clone();
    tokio::spawn(async move {
        use std::sync::atomic::Ordering;

        // Serialize reindexes machine-wide to avoid stacking multiple
        // simultaneous ONNX embedder sessions (Jetsam kill at ~28GB on macOS).
        // Late arrivals queue here; their SSE stream is already attached and
        // will replay buffered events once the permit is acquired.
        let _permit = reindex_semaphore()
            .acquire()
            .await
            .expect("reindex semaphore is never closed");

        let started = Instant::now();
        let root = handle.root_path.clone();
        let index_id: IndexId = handle.id.clone();

        // Phase 1: walk + filter the source tree (helper-extracted: issue #98).
        let walk = collect_files_to_index(&handle);
        let total = walk.files.len();
        progress.total_files.store(total, Ordering::Release);
        progress
            .push(serde_json::json!({
                "event": "start",
                "total_files": total,
                "index_id": index_id.0,
                "root_path": root,
                "force": force,
            }))
            .await;

        let hashes = hashes_for(&index_id);
        // `--force` wipes the per-index content-hash cache so every file is
        // re-parsed, re-embedded, and re-committed even if its bytes haven't
        // changed since the last reindex in this daemon's lifetime. Without
        // this, the hash-skip check below silently turns `--force` into a
        // no-op on a warm daemon.
        if force {
            hashes.clear();
        }

        // Issue #28, Phase 4: a `--force` reindex stages its rebuilt corpus in
        // a sibling `index.redb.tmp`. Every `commit_parsed_batch` below writes
        // the new corpus to the staging file; the live `index.redb` is left
        // untouched until the reindex completes, at which point the staging
        // file is atomically renamed over it. `None` when staging was skipped
        // (non-force reindex, BM25-only index, or unresolvable temp path) — in
        // that case commits write directly to the live corpus, exactly as
        // before Phase 4.
        let corpus_swap_tmp: Option<PathBuf> = if force {
            begin_force_corpus_swap(&handle, &index_id).await
        } else {
            None
        };

        // Per-subsystem timing accumulators. Each phase (parse, embed, BM25,
        // vector upsert) is measured inside the indexer (see `ParsedBatch` /
        // `CommitTimings`) and summed across all batches here. KG is measured
        // separately at the end. Together with `vector_count`, this gives
        // operators per-subsystem visibility — and crucially, a non-zero
        // `embed_ms` with `vector_count == 0` is the smoking-gun signal for the
        // "embedder silently fell back to BM25" failure mode.
        let mut total_parse_ms: u64 = 0;
        let mut total_embed_ms: u64 = 0;
        let mut total_bm25_ms: u64 = 0;
        let mut total_vector_upsert_ms: u64 = 0;
        let mut total_vector_count: usize = 0;

        // Memory-protection state (issues #76, #82). `mem_limit` is `Some`
        // only when `TRUSTY_MEMORY_LIMIT_MB` is set. The previous design
        // sampled RSS every 10 batches *before* parse/embed/commit, which let
        // a single batch push RSS 4× over the configured limit before being
        // noticed (issue #82: 10 GB limit → 40 GB actual).
        //
        // The new design has three layers of protection:
        //   1. A background poller task (spawned below) samples RSS every
        //      `MEM_POLL_INTERVAL` and sets `mem_abort` the moment the limit
        //      is breached. This catches mid-batch spikes that batch-boundary
        //      checks miss.
        //   2. The main loop checks `mem_abort` on EVERY batch (not every 10)
        //      and also AFTER `commit_parsed_batch` returns, so the largest
        //      allocator (HNSW + redb commit) is bracketed by checks.
        //   3. The KG rebuild at the end also honours the abort flag.
        //
        // `peak_rss_mb` is updated by the poller via an atomic so the final
        // log line reflects the true peak, not just batch-boundary samples.
        // Use the indexing-pipeline-specific limit (TRUSTY_INDEX_MEMORY_LIMIT_MB)
        // rather than the global daemon ceiling. The pipeline's working set —
        // ONNX session + transient ORT arena + CoreML unified-memory buffers
        // on Apple Silicon — has a very different (typically much larger)
        // memory footprint than the steady-state daemon. Falling back to the
        // global limit is automatic when the indexing env var is unset, so
        // operators who haven't opted in see no behavioural change.
        let mem_limit = index_memory_limit_mb();
        let mem_abort = Arc::new(AtomicBool::new(false));
        let peak_rss_atomic = Arc::new(AtomicU64::new(current_rss_mb().unwrap_or(0)));
        let mut mem_limit_hit: bool = false;

        // Spawn the background poller (helper-extracted: issue #98). Runs
        // until `poller_stop` flips, updating `peak_rss_atomic` and tripping
        // `mem_abort` whenever RSS crosses `mem_limit`.
        let (poller_handle, poller_stop) = spawn_memory_poller(
            mem_limit,
            mem_abort.clone(),
            peak_rss_atomic.clone(),
            index_id.0.clone(),
        );

        // Phase 2: pipelined parse/embed/commit (issue #20).
        //
        // Producer task (spawned below) walks the file batches, calls
        // `prepare_and_parse_batch` (file reads + tree-sitter parse + ONNX
        // embed — no write lock), and sends each `ParsedReadyBatch` over a
        // bounded mpsc channel. The consumer loop here drains the channel
        // and calls `commit_parsed_and_finalize` (write lock — BM25 + HNSW +
        // redb commit), which must stay sequential.
        //
        // Why this pipelines: the read+parse stage uses zero indexer write
        // locks, so it can race ahead while the previous batch's commit
        // still holds the write lock. Channel capacity 1 caps in-flight
        // memory at two batches (one being committed, one buffered) — the
        // same envelope the previous sequential loop already paid for.
        let ctx = BatchCtx {
            handle: handle.clone(),
            progress: progress.clone(),
            root: root.clone(),
            index_id: index_id.clone(),
            hashes: hashes.clone(),
            mem_limit,
            mem_abort: mem_abort.clone(),
            peak_rss_atomic: peak_rss_atomic.clone(),
            started,
            total,
        };

        // Snapshot the batch list into owned `Vec<PathBuf>`s so the producer
        // task can outlive the borrow on `walk.files`. Memory cost is one
        // `PathBuf` per file (already paid by `walk`).
        let batches: Vec<Vec<PathBuf>> = walk
            .files
            .chunks(REINDEX_BATCH_SIZE)
            .map(|b| b.to_vec())
            .collect();

        // Bounded channel — capacity 1 keeps memory usage in the same
        // envelope as the prior sequential loop (one batch in transit, one
        // being committed).
        let (tx, mut rx) = mpsc::channel::<ParsedReadyBatch>(1);
        let producer_ctx = ctx.clone();
        let producer_mem_abort = mem_abort.clone();
        let producer_index_id = index_id.0.clone();
        let producer = tokio::spawn(async move {
            for batch in batches {
                // Honour the mem-abort flag at the producer too so we stop
                // reading/parsing as soon as the consumer (or the memory
                // poller) trips it.
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
                // If the consumer has dropped the receiver (e.g. an earlier
                // commit tripped mem-abort and we broke out of the loop),
                // stop producing.
                if tx.send(ready).await.is_err() {
                    break;
                }
            }
            // Dropping `tx` here signals end-of-stream to the consumer.
        });

        // Consumer loop: commits batches sequentially as the producer feeds
        // them. The commit phase holds the indexer write lock; the producer
        // task is concurrently running read+parse for the next batch.
        while let Some(ready) = rx.recv().await {
            let outcome = commit_parsed_and_finalize(&ctx, ready).await;
            total_parse_ms = total_parse_ms.saturating_add(outcome.parse_ms);
            total_embed_ms = total_embed_ms.saturating_add(outcome.embed_ms);
            total_bm25_ms = total_bm25_ms.saturating_add(outcome.bm25_ms);
            total_vector_upsert_ms =
                total_vector_upsert_ms.saturating_add(outcome.vector_upsert_ms);
            total_vector_count = total_vector_count.saturating_add(outcome.vector_count);
            if outcome.mem_limit_hit {
                mem_limit_hit = true;
                // Close the receiver so the producer notices on its next
                // `send()` and halts; then drain any already-sent batch so
                // the producer task doesn't block on send forever.
                rx.close();
                while rx.recv().await.is_some() {}
                break;
            }
        }
        // Best-effort: the producer should already have terminated (either
        // by running out of batches, by observing `mem_abort`, or by the
        // receiver being closed). Awaiting it surfaces panics for tracing.
        let _ = producer.await;

        // Issue #29: the per-batch HNSW snapshot is throttled to one every
        // `HNSW_SNAPSHOT_BATCH_INTERVAL` batches, so the most recent ≤15
        // batches' vectors may not be on disk yet. Force one final snapshot
        // now — while the live HNSW store is still wired and before the
        // `--force` corpus swap drops the durable corpus handle — so a crash
        // before the next reindex never loses the tail of the rebuild.
        {
            let indexer = handle.indexer.read().await;
            indexer.force_incremental_persist();
        }

        // Issue #28, Phase 4: finalize the atomic corpus swap. Every batch
        // committed above wrote to `index.redb.tmp`; now that the batch loop
        // is done we either atomically rename it over the live `index.redb`
        // (clean completion) or discard it and restore the original
        // (memory-abort). Done before the KG rebuild so the durable corpus is
        // settled regardless of how the rebuild fares.
        if let Some(tmp_path) = &corpus_swap_tmp {
            let aborted = mem_limit_hit || mem_abort.load(AtomicOrdering::Acquire);
            if aborted {
                abort_force_corpus_swap(&handle, &index_id, tmp_path).await;
            } else {
                commit_force_corpus_swap(&handle, &index_id, tmp_path).await;
            }
        }

        // Phase 3: rebuild the symbol graph once for the whole reindex.
        // Issue #90: always run, even after a memory abort — graph
        // construction is bounded by `TRUSTY_MAX_KG_NODES` and independent of
        // the embedding spike. See `rebuild_symbol_graph_for_reindex`.
        let kg = rebuild_symbol_graph_for_reindex(&handle).await;
        // Issue #41 phase 3: run Louvain community detection on the freshly
        // rebuilt KG so agents can query topology. Fire-and-forget on a
        // blocking thread: detection can take seconds on a 100k-node graph
        // and we don't want it to delay the `complete` SSE event. A failure
        // is logged at `warn` — searches still work, they just won't carry a
        // `community_id` until the next successful reindex.
        spawn_community_detection(&handle, scorer_cache.clone());
        if mem_limit_hit || mem_abort.load(AtomicOrdering::Acquire) {
            tracing::warn!(
                "reindex: memory limit was breached during batch processing for \
                 index {} (peak_rss={}MB, limit={:?}MB) — KG was still rebuilt \
                 (symbols={}, edges={}) because graph construction is bounded by \
                 TRUSTY_MAX_KG_NODES and independent of the embedding spike",
                index_id.0,
                peak_rss_atomic.load(AtomicOrdering::Acquire),
                mem_limit,
                kg.symbol_count,
                kg.edge_count,
            );
        }

        // Stop the background poller and collect the true peak it observed.
        poller_stop.store(true, AtomicOrdering::Release);
        // Best-effort: don't fail completion if the poller is wedged.
        let _ = poller_handle.await;

        // Issue #120: distinguish memory-abort from clean completion so the
        // HTTP reindex_handler can apply a cooldown before honouring the next
        // reindex request. Also record the abort timestamp on the shared map
        // so the cooldown survives across handler invocations.
        let aborted_memory = mem_limit_hit || mem_abort.load(AtomicOrdering::Acquire);
        if aborted_memory {
            progress.status.store(ReindexStatus::AbortedMemory);
            if let Some(map) = aborted_map.as_ref() {
                map.insert(index_id.clone(), Instant::now());
            }
        } else {
            progress.status.store(ReindexStatus::Complete);
            // Issue #75: refresh the captured HEAD SHA so subsequent searches
            // compare against the commit we just indexed against. Best-effort:
            // a `None` (non-git workdir / missing git binary) silently clears
            // any previously cached value, which keeps the stale flag honest
            // rather than reporting freshness we can no longer verify.
            let new_sha = crate::core::git::head_sha(&handle.root_path);
            *handle.indexed_head_sha.write().await = new_sha;
        }

        // Final synchronous RSS poll so the peak reflects post-KG memory
        // (the symbol graph rebuild may itself push RSS higher than any
        // background sample taken before it ran).
        if let Some(rss) = current_rss_mb() {
            let prev = peak_rss_atomic.load(AtomicOrdering::Acquire);
            if rss > prev {
                peak_rss_atomic.store(rss, AtomicOrdering::Release);
            }
        }
        let peak_rss_mb = peak_rss_atomic.load(AtomicOrdering::Acquire);
        let indexed_final = progress.indexed.load(Ordering::Acquire);
        let total_chunks = progress.total_chunks.load(Ordering::Acquire);
        let elapsed_ms = started.elapsed().as_millis() as u64;
        tracing::info!(
            "reindex complete: index={} files={} chunks={} elapsed_ms={} \
             peak_rss_mb={} memory_limit_hit={}",
            index_id.0,
            indexed_final,
            total_chunks,
            elapsed_ms,
            peak_rss_mb,
            mem_limit_hit,
        );

        let totals = RunTotals {
            parse_ms: total_parse_ms,
            embed_ms: total_embed_ms,
            bm25_ms: total_bm25_ms,
            vector_upsert_ms: total_vector_upsert_ms,
            vector_count: total_vector_count,
            mem_limit_hit,
        };
        emit_complete_event(&progress, started, peak_rss_mb, &totals, &kg).await;

        // Issue #112: refresh the per-index context embedding from the
        // root-level metadata files. Best-effort — failure here is logged
        // and the handle's `context_embedding` stays `None`, which the
        // fan-out router treats as a neutral 1.0 weight.
        refresh_context_embedding(&handle).await;

        // Issue #75: GC the progress entry after a short delay
        // (helper-extracted: issue #98).
        schedule_progress_cleanup(cleanup_map, cleanup_id);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::indexer::CodeIndexer;
    use std::fs;
    use std::sync::atomic::Ordering;

    /// Issue #81: `spawn_community_detection` must evict a cached
    /// `GraphScorer` once it has persisted the community partition, so a
    /// scorer that was cached with no communities (e.g. built by a search
    /// that raced the reindex) is rebuilt with the fresh partition on the
    /// next lookup — restoring a non-zero `community_cohesion`.
    ///
    /// Why: the reindex handler invalidates the scorer cache at *queue* time,
    /// but Louvain detection runs on a detached task that finishes *later*.
    /// Any scorer cached in that window held an empty `community_map`, so
    /// cohesion was permanently stuck at 0.0 until the next reindex. This test
    /// pins the post-detection eviction.
    /// What: builds a corpus-backed indexer whose symbol graph is two cliques
    /// (so Louvain finds ≥1 community), seeds the scorer cache with a stale
    /// entry, runs `spawn_community_detection`, waits for the persisted
    /// partition, and asserts (a) the stale cache entry was evicted and (b) a
    /// scorer rebuilt from the now-persisted communities reports members in
    /// the same community.
    /// Test: this test.
    #[tokio::test]
    async fn community_detection_invalidates_scorer_cache() {
        use crate::core::chunker::{ChunkType, RawChunk};
        use crate::core::corpus::CorpusStore;
        use crate::core::indexer::graph_score::GraphScorer;

        fn clique_chunk(id: &str, sym: &str, calls: &[&str]) -> RawChunk {
            RawChunk {
                id: id.to_string(),
                file: "a.rs".to_string(),
                start_line: 1,
                end_line: 2,
                content: format!("fn {sym}() {{}}"),
                function_name: Some(sym.to_string()),
                language: Some("rust".to_string()),
                chunk_type: ChunkType::Code,
                calls: calls.iter().map(|s| s.to_string()).collect(),
                inherits_from: Vec::new(),
                chunk_depth: 0,
                parent_chunk_id: None,
                child_chunk_ids: Vec::new(),
                nlp_keywords: Vec::new(),
                nlp_code_refs: Vec::new(),
                virtual_terms: Vec::new(),
            }
        }

        let tmp = tempfile::tempdir().expect("tempdir");
        let redb = tmp.path().join("index.redb");
        let corpus = Arc::new(CorpusStore::open(&redb).expect("open corpus"));

        let mut indexer = CodeIndexer::new("cohesion-test".to_string(), tmp.path());
        indexer.set_corpus_store(Arc::clone(&corpus));
        // Two cliques joined by a single bridge edge (a3 → b1). add_chunk
        // rebuilds the symbol graph each call, so after the last insert the
        // graph is fully populated and `symbol_for_chunk` resolves.
        for c in [
            clique_chunk("a:1", "a1", &["a2", "a3"]),
            clique_chunk("a:2", "a2", &["a1", "a3"]),
            clique_chunk("a:3", "a3", &["a1", "a2", "b1"]),
            clique_chunk("b:1", "b1", &["b2", "b3"]),
            clique_chunk("b:2", "b2", &["b1", "b3"]),
            clique_chunk("b:3", "b3", &["b1", "b2"]),
        ] {
            indexer.add_chunk(c).await.expect("add_chunk");
        }

        let handle = Arc::new(IndexHandle::bare(
            IndexId::new("cohesion-test"),
            Arc::new(tokio::sync::RwLock::new(indexer)),
            tmp.path().to_path_buf(),
        ));

        // Seed the cache with a STALE scorer built from no communities — this
        // mimics a search that raced the reindex. `same_community` would
        // return false for every pair, yielding cohesion 0.0.
        let cache: GraphScorerCache = Arc::new(DashMap::new());
        let stale = {
            let idx = handle.indexer.read().await;
            let graph = idx.snapshot_symbol_graph().await;
            Arc::new(GraphScorer::build(&graph, &[]))
        };
        assert!(
            !stale.same_community("a1", "a2"),
            "pre-condition: stale scorer (no communities) must report no cohesion"
        );
        cache.insert(IndexId::new("cohesion-test"), Arc::clone(&stale));

        // Run detection with the cache handle; it persists communities and
        // must evict the stale entry.
        spawn_community_detection(&handle, Some(Arc::clone(&cache)));

        // Wait for the detached task to persist + invalidate.
        let mut evicted = false;
        for _ in 0..100 {
            if cache.get(&IndexId::new("cohesion-test")).is_none()
                && corpus
                    .load_communities()
                    .map(|v| !v.is_empty())
                    .unwrap_or(false)
            {
                evicted = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(
            evicted,
            "community detection must persist communities and evict the stale scorer"
        );

        // Rebuilding the scorer from the now-persisted partition must restore
        // same-community detection (non-zero cohesion).
        let communities: Vec<_> = corpus
            .load_communities()
            .expect("load")
            .into_iter()
            .filter_map(|(_, bytes)| {
                serde_json::from_slice::<crate::core::community::CommunityRecord>(&bytes).ok()
            })
            .collect();
        let graph = handle.indexer.read().await.snapshot_symbol_graph().await;
        let fresh = GraphScorer::build(&graph, &communities);
        assert!(
            fresh.same_community("a1", "a2"),
            "rebuilt scorer must report a-clique members in the same community \
             (non-zero cohesion, issue #81)"
        );
    }

    /// Filter wiring: with `include_paths` set on the handle, the reindex
    /// must walk ONLY those subtrees. Files outside the configured slice
    /// must not appear in the corpus.
    ///
    /// Why: `trusty-search.yaml` declares `paths: [api/src]` to slice a
    /// polyrepo. Without this test, a regression that drops the
    /// `handle.include_paths` branch silently reverts to "walk everything",
    /// which is the bug the YAML config exists to avoid.
    /// What: stage a fixture with `api/keep.rs` and `ui/drop.rs`, register a
    /// handle whose `include_paths = [<root>/api]`, run the reindex, and
    /// assert only the api file was indexed.
    /// Test: this test.
    #[tokio::test]
    async fn reindex_honours_include_paths_filter() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        fs::create_dir_all(root.join("api")).unwrap();
        fs::create_dir_all(root.join("ui")).unwrap();
        fs::write(root.join("api/keep.rs"), "fn keep_me() {}\n").unwrap();
        fs::write(root.join("ui/drop.rs"), "fn drop_me() {}\n").unwrap();

        let indexer = CodeIndexer::new("filter-test", root.clone());
        let handle = Arc::new(IndexHandle {
            id: IndexId::new("filter-test"),
            indexer: Arc::new(tokio::sync::RwLock::new(indexer)),
            root_path: root.clone(),
            include_paths: vec![root.join("api")],
            exclude_globs: vec![],
            extensions: vec![],
            domain_terms: vec![],
            include_docs: false,
            path_filter: vec![],
            context_embedding: Arc::new(tokio::sync::RwLock::new(None)),
            context_summary: Arc::new(tokio::sync::RwLock::new(None)),
            indexed_head_sha: Arc::new(tokio::sync::RwLock::new(None)),
        });
        let progress = Arc::new(ReindexProgress::new());
        spawn_reindex(handle.clone(), progress.clone(), false);

        // Wait up to 10s for completion.
        for _ in 0..100 {
            if progress.status.load() == ReindexStatus::Complete {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert_eq!(progress.status.load(), ReindexStatus::Complete);
        assert_eq!(
            progress.total_files.load(Ordering::Acquire),
            1,
            "only api/keep.rs should be walked"
        );

        // And the corpus must contain `keep_me` but not `drop_me`.
        let idx = handle.indexer.read().await;
        let r = idx
            .search(&crate::core::indexer::SearchQuery {
                text: "keep_me".into(),
                top_k: 5,
                expand_graph: false,
                compact: false,
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(r.iter().any(|c| c.content.contains("keep_me")));
        let r2 = idx
            .search(&crate::core::indexer::SearchQuery {
                text: "drop_me".into(),
                top_k: 5,
                expand_graph: false,
                compact: false,
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(
            !r2.iter().any(|c| c.content.contains("drop_me")),
            "ui/drop.rs must not have been indexed"
        );
    }

    /// Issue #111 end-to-end: with `path_filter = ["common-*"]`, the reindex
    /// must include files inside `common-utils/` but exclude `other-repo/`.
    /// Uses the BM25-only path (no embedder needed) for hermetic execution.
    #[tokio::test]
    async fn reindex_honours_path_filter() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        std::fs::create_dir_all(root.join("common-utils")).unwrap();
        std::fs::create_dir_all(root.join("other-repo")).unwrap();
        std::fs::write(root.join("common-utils/keep.rs"), "fn keep_common() {}\n").unwrap();
        std::fs::write(root.join("other-repo/drop.rs"), "fn drop_other() {}\n").unwrap();

        let indexer = CodeIndexer::new("pf-test", root.clone());
        let handle = Arc::new(IndexHandle {
            id: IndexId::new("pf-test"),
            indexer: Arc::new(tokio::sync::RwLock::new(indexer)),
            root_path: root.clone(),
            include_paths: vec![],
            exclude_globs: vec![],
            extensions: vec![],
            domain_terms: vec![],
            include_docs: false,
            path_filter: vec!["common-*".to_string()],
            context_embedding: Arc::new(tokio::sync::RwLock::new(None)),
            context_summary: Arc::new(tokio::sync::RwLock::new(None)),
            indexed_head_sha: Arc::new(tokio::sync::RwLock::new(None)),
        });
        let progress = Arc::new(ReindexProgress::new());
        spawn_reindex(handle.clone(), progress.clone(), false);

        for _ in 0..100 {
            if progress.status.load() == ReindexStatus::Complete {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert_eq!(progress.status.load(), ReindexStatus::Complete);
        assert_eq!(
            progress.total_files.load(Ordering::Acquire),
            1,
            "only common-utils/keep.rs should pass the path_filter"
        );

        let idx = handle.indexer.read().await;
        let r = idx
            .search(&crate::core::indexer::SearchQuery {
                text: "keep_common".into(),
                top_k: 5,
                expand_graph: false,
                compact: false,
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(r.iter().any(|c| c.content.contains("keep_common")));
        let r2 = idx
            .search(&crate::core::indexer::SearchQuery {
                text: "drop_other".into(),
                top_k: 5,
                expand_graph: false,
                compact: false,
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(
            !r2.iter().any(|c| c.content.contains("drop_other")),
            "other-repo must not have been indexed"
        );
    }

    #[tokio::test]
    async fn reindex_walks_directory_and_emits_events() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        fs::write(root.join("a.rs"), "fn a() {}").unwrap();
        fs::write(root.join("b.py"), "def b():\n    pass\n").unwrap();
        fs::create_dir(root.join("target")).unwrap();
        fs::write(root.join("target/skip.rs"), "fn skip() {}").unwrap();

        let indexer = CodeIndexer::new("test".to_string(), root.clone());
        let handle = Arc::new(IndexHandle::bare(
            IndexId::new("test"),
            Arc::new(tokio::sync::RwLock::new(indexer)),
            root.clone(),
        ));
        let progress = Arc::new(ReindexProgress::new());
        spawn_reindex(handle, progress.clone(), false);

        // Wait up to 10s for completion.
        for _ in 0..100 {
            if progress.status.load() == ReindexStatus::Complete {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert_eq!(progress.status.load(), ReindexStatus::Complete);
        assert_eq!(progress.total_files.load(Ordering::Acquire), 2);
        assert_eq!(progress.indexed.load(Ordering::Acquire), 2);

        let events = progress.events.lock().await;
        assert!(events
            .first()
            .map(|s| s.contains("\"start\""))
            .unwrap_or(false));
        assert!(events
            .last()
            .map(|s| s.contains("\"complete\""))
            .unwrap_or(false));
    }

    /// Issue #112: after a reindex completes, the handle's
    /// `context_embedding` and `context_summary` must be populated when
    /// recognised metadata files exist in `root_path`. Uses a `MockEmbedder`
    /// so the test is fully hermetic.
    #[tokio::test]
    async fn context_embedding_populated_after_reindex() {
        use crate::core::embed::{Embedder, MockEmbedder};
        use crate::core::store::{UsearchStore, VectorStore};

        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        // Stage a source file plus a README so the metadata scraper has
        // something to embed.
        fs::write(root.join("lib.rs"), "fn hello() {}\n").unwrap();
        fs::write(
            root.join("README.md"),
            "# proj\n\nA test project for #112.\n",
        )
        .unwrap();

        let dim = 32;
        let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(dim));
        let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(dim).expect("usearch new"));
        let indexer = CodeIndexer::new("ctx-test", root.clone()).with_components(embedder, store);

        let handle = Arc::new(IndexHandle::bare(
            IndexId::new("ctx-test"),
            Arc::new(tokio::sync::RwLock::new(indexer)),
            root.clone(),
        ));
        let progress = Arc::new(ReindexProgress::new());
        spawn_reindex(handle.clone(), progress.clone(), false);

        for _ in 0..100 {
            if progress.status.load() == ReindexStatus::Complete {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert_eq!(progress.status.load(), ReindexStatus::Complete);

        let ctx = handle.context_embedding.read().await.clone();
        assert!(
            ctx.is_some(),
            "context_embedding must be populated when metadata is present and embedder is wired"
        );
        assert_eq!(ctx.unwrap().len(), dim, "embedding must have embedder dim");

        let summary = handle.context_summary.read().await.clone();
        assert!(summary.is_some(), "context_summary must be populated");
        let s = summary.unwrap();
        assert!(s.contains("proj") || s.contains("README"));
    }

    /// Issue #112: when no recognised metadata files exist, the context
    /// embedding stays `None` so the router falls back to a neutral 1.0
    /// weight for this index.
    #[tokio::test]
    async fn context_embedding_none_when_no_metadata() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        // Only a source file — no README, no Cargo.toml, etc.
        fs::write(root.join("lib.rs"), "fn hello() {}\n").unwrap();

        let indexer = CodeIndexer::new("no-meta", root.clone());
        let handle = Arc::new(IndexHandle::bare(
            IndexId::new("no-meta"),
            Arc::new(tokio::sync::RwLock::new(indexer)),
            root.clone(),
        ));
        let progress = Arc::new(ReindexProgress::new());
        spawn_reindex(handle.clone(), progress.clone(), false);

        for _ in 0..100 {
            if progress.status.load() == ReindexStatus::Complete {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert_eq!(progress.status.load(), ReindexStatus::Complete);
        assert!(handle.context_embedding.read().await.is_none());
        assert!(handle.context_summary.read().await.is_none());
    }
}
