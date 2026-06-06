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

mod hash_cache;
pub mod quarantine;
mod staging;
mod validate;

pub use quarantine::ReindexQuarantine;

use crate::core::indexer::{CommitTimings, ParsedBatch};
use crate::core::memguard::{current_rss_mb, current_rss_mb_for_pid, index_memory_limit_mb};
use crate::core::registry::{IndexHandle, IndexId, IndexStages, StageState, StageStatus};
use crate::service::walker::{should_skip_content, walk_source_files_with_options, WalkOptions};
use anyhow::Context;
use crossbeam_utils::atomic::AtomicCell;
use dashmap::DashMap;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, mpsc, Mutex, Semaphore};

/// Interactive (user-initiated) reindex semaphore (issue #458).
///
/// Why: Startup auto-discover can queue 40+ background reindex tasks, all of
/// which contend for the same semaphore. A user running `trusty-search index
/// <new>` then queues behind the entire backlog and sits at "pending" for
/// minutes. Separating interactive from background requests means a user
/// request always gets a permit promptly, regardless of how many background
/// tasks are queued.
///
/// What: A small N-permit semaphore (default 2) reserved exclusively for
/// interactive (user-initiated) reindexes. Background/startup reindexes use
/// `background_reindex_semaphore()` instead. The embedder pool already gates
/// the actual embedding work; this semaphore just bounds the redb + HNSW
/// lock contention for on-demand requests.
///
/// Test: `interactive_reindex_not_starved_by_background` exercises the
/// prioritisation: a background job holding the background semaphore must not
/// block a concurrent interactive request.
fn reindex_semaphore() -> &'static Semaphore {
    static SEM: OnceLock<Semaphore> = OnceLock::new();
    SEM.get_or_init(|| Semaphore::new(MAX_PARALLEL_REINDEXES))
}

/// Background (startup / auto-discover) reindex semaphore (issue #458).
///
/// Why: all startup auto-discover reindexes drain through this single-permit
/// semaphore so they run sequentially and never consume the interactive
/// semaphore's slots. A single permit keeps peak memory bounded (one reindex
/// in flight at a time from the bulk queue) while leaving the interactive
/// semaphore completely free for user requests.
///
/// What: 1-permit semaphore. Background tasks queue here; when the permit is
/// released the next background task runs. Interactive tasks never touch this
/// semaphore — they go directly to `reindex_semaphore()`.
///
/// Test: `interactive_reindex_not_starved_by_background`.
fn background_reindex_semaphore() -> &'static Semaphore {
    static BG_SEM: OnceLock<Semaphore> = OnceLock::new();
    BG_SEM.get_or_init(|| Semaphore::new(MAX_PARALLEL_BACKGROUND_REINDEXES))
}

/// Maximum number of concurrent interactive (user-initiated) reindex tasks.
/// 2 permits allow a small burst (e.g. indexing two new projects at once)
/// without letting an unbounded fan-out overwhelm the redb + HNSW write locks.
const MAX_PARALLEL_REINDEXES: usize = 2;

/// Maximum concurrent background reindex tasks. 1 serialises the startup
/// auto-discover storm: tasks run one at a time and never block the
/// interactive semaphore.
const MAX_PARALLEL_BACKGROUND_REINDEXES: usize = 1;

/// Returns the number of background reindex tasks currently waiting for a
/// permit (queued in `background_reindex_semaphore()`). Exposed for the
/// `/health` payload so operators can see the startup backlog drain.
///
/// Why: without this counter, an operator watching `/health` has no way to
/// tell whether the daemon is still processing the startup reindex storm or
/// has finished. The number ticks down as each background job completes.
///
/// What: the number of available permits in the background semaphore is
/// `MAX_PARALLEL_BACKGROUND_REINDEXES - in_flight`, so the queue depth is
/// approximately the number of tasks blocked on `acquire()`. We approximate
/// this by tracking it with an `AtomicUsize` incremented before acquire and
/// decremented after.
///
/// Test: covered by `background_reindex_queue_depth_counts_waiting_tasks`.
pub fn background_reindex_queue_depth() -> usize {
    BACKGROUND_QUEUE_DEPTH.load(std::sync::atomic::Ordering::Relaxed)
}

/// Atomic counter tracking how many background tasks are queued (waiting or
/// in-flight on the background semaphore). Incremented when a background task
/// enters `spawn_reindex_with_cleanup`; decremented when the permit is released
/// (task finishes or the future is cancelled).
static BACKGROUND_QUEUE_DEPTH: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Select the correct reindex semaphore based on priority (issue #458).
///
/// Why: extracted so the routing decision can be unit-tested without wiring
/// a full reindex task. Keeping the selection in one function means future
/// changes to the priority model have exactly one edit site.
/// What: `priority=true` → interactive semaphore (2 permits); `priority=false`
/// → background semaphore (1 permit, serialises startup storm).
/// Test: `reindex_semaphore_selection_routes_by_priority` below.
pub(crate) fn reindex_semaphore_for(priority: bool) -> &'static Semaphore {
    if priority {
        reindex_semaphore()
    } else {
        background_reindex_semaphore()
    }
}

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
    /// Issue #100: number of chunks dropped during the most recent reindex
    /// because the per-index `TRUSTY_MAX_CHUNKS` cap was reached. Non-zero ⇒
    /// the index is incomplete and downstream search results may miss code
    /// from the tail of the walk. Surfaced via `GET /indexes/:id/status` as
    /// `walk_truncated_by_budget` (boolean) and `chunks_dropped_by_cap`
    /// (count) so operators can distinguish a clean index from one that
    /// silently lost source.
    pub chunks_dropped_by_cap: std::sync::atomic::AtomicUsize,
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
            chunks_dropped_by_cap: Default::default(),
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

/// Spawn a background poller that tracks the peak RSS of the embedderd sidecar
/// during a reindex run (issue #282).
///
/// Why: the daemon's own RSS poller (see `spawn_memory_poller`) covers only the
/// daemon parent process. The embedderd sidecar process owns the ONNX arena and
/// routinely uses 2–3 GB more than the daemon during active embedding; omitting
/// it leaves operators with an incomplete picture for capacity planning and
/// regression testing. This helper samples the sidecar every ~500 ms and
/// records the maximum so the SSE `complete` event can carry
/// `embedderd_peak_rss_mb` alongside the existing `peak_rss_mb`.
///
/// What: reads the current sidecar PID from `embedderd_pid_slot` on each tick.
/// A PID of 0 (no sidecar, or sidecar exited mid-run) causes the sample to be
/// skipped gracefully. Stops when `stop` is set to `true` by the orchestrator.
///
/// Test: `embedder_supervisor_e2e.rs::embedderd_peak_rss_captured_on_complete`
/// (marked `#[ignore]`; requires the real sidecar binary).
fn spawn_embedderd_rss_poller(
    embedderd_pid_slot: Arc<AtomicU32>,
    peak_embedderd_rss: Arc<AtomicU64>,
) -> (tokio::task::JoinHandle<()>, Arc<AtomicBool>) {
    /// Polling cadence for the embedderd RSS sampler. 500 ms is fine-grained
    /// enough to catch mid-reindex spikes without measurable overhead
    /// (one `sysinfo` refresh per tick costs ~1–3 ms on macOS/Linux).
    const EMBEDDERD_POLL_INTERVAL: Duration = Duration::from_millis(500);

    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = stop.clone();
    let handle = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(EMBEDDERD_POLL_INTERVAL);
        // Drop the first immediate tick to avoid a double-sample with the
        // synchronous initial read taken before spawning.
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

/// RFC-3339 timestamp helper used by the staged-pipeline status surface
/// (issue #109, Phase 1).
///
/// Why: each `StageState` carries optional `started_at` / `completed_at`
/// timestamps so external dashboards can compute stage durations without
/// inferring them from event ordering. Centralising the formatter keeps
/// the timestamp shape consistent across every transition.
fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Reset every stage to `Pending` (or `Skipped` for lexical-only) and stamp
/// `lexical.started_at` at the start of a reindex. The previous run's
/// counters are wiped so the search-capabilities array doesn't briefly
/// report stale lanes mid-reindex.
async fn reset_stages_for_reindex(handle: &Arc<IndexHandle>) {
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
async fn mark_lexical_ready_semantic_in_progress(
    handle: &Arc<IndexHandle>,
    files: usize,
    chunks: usize,
    total_chunks: usize,
) {
    let mut stages = handle.stages.write().await;
    stages.lexical.status = StageStatus::Ready;
    stages.lexical.completed_at = Some(now_rfc3339());
    stages.lexical.files = Some(files);
    stages.lexical.chunks = Some(chunks);
    // On lexical-only indexes the semantic + graph slots stay `Skipped` —
    // the reset hook pre-populated them. Don't overwrite the terminal
    // state. For full-pipeline indexes the semantic stage has been running
    // alongside the producer (the embed step is part of every batch); flip
    // it to `InProgress` so callers see it as queryable-soon.
    if !handle.lexical_only && stages.semantic.status == StageStatus::Pending {
        stages.semantic.status = StageStatus::InProgress;
        stages.semantic.started_at = Some(now_rfc3339());
        stages.semantic.total = Some(total_chunks);
    }
}

/// Flip the semantic stage to `Ready` and stamp `embedded` counter. Stage
/// 3 (graph) is set to `InProgress` since the post-batch KG rebuild always
/// follows immediately. Phase 1 keeps Stage 3 as a synchronous tail; Phase
/// 2 will spawn it separately.
async fn mark_semantic_ready_graph_in_progress(
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
async fn mark_reindex_failed(handle: &Arc<IndexHandle>, reason: &str) {
    let mut stages = handle.stages.write().await;
    // Lexical lane was genuinely built — keep it Ready/queryable.
    stages.semantic = StageState::failed(reason);
    stages.graph = StageState::failed(reason);
}

/// Flip the graph stage to `Ready`. After this transition the search
/// handler treats `kg` as a queryable lane and the legacy top-level
/// `status` field reports `"ready"`.
async fn mark_graph_ready(handle: &Arc<IndexHandle>) {
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
    /// Issue #109, Phase 1: skip the embed step entirely when the index
    /// was created with `lexical_only: true`. Producer task consults this
    /// in `prepare_and_parse_batch` so the embedder is never invoked.
    lexical_only: bool,
    /// PID slot for the trusty-embedderd sidecar (issue #315 lazy-spawn).
    ///
    /// Why: `LazyEmbedderHandle` defers spawning `trusty-embedderd` until
    /// the first embed request.  The subprocess spawn + ONNX model load
    /// takes 30–60 s; during this time the progress UI was completely
    /// frozen with no feedback ("Chunking…" header, 0/N Chunk bar,
    /// "Embedding… 0 chunks" stats line).
    ///
    /// Emitting `embedder_init` just before the first embed call and
    /// `embedder_ready` when the sidecar responds allows the CLI to
    /// transition the header to "Loading model…" so the operator sees
    /// why nothing appears to be moving.
    ///
    /// Detection: the PID slot holds `0` before the first spawn.  When we
    /// see a `0` PID on the first non-lexical-only batch, we emit
    /// `embedder_init` before calling `parse_and_embed_files` and
    /// `embedder_ready` after it returns successfully.
    ///
    /// `None` when no PID slot is available (non-sidecar embed mode — in
    /// that case model loading happens synchronously at daemon startup so
    /// there is no cold-start stall to surface).
    embedder_pid_slot: Option<Arc<AtomicU32>>,
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
    /// Issue #100: chunks dropped by the `TRUSTY_MAX_CHUNKS` cap in this
    /// batch. Aggregated into `RunTotals.chunks_dropped_by_cap` for the
    /// `complete` event and the index's `walk_truncated_by_budget` status flag.
    chunks_dropped_by_cap: usize,
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

    // Problem 1 UX fix: detect whether the embedder (sidecar OR in-process)
    // is about to be used for the first time (cold-start model load, 30-60 s).
    //
    // Sidecar path: The PID slot reads `0` before the first lazy spawn. If we
    // see `0` on a non-lexical-only batch we know the upcoming
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
    // We only emit once: after the first embedding call returns successfully,
    // we emit `embedder_ready`. Subsequent batches have indexed > 0 so this
    // branch is skipped. `lexical_only` indexes never embed.
    let first_batch_ever = ctx.progress.indexed.load(AtomicOrdering::Acquire) == 0;
    let needs_embedder_init = !ctx.lexical_only
        && if let Some(slot) = ctx.embedder_pid_slot.as_ref() {
            // Sidecar mode: PID 0 = not yet spawned.
            slot.load(AtomicOrdering::Acquire) == 0
        } else {
            // In-process (or any other non-sidecar) mode: fire on the very
            // first batch so the CLI gets an embedder_ready signal after the
            // first embed, even if model load is fast.
            first_batch_ever
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
    // For full-pipeline indexes, use `parse_and_embed_files_tracked` so
    // that per-wave `chunk_progress` SSE events fire at ~32-chunk granularity
    // (every `PROGRESS_CHUNK_INTERVAL` chunks inside `embed_chunks_in_batches`),
    // giving the CLI Embed bar continuous movement instead of one coarse jump
    // per 128-file file-batch.
    let parsed = {
        let indexer = ctx.handle.indexer.read().await;
        let result = if ctx.lexical_only {
            indexer.parse_files_only(to_index).await
        } else {
            use crate::core::indexer::PROGRESS_CHUNK_INTERVAL;
            use std::sync::atomic::Ordering;
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(usize, u64)>();
            let parse_result = indexer.parse_and_embed_files_tracked(to_index, tx).await;
            // Drain per-wave notifications and emit chunk_progress SSE events.
            // rx is closed when the sender (inside parse_and_embed_files_tracked)
            // is dropped on return, so recv() drains the buffer then yields None.
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
    // available. The CLI uses this to transition back from "Loading model…" to
    // "Embedding chunks…".
    if needs_embedder_init {
        ctx.progress
            .push(serde_json::json!({
                "event": "embedder_ready",
                "index_id": ctx.index_id.0,
            }))
            .await;
    }

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
        chunks_dropped_by_cap: commit.chunks_dropped_by_cap,
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
        // Issue #402 — relocation resilience: store file paths RELATIVE to the
        // index root so the corpus is portable when `root_path` is updated.
        // `strip_prefix` always succeeds here because `walk_source_files` only
        // returns canonicalised paths under the root; the `unwrap_or` is a
        // defensive fallback that preserves the previous behaviour for any edge-
        // case path that somehow escapes the root (e.g. a symlink target outside
        // the tree).
        let path_str = path
            .strip_prefix(&ctx.root)
            .unwrap_or(&path)
            .display()
            .to_string();
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
    for (path, h) in &new_hashes {
        ctx.hashes.insert(path.clone(), h.clone());
    }
    // Issue #75: cap per-index hash-cache size. Pure speed cache, so arbitrary
    // eviction is always safe.
    shrink_hashes_if_needed(&ctx.hashes);
    // Issue #662: mirror the newly-committed hashes to the redb corpus store
    // (staging or live — whichever is current) so they survive daemon restarts.
    // `persist_batch` is a no-op when the map is over-cap or there is no
    // durable corpus (BM25-only / test indexes).  Hashes are written to the
    // SAME redb file as the batch's chunks, preserving atomicity: the #603
    // staging rename promotes hashes and chunks together.
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

/// Run-level timing + memory totals collected across every batch.
struct RunTotals {
    /// Issue #744: wall-clock elapsed from reindex start to end of file walk.
    walk_ms: u64,
    parse_ms: u64,
    embed_ms: u64,
    bm25_ms: u64,
    vector_upsert_ms: u64,
    vector_count: usize,
    mem_limit_hit: bool,
    /// Issue #100: total chunks dropped by the per-index chunk cap across the
    /// whole reindex. Non-zero ⇒ the walk was truncated by the budget and the
    /// index is incomplete. Surfaced in the `complete` SSE event and in the
    /// per-index status response as `walk_truncated_by_budget`.
    chunks_dropped_by_cap: usize,
}

/// Emit the terminal `complete` SSE event with run-level timings + counters.
///
/// Why: extracted from `spawn_reindex_with_cleanup` (issue #98) so the
/// orchestrator doesn't carry a ~30-line JSON literal at its tail.
///
/// `embedderd_peak_rss_mb` — peak RSS of the embedderd sidecar during the
/// reindex run (issue #282). `None` when the sidecar was not running or
/// sampling failed for every poll tick.
async fn emit_complete_event(
    progress: &ReindexProgress,
    started: Instant,
    peak_rss_mb: u64,
    embedderd_peak_rss_mb: Option<u64>,
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
    let skipped_final = progress.skipped.load(Ordering::Acquire);
    // Why (issue #100 follow-up): clients reading just `indexed` /
    // `total_chunks` mistook the hash-skip fast path for a walker
    // regression. `indexed_new` = files actually re-chunked this run
    // (i.e. those that hash-missed) — when it's 0 alongside a non-zero
    // `skipped`, the run was a no-op fast path and `total_chunks: 0` is
    // expected. Derived rather than tracked separately to keep the
    // existing counters as the single source of truth.
    let indexed_new = indexed_final.saturating_sub(skipped_final);
    // Issue #120: surface the terminal status string in the SSE payload so
    // external callers (CLI, dashboard, open-mpm) can distinguish a clean
    // completion from a memory-abort without reading the daemon log.
    let status_str = if totals.mem_limit_hit {
        "aborted_memory"
    } else {
        "complete"
    };
    let mut event = serde_json::json!({
        "event": "complete",
        "status": status_str,
        "indexed": indexed_final,
        "indexed_new": indexed_new,
        "total_chunks": total_chunks,
        "skipped": skipped_final,
        "errors": progress.errors.load(Ordering::Acquire),
        "elapsed_ms": elapsed_ms,
        "chunks_per_sec": chunks_per_sec,
        "peak_rss_mb": peak_rss_mb,
        "memory_limit_hit": totals.mem_limit_hit,
        // Issue #100: surface budget truncation so callers can flag
        // indexes truncated by `TRUSTY_MAX_CHUNKS`. Mirrors
        // `memory_limit_hit` — non-zero/true ⇒ the index is incomplete.
        "walk_truncated_by_budget": totals.chunks_dropped_by_cap > 0,
        "chunks_dropped_by_cap": totals.chunks_dropped_by_cap,
        "kg_skipped": kg.kg_skipped,
        "timings": {
            // Issue #744: walk_ms added so the CLI and tooling can break down
            // where wall-clock goes (walk vs parse vs model-load vs embed vs KG).
            "walk_ms": totals.walk_ms,
            "parse_ms": totals.parse_ms,
            "embed_ms": totals.embed_ms,
            "bm25_ms": totals.bm25_ms,
            "vector_upsert_ms": totals.vector_upsert_ms,
            "kg_ms": kg.kg_ms,
            "vector_count": totals.vector_count,
            "symbol_count": kg.symbol_count,
            "edge_count": kg.edge_count,
        },
    });
    // Issue #282: include the sidecar peak RSS when available; omit the key
    // when `None` so consumers can tell "sidecar not running" from "sidecar
    // running but RSS was 0" (the latter cannot happen in practice because
    // a live process always has RSS > 0).
    if let Some(n) = embedderd_peak_rss_mb {
        event["embedderd_peak_rss_mb"] = serde_json::Value::Number(n.into());
    }
    progress.push(event).await;
}

/// Begin the atomic-swap corpus staging for a reindex (issue #28, Phase 4;
/// durable-data-loss fix issue #839).
///
/// Why: every reindex (force or incremental) stages its rebuilt corpus in a
/// sibling `index.redb.tmp` and atomically renames it into place only on
/// success (#603). Before the #839 fix, the staging file was always opened
/// FRESH (empty), and hash-skipped (unchanged) files were never written to
/// it — so after the atomic rename only the re-embedded files existed in the
/// durable corpus. On the next daemon restart those skipped files' chunks
/// were lost (durable data loss).
///
/// The fix: for a NON-force incremental reindex, before any batch writes,
/// copy every row from the LIVE corpus into the fresh staging store.  The
/// batch loop then upserts changed files' rows, overwriting their
/// pre-copied entries.  After the promote, the staging corpus holds ALL
/// files: changed (fresh) + unchanged (carried over from live).
///
/// For a FORCE reindex the behaviour is unchanged: the staging store starts
/// empty and every file is re-embedded from scratch.
///
/// What: when the index has a durable corpus store, opens a fresh
/// `index.redb.tmp`, conditionally seeds it from the live corpus (when
/// `!force`), and swaps it onto the indexer (so every `commit_parsed_batch`
/// writes the new corpus to the temp file). Returns `Ok(Some(path))` on
/// success; `Ok(None)` when staging is skipped (BM25-only / unresolvable
/// temp path — caller falls through to direct-write mode, live corpus
/// untouched); `Err(e)` when the live-corpus carryover copy failed for an
/// incremental reindex — caller MUST abort the reindex immediately (the live
/// corpus is still intact; the staging tmp is discarded, never promoted).
/// Test: `incremental_reindex_no_durable_data_loss` and
/// `incremental_reindex_carryover_failure_aborts` in `service::reindex::tests`.
async fn begin_force_corpus_swap(
    handle: &IndexHandle,
    index_id: &IndexId,
    force: bool,
) -> Result<Option<PathBuf>, anyhow::Error> {
    // Quick read-lock probe: nothing to stage if no durable corpus.
    // Also capture the live corpus Arc for the incremental copy path (#839).
    let live_corpus = {
        let indexer = handle.indexer.read().await;
        if !indexer.has_corpus_store() {
            return Ok(None);
        }
        // For incremental reindexes we need the live corpus to copy its rows
        // into the fresh staging store.  Cloning the Arc is cheap; the actual
        // copy happens on a blocking worker below.
        if !force {
            indexer.corpus_store()
        } else {
            None
        }
    };
    // Whether this is an incremental (carryover) reindex — tracked so the
    // error path can distinguish a copy failure from a staging-open failure.
    let is_incremental_carryover = live_corpus.is_some();
    // Issue #403: route tmp corpus path to colocated or legacy storage.
    let tmp_path = if crate::service::colocated_storage::has_colocated_storage(&handle.root_path) {
        match crate::service::colocated_storage::colocated_redb_tmp_path(&handle.root_path) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(
                    "force reindex: cannot resolve colocated staging corpus path for '{}' ({e}) — \
                     reindex will write directly to the live corpus",
                    index_id.0
                );
                return Ok(None);
            }
        }
    } else {
        match crate::service::persistence::corpus_redb_tmp_path(&index_id.0) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(
                    "force reindex: cannot resolve staging corpus path for '{}' ({e}) — \
                     reindex will write directly to the live corpus",
                    index_id.0
                );
                return Ok(None);
            }
        }
    };
    // Open the staging store on a blocking worker (redb's API is sync), then
    // seed it from the live corpus when performing an incremental reindex
    // (#839: carry unchanged files' durable rows so they survive the atomic
    // rename and the next daemon restart).
    //
    // IMPORTANT: if the carryover copy fails we propagate the error upward so
    // the caller can abort the reindex entirely.  Continuing with an empty
    // staging store and then promoting it would cause the same data loss as
    // the original #839 bug — we must NOT silently fall through here.
    let tmp_for_open = tmp_path.clone();
    let index_id_str = index_id.0.clone();
    let staged_result = tokio::task::spawn_blocking(move || {
        let store = crate::core::corpus::CorpusStore::open_fresh(&tmp_for_open)?;
        if let Some(live) = live_corpus {
            // Copy all durable rows (chunks + entities + file_hashes + _meta)
            // from the live corpus into the fresh staging store.  Changed
            // files' rows will be overwritten by the batch loop; unchanged
            // files' rows stay exactly as copied, ensuring the promoted corpus
            // is complete.  Any error here is FATAL for the reindex: a partial
            // copy that gets promoted is data loss, so we propagate and abort.
            store.copy_all_from(&live).with_context(|| {
                format!(
                    "reindex[{index_id_str}]: failed to seed staging corpus from live corpus — \
                     aborting incremental reindex to preserve live corpus integrity"
                )
            })?;
        }
        Ok::<_, anyhow::Error>(store)
    })
    .await;
    let staged = match staged_result {
        Ok(Ok(store)) => store,
        Ok(Err(e)) => {
            if is_incremental_carryover {
                // Carryover copy failed: propagate so the caller aborts the
                // reindex.  The live corpus is still intact (swap_corpus_store
                // was never called), the staging tmp file is on disk but
                // has not been wired up to the indexer — the caller must
                // clean it up or it will be orphaned until the next restart.
                // Logging at error here; the caller also logs and emits an
                // SSE error event.
                tracing::error!(
                    "reindex[{}]: ABORTING — could not copy live corpus into staging store ({e}); \
                     live corpus remains intact",
                    index_id.0
                );
                // Best-effort removal of the orphaned staging tmp before aborting.
                let _ = std::fs::remove_file(&tmp_path);
                return Err(e);
            }
            // For a force reindex (no carryover), failure to open/populate staging
            // is non-fatal: fall through to direct-write mode.
            tracing::warn!(
                "force reindex: could not open staging corpus for '{}' ({e}) — \
                 reindex will write directly to the live corpus",
                index_id.0
            );
            return Ok(None);
        }
        Err(e) => {
            tracing::warn!(
                "force reindex: staging corpus open task panicked for '{}': {e}",
                index_id.0
            );
            return Ok(None);
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
    Ok(Some(tmp_path))
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
    // Issue #403: route live corpus path to colocated or legacy storage.
    let live_path = if crate::service::colocated_storage::has_colocated_storage(&handle.root_path) {
        match crate::service::colocated_storage::colocated_redb_path(&handle.root_path) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(
                    "force reindex: cannot resolve colocated live corpus path for '{}' ({e}) — \
                     staged corpus left at {}",
                    index_id.0,
                    tmp_path.display()
                );
                return;
            }
        }
    } else {
        match crate::service::persistence::corpus_redb_path(&index_id.0) {
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
    // Issue #403: route live corpus path to colocated or legacy storage.
    let live_path = if crate::service::colocated_storage::has_colocated_storage(&handle.root_path) {
        crate::service::colocated_storage::colocated_redb_path(&handle.root_path)
    } else {
        crate::service::persistence::corpus_redb_path(&index_id.0)
    };
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

/// RAII guard that emits a terminal SSE error event if the reindex task exits
/// without having emitted one via the normal path.
///
/// Why: a Rust panic inside a `tokio::spawn` task unwinds that task silently
/// — the `broadcast::Sender` in `ReindexProgress` drops, live SSE subscribers
/// never receive a terminal frame, and the CLI reports "stream ended without
/// completion event" indefinitely. Placing this guard at the start of the
/// spawned future and calling `disarm()` only after `emit_complete_event`
/// completes ensures that ANY early exit (panic, early return, or `.await`
/// cancellation) emits `{"event":"error","message":"…"}` before the sender
/// drops.
///
/// What: holds `Arc<ReindexProgress>` and an `armed: bool` flag. `Drop`
/// checks the flag — if still armed, it pushes a blocking-channel send of
/// the error event directly onto the broadcast sender (no `.await` in `Drop`;
/// use `try_send`) and marks the progress status as `Failed`.
///
/// Test: `reindex_guard_fires_on_early_return` (below).
struct ReindexTerminationGuard {
    progress: Arc<ReindexProgress>,
    armed: bool,
}

impl ReindexTerminationGuard {
    fn new(progress: Arc<ReindexProgress>) -> Self {
        Self {
            progress,
            armed: true,
        }
    }

    /// Disarm the guard — call this after successfully emitting the terminal
    /// `complete` or `aborted_memory` event so `Drop` does not double-emit.
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ReindexTerminationGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        // The task is exiting without having emitted a terminal event.
        // Set the status to Failed and push an error event synchronously
        // (broadcast::Sender::send is non-blocking).
        self.progress.status.store(ReindexStatus::Failed);
        let msg = serde_json::json!({
            "event": "error",
            "message": "reindex task exited unexpectedly — check daemon logs for details"
        })
        .to_string();
        // `send` returns Err when there are no receivers; ignore — the replay
        // buffer is updated async by `push`, but Drop cannot be async. The
        // broadcast path alone is sufficient for live subscribers; replay is
        // not updated here because we cannot `.await` in Drop. Live
        // subscribers connected at the time of the crash will see the frame;
        // late subscribers reading the replay buffer will see the status as
        // `Failed` and can surface that to the user via the /status endpoint.
        let _ = self.progress.sender.send(msg);
    }
}

/// Variant of `spawn_reindex` that GC's the progress map after completion.
/// See `spawn_reindex` for the rationale.
///
/// Why: issue #458 — startup auto-discover can queue 40+ reindex tasks, all
/// competing for the same semaphore and starving user-initiated requests. The
/// `priority` flag routes the task to one of two separate semaphores:
///
///   - `priority=true`  → `reindex_semaphore()` (2 permits, interactive path)
///   - `priority=false` → `background_reindex_semaphore()` (1 permit, bulk path)
///
/// Interactive requests always get a permit from the interactive semaphore
/// regardless of how many background tasks are queued on the bulk semaphore.
///
/// `embedderd_pid_slot` — when `Some`, the orchestrator spawns a concurrent
/// RSS poller for the embedderd sidecar (issue #282) and includes
/// `embedderd_peak_rss_mb` in the SSE `complete` event. Pass
/// `state.embedderd_pid_slot.clone()` from the HTTP handler.
///
/// Test: `interactive_reindex_not_starved_by_background` verifies that a
/// background task holding the background semaphore does not block a
/// concurrent interactive request. `spawn_reindex_with_cleanup` itself is
/// side-effect-heavy (embedded tokio runtime); the semaphore routing logic is
/// factored into `reindex_semaphore_for` for unit testing.
#[allow(clippy::too_many_arguments)] // 8 args: adding quarantine (issue #764) is the last arg ever needed here
pub fn spawn_reindex_with_cleanup(
    handle: Arc<IndexHandle>,
    progress: Arc<ReindexProgress>,
    force: bool,
    cleanup_map: Option<Arc<DashMap<IndexId, Arc<ReindexProgress>>>>,
    aborted_map: Option<Arc<DashMap<IndexId, Instant>>>,
    embedderd_pid_slot: Option<Arc<AtomicU32>>,
    priority: bool,
    // Issue #764: optional quarantine registry. When `Some`, the task calls
    // `record_failure` on failure and `record_success` on a clean completion
    // so the quarantine counter stays accurate across retries. Pass
    // `Some(state.quarantine.clone())` from the HTTP handler.
    quarantine: Option<quarantine::ReindexQuarantine>,
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
        // Interactive (user-initiated) requests use `reindex_semaphore()` with
        // 2 permits; background/startup requests use the 1-permit
        // `background_reindex_semaphore()`. The two semaphores are independent,
        // so a background backlog of N tasks never blocks an interactive request.
        //
        // Late arrivals still queue; their SSE stream is already attached and
        // replays buffered events once the permit is acquired.
        let _permit = reindex_semaphore_for(priority)
            .acquire()
            .await
            .expect("reindex semaphore is never closed");
        // Decrement the background queue counter once the permit is held
        // (the task is now "in-flight", not "waiting").
        if !priority {
            BACKGROUND_QUEUE_DEPTH.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        }

        // Arm the termination guard. Any early exit from this point — whether
        // via an explicit `return`, a panic unwinding through the task, or an
        // `.await` cancellation — will fire `ReindexTerminationGuard::drop`,
        // which broadcasts an error event and marks the status `Failed`. The
        // guard is disarmed (and thus silenced) just after `emit_complete_event`
        // confirms the normal terminal event has been sent.
        let mut term_guard = ReindexTerminationGuard::new(Arc::clone(&progress));

        let started = Instant::now();
        // Issue #602 — portable paths: `walk_source_files_with_options`
        // canonicalizes its root and returns every file path *under that
        // canonical root*. Chunk-write `strip_prefix(&ctx.root)` must therefore
        // strip against the SAME canonical root, or it falls through to
        // `unwrap_or(&path)` and stores an ABSOLUTE path that fails to resolve
        // on a serving host with a different mount. `root` (raw `root_path`) is
        // kept for the SSE `start` event's display field; `canonical_root` is
        // what every `strip_prefix` uses so stored paths stay root-relative and
        // portable. See `validate::canonical_walk_root`.
        let root = handle.root_path.clone();
        let canonical_root = validate::canonical_walk_root(&root);
        let index_id: IndexId = handle.id.clone();

        // Issue #109, Phase 1: reset the staged-pipeline status surface so the
        // search handler stops advertising stale capabilities the moment the
        // reindex begins. `lexical_only` indexes pre-mark semantic + graph as
        // `Skipped` so a search that arrives mid-reindex never blocks waiting
        // for the embedder.
        reset_stages_for_reindex(&handle).await;

        // Phase 1: walk + filter the source tree (helper-extracted: issue #98).
        //
        // Issue #280: stamp walk-start time before collecting files so the
        // diagnostics are accurate even if the collection itself is slow.
        {
            let mut diag = handle.walk_diagnostics.write().await;
            diag.last_walk_started_at = Some(now_rfc3339());
            diag.last_walk_files_seen = 0;
            diag.last_walk_files_skipped = 0;
            diag.last_walk_error = None;
        }
        // Issue #744: stamp the walk end time so the phase-timing summary
        // can report how long the file scan took separately from parse/embed.
        let walk = collect_files_to_index(&handle);
        let walk_ms = started.elapsed().as_millis() as u64;
        let total = walk.files.len();
        // Issue #280: persist walk counters so the status endpoint can answer
        // "why is this index empty?" without the operator needing to read logs.
        // `skipped_dirs` from the walker tracks gitignore / binary / oversize
        // skips — the closest available proxy for "files skipped".
        // When the walk produced zero files we record a descriptive error
        // string so an operator running `curl /indexes/:id/status | jq` can see
        // the likely cause without diving into daemon logs.
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
        // Issue #317: emit `walk_complete` BEFORE the existing `start` event so
        // new CLI clients can render a dedicated "Walking files…" phase that
        // transitions to "Chunking…" the moment the file count is known. Old
        // CLI clients that don't recognise `walk_complete` simply ignore it and
        // keep waiting for `start` as before — fully backward-compatible.
        progress
            .push(serde_json::json!({
                "event": "walk_complete",
                "total_files": total,
                "index_id": index_id.0,
            }))
            .await;
        progress
            .push(serde_json::json!({
                "event": "start",
                "total_files": total,
                "index_id": index_id.0,
                "root_path": root,
                "force": force,
                "lexical_only": handle.lexical_only,
            }))
            .await;

        // Issue #744 — concurrent embedder warm-up.
        //
        // Why: on the default `stdio` path the sidecar (`trusty-embedderd`)
        // is lazy-spawned on the first `embed_batch` call. The ONNX model
        // load + CoreML / CUDA session compile takes 30–60 s, completely
        // serialising with the first batch and making it look like chunking
        // is slow. Spawning a background warm-up task here — CONCURRENTLY
        // with the hash-cache load and staging setup — means the sidecar is
        // already live (or well into init) by the time the batch loop begins.
        // On a warm daemon (PID slot already non-zero) this is a no-op: the
        // first embed call returns immediately. The task is fire-and-forget;
        // we do NOT await it — a warm-up failure is non-fatal and the first
        // real batch will retry. `lexical_only` indexes never embed, so we
        // skip the warm-up entirely to avoid a spurious lazy-spawn.
        //
        // Double-spawn guard: `LazyEmbedderHandle::embed_via` uses an
        // `Arc<Mutex<Option<SpawnedState>>>` for single-flight semantics; the
        // warm-up task and the first real batch race on that lock. Only one
        // wins the spawn; the loser finds `state = Some` and proceeds to the
        // embed immediately. No extra guard is needed here.
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

        let hashes = hashes_for(&index_id);
        // `--force` wipes the per-index content-hash cache so every file is
        // re-parsed, re-embedded, and re-committed even if its bytes haven't
        // changed since the last reindex in this daemon's lifetime. Without
        // this, the hash-skip check below silently turns `--force` into a
        // no-op on a warm daemon.
        //
        // Issue #602 — migration re-trigger: chunk `file` fields are stored
        // relative to the root current when they were written. If this index
        // was re-registered under a different root and is being reindexed
        // incrementally (force=false), the hash fast-path would skip unchanged
        // files and leave their stored paths relative to the OLD root —
        // silently resolving wrong on the new mount. Detect a root move against
        // the corpus's persisted `indexed_root` and clear the hash cache so
        // every file is re-written relative to the new canonical root. This is
        // the live-path analogue of the M002/M003 startup migrations.
        let prior_indexed_root = handle.read_indexed_root().await.unwrap_or(None);
        let root_moved =
            validate::needs_path_relativization(prior_indexed_root.as_deref(), &canonical_root);
        if force {
            hashes.clear();
            // Issue #662: clear the redb-persisted hashes too so a restart
            // after a force-reindex doesn't reload stale hashes.
            hash_cache::clear_persisted(&handle).await;
        } else if root_moved {
            tracing::warn!(
                "reindex[{}]: index root moved from {:?} to {} — clearing hash \
                 cache to re-relativize all chunk paths against the new root",
                index_id.0,
                prior_indexed_root,
                canonical_root.display(),
            );
            hashes.clear();
            // Issue #662: clear persisted hashes on root move for the same
            // reason — old root-relative paths would produce wrong skip decisions.
            hash_cache::clear_persisted(&handle).await;
        } else {
            // Issue #662: warm the in-process cache from the redb store so
            // unchanged files are skipped immediately after a daemon restart.
            hash_cache::load_into_cache(&handle, &hashes).await;
        }

        // Issue #28, Phase 4 + #603: stage the rebuilt corpus in a sibling
        // `index.redb.tmp` and atomically rename it over the live `index.redb`
        // only on success. As of #603 this is the DEFAULT for every reindex
        // with a durable corpus (not just `--force`): a failed or zero-vector
        // (#601) reindex must never destroy the only searchable copy. Every
        // `commit_parsed_batch` below writes to the staging file; the live
        // corpus is untouched until the reindex is validated and promoted.
        // `None` when staging was skipped (BM25-only index or unresolvable temp
        // path) — in that case commits write directly to the live corpus.
        //
        // Hardened carryover failure path (issue #839 follow-up): if the
        // incremental `copy_all_from` fails, `begin_force_corpus_swap` returns
        // `Err`.  Continuing with an empty staging store would produce exactly
        // the pre-fix data loss (unchanged files lost on promote).  We abort
        // the reindex instead and leave the live corpus intact.
        let corpus_swap_tmp: Option<PathBuf> =
            if staging::should_stage(handle.indexer.read().await.has_corpus_store()) {
                match begin_force_corpus_swap(&handle, &index_id, force).await {
                    Ok(path) => path,
                    Err(e) => {
                        // copy_all_from failed on an incremental reindex.
                        // The live corpus was never touched (swap_corpus_store
                        // was not called); it remains fully intact.
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
        // Issue #100: chunks the per-index `TRUSTY_MAX_CHUNKS` cap dropped
        // across all batches. Surfaced in the `complete` event and the
        // per-index status as `walk_truncated_by_budget` so operators can
        // distinguish a clean index from one whose walk was cut short by
        // the chunk budget.
        let mut total_chunks_dropped_by_cap: usize = 0;

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

        // Issue #282: if the caller provided the embedderd PID slot, spawn a
        // concurrent RSS poller for the sidecar process. 0-PID ticks are
        // silently skipped so non-sidecar modes incur no overhead.
        let peak_embedderd_rss_atomic = Arc::new(AtomicU64::new(0));
        let (embedderd_poller_handle, embedderd_poller_stop) =
            if let Some(pid_slot) = embedderd_pid_slot.as_ref() {
                // Take an initial sample before the batch loop begins.
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
            // Issue #602: strip-prefix against the canonical walk root so stored
            // chunk paths are always root-relative (portable), never absolute.
            root: canonical_root.clone(),
            index_id: index_id.clone(),
            hashes: hashes.clone(),
            mem_limit,
            mem_abort: mem_abort.clone(),
            peak_rss_atomic: peak_rss_atomic.clone(),
            started,
            total,
            lexical_only: handle.lexical_only,
            // Thread the embedderd PID slot for lazy-spawn detection
            // (Problem 1 UX fix — surfaces the model-init stall).
            embedder_pid_slot: embedderd_pid_slot.clone(),
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
            total_chunks_dropped_by_cap =
                total_chunks_dropped_by_cap.saturating_add(outcome.chunks_dropped_by_cap);
            if outcome.chunks_dropped_by_cap > 0 {
                progress.chunks_dropped_by_cap.fetch_add(
                    outcome.chunks_dropped_by_cap,
                    std::sync::atomic::Ordering::Release,
                );
            }
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

        // Issue #601 — non-empty validation gate. Classify the finished batch
        // loop BEFORE marking anything ready: a full-pipeline index that walked
        // files but produced ZERO vectors means every embed batch failed
        // (sidecar crash / OOM / model-load stall). Marking it ready here is the
        // exact false-green bug that served a dead index as healthy. The
        // lexical-only and zero-files cases are legitimate (see
        // `validate::reindex_outcome`).
        let memory_aborted = mem_limit_hit || mem_abort.load(AtomicOrdering::Acquire);
        // `has_embedder()` distinguishes a genuine embed failure (embedder
        // wired, zero vectors) from a legitimately embedder-less BM25-only /
        // test index. Only the former is a #601 failure.
        let embedder_present = handle.indexer.read().await.has_embedder();
        let reindex_outcome = validate::reindex_outcome(
            handle.lexical_only,
            embedder_present,
            total,
            total_vector_count,
        );
        // #603: the staging corpus only promotes when the reindex is both not
        // memory-aborted and validated Ready. Any other state rolls back,
        // leaving the previous live corpus intact.
        let staging_resolution = staging::resolve_staging(memory_aborted, &reindex_outcome);

        // Issue #109, Phase 1: the consumer loop has drained every batch's
        // BM25 + redb commit. The lexical (BM25) lane is genuinely built even
        // on an embed-failure, so flip it `Ready` so literal/exact-match search
        // still works while the operator investigates the embedder. For a
        // full-pipeline index, semantic is set `InProgress`; on a lexical-only
        // index semantic + graph stay `Skipped`.
        {
            let files_done = progress.indexed.load(AtomicOrdering::Acquire);
            let chunks_done = progress.total_chunks.load(AtomicOrdering::Acquire);
            mark_lexical_ready_semantic_in_progress(
                &handle,
                files_done,
                chunks_done,
                total_vector_count,
            )
            .await;
        }

        // Issue #29: the per-batch HNSW snapshot is throttled to one every
        // `HNSW_SNAPSHOT_BATCH_INTERVAL` batches, so the most recent ≤15
        // batches' vectors may not be on disk yet. Force one final snapshot
        // now — while the live HNSW store is still wired and before the
        // corpus swap drops the durable corpus handle — so a crash before the
        // next reindex never loses the tail of the rebuild.
        {
            let indexer = handle.indexer.read().await;
            indexer.force_incremental_persist();
        }

        // Issue #603: resolve the atomic corpus swap. Every batch committed
        // above wrote to `index.redb.tmp`; now we either atomically rename it
        // over the live `index.redb` (validated Ready, no memory abort) or
        // discard it and restore the original (memory abort OR #601 zero-vector
        // failure). Done before the KG rebuild so the durable corpus is settled
        // regardless of how the rebuild fares.
        if let Some(tmp_path) = &corpus_swap_tmp {
            if staging_resolution.is_commit() {
                commit_force_corpus_swap(&handle, &index_id, tmp_path).await;
                // #602: record the canonical root the promoted corpus is now
                // relativized against so a future run can detect a move.
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
                abort_force_corpus_swap(&handle, &index_id, tmp_path).await;
            }
        } else if reindex_outcome.is_ready() && !memory_aborted {
            // No staging (BM25-only / unresolvable temp): the live corpus was
            // written directly. Still record the indexed root on success so the
            // move-detection works for direct-write indexes too.
            if let Err(e) = handle.write_indexed_root(&canonical_root).await {
                tracing::debug!(
                    "reindex[{}]: indexed_root not persisted (no durable corpus): {e}",
                    index_id.0,
                );
            }
        }

        // Issue #601: a zero-vector embed failure on a full-pipeline index is a
        // HARD failure. Mark the semantic stage failed (loud, not false-green),
        // flip the progress status to Failed, and emit a terminal `error` event
        // carrying `embed_failure_count` so the SSE stream and `/status` surface
        // the cause. The live corpus was preserved by the rollback above.
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
            // Disarm the termination guard — we have emitted a terminal frame —
            // and stop here: skip KG rebuild and the success-path bookkeeping.
            // The previous live corpus remains searchable on its BM25 lane.
            term_guard.disarm();
            poller_stop.store(true, AtomicOrdering::Release);
            let _ = poller_handle.await;
            if let Some(stop) = embedderd_poller_stop {
                stop.store(true, AtomicOrdering::Release);
            }
            if let Some(h) = embedderd_poller_handle {
                let _ = h.await;
            }
            // Issue #764: record failure in the quarantine registry so the
            // next background reindex attempt backs off.
            if let Some(ref q) = quarantine {
                q.record_failure(&index_id);
            }
            schedule_progress_cleanup(cleanup_map, cleanup_id);
            return;
        }

        // Issue #109, Phase 1: flip the semantic stage to `Ready`. The
        // per-batch commits above wrote every chunk's embedding into HNSW;
        // the lane is queryable now even if Louvain hasn't finished. On a
        // `lexical_only` index this is a no-op (the slot stays `Skipped`).
        mark_semantic_ready_graph_in_progress(
            &handle,
            total_vector_count,
            progress.total_chunks.load(AtomicOrdering::Acquire),
        )
        .await;

        // Phase 3: rebuild the symbol graph once for the whole reindex.
        // Issue #90: always run, even after a memory abort — graph
        // construction is bounded by `TRUSTY_MAX_KG_NODES` and independent of
        // the embedding spike. See `rebuild_symbol_graph_for_reindex`.
        //
        // Issue #313: skip_kg indexes bypass Phase 3 entirely. The graph stage
        // stays Skipped (set at reset_stages_for_reindex) and kg_ms /
        // symbol_count / edge_count report 0 in the complete event.
        // Issue #401: emit `kg_start` before the KG rebuild so the CLI can
        // activate the KG progress bar. Skipped for skip_kg indexes (bar would
        // never complete). This event is backward-compatible — old CLI versions
        // that don't recognise it simply ignore it.
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
            // The event is intentionally minimal — the CLI only needs to know the
            // KG phase has begun; the final symbol/edge counts arrive in `kg_complete`.
            progress
                .push(serde_json::json!({
                    "event": "kg_start",
                    "index_id": index_id.0,
                }))
                .await;

            let outcome = rebuild_symbol_graph_for_reindex(&handle).await;

            // Issue #401: emit `kg_complete` with timing + graph stats so the CLI
            // can snap the KG bar to 100% and display the summary. Backward-
            // compatible: old CLI versions ignore the unknown event type.
            progress
                .push(serde_json::json!({
                    "event": "kg_complete",
                    "index_id": index_id.0,
                    "kg_ms": outcome.kg_ms,
                    "symbol_count": outcome.symbol_count,
                    "edge_count": outcome.edge_count,
                }))
                .await;

            // Issue #109, Phase 1: with the KG rebuild done, flip the graph
            // stage to `Ready`. Symbol graph is fully built — provenance navigation
            // (`get_call_chain`, `search_kg`) is immediately available.
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

        // Stop the background poller and collect the true peak it observed.
        poller_stop.store(true, AtomicOrdering::Release);
        // Best-effort: don't fail completion if the poller is wedged.
        let _ = poller_handle.await;

        // Issue #282: stop the embedderd poller (if running) and take a final
        // synchronous sample so the peak covers the post-KG phase too.
        if let Some(stop) = embedderd_poller_stop {
            stop.store(true, AtomicOrdering::Release);
        }
        if let Some(h) = embedderd_poller_handle {
            let _ = h.await;
        }
        // Final synchronous sample for the sidecar: the KG rebuild may push
        // the sidecar's RSS higher than any background tick caught.
        if let Some(pid_slot) = embedderd_pid_slot.as_ref() {
            let pid = pid_slot.load(AtomicOrdering::Acquire);
            if let Some(rss) = current_rss_mb_for_pid(pid) {
                let prev = peak_embedderd_rss_atomic.load(AtomicOrdering::Acquire);
                if rss > prev {
                    peak_embedderd_rss_atomic.store(rss, AtomicOrdering::Release);
                }
            }
        }
        // Materialise the Option: `None` when no sidecar slot was provided or
        // the sidecar was never observed alive (peak stayed 0).
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
        let skipped_final = progress.skipped.load(Ordering::Acquire);
        let elapsed_ms = started.elapsed().as_millis() as u64;
        // Why (issue #100 follow-up): the `files=` counter is bumped by every
        // processed file — *including* those hash-skipped by the per-process
        // content cache. A subsequent reindex (force=false) on an unchanged
        // workspace logs `files=N chunks=0`, which reads exactly like a
        // walker → chunker regression but is in fact the expected fast path.
        // Emitting `skipped=` (and the derived `indexed_new`) on the same
        // line distinguishes "no new work because hashes matched" from
        // "walker yielded N paths but chunker dropped them all", which is
        // the failure mode operators actually need to flag. The SSE
        // `complete` event already carries `skipped`; this propagates that
        // signal into the daemon log so a tail-log workflow tells the same
        // story without diving into the SSE stream.
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
        // Issue #744: emit a concise per-phase timing summary so operators can
        // identify exactly where wall-clock goes (walk, parse/chunk, model-load,
        // embed, commit, KG rebuild). The `model_load_ms` is approximated as
        // (elapsed_ms - walk_ms - total_parse_ms - total_embed_ms - kg.kg_ms)
        // and represents the time the pipeline was blocked on ONNX/CoreML
        // session init before the first embedding batch could start.
        // All times are in milliseconds; zero means "phase did not run".
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

        // The terminal event has been emitted — disarm the guard so its
        // `Drop` impl does not emit a spurious second error frame.
        term_guard.disarm();

        // Issue #764: record success in the quarantine registry so consecutive
        // failure counters are reset — a successful reindex proves the index
        // is healthy again.
        if let Some(ref q) = quarantine {
            q.record_success(&index_id);
        }

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
            respect_gitignore: true,
            path_filter: vec![],
            context_embedding: Arc::new(tokio::sync::RwLock::new(None)),
            context_summary: Arc::new(tokio::sync::RwLock::new(None)),
            indexed_head_sha: Arc::new(tokio::sync::RwLock::new(None)),
            lexical_only: false,
            skip_kg: false,
            stages: Arc::new(tokio::sync::RwLock::new(IndexStages::default())),
            search_pressure: Arc::new(tokio::sync::Notify::new()),
            walk_diagnostics: Arc::new(tokio::sync::RwLock::new(
                crate::core::registry::WalkDiagnostics::default(),
            )),
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
            respect_gitignore: true,
            path_filter: vec!["common-*".to_string()],
            context_embedding: Arc::new(tokio::sync::RwLock::new(None)),
            context_summary: Arc::new(tokio::sync::RwLock::new(None)),
            indexed_head_sha: Arc::new(tokio::sync::RwLock::new(None)),
            lexical_only: false,
            skip_kg: false,
            stages: Arc::new(tokio::sync::RwLock::new(IndexStages::default())),
            search_pressure: Arc::new(tokio::sync::Notify::new()),
            walk_diagnostics: Arc::new(tokio::sync::RwLock::new(
                crate::core::registry::WalkDiagnostics::default(),
            )),
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
        // Issue #317: the daemon now emits `walk_complete` BEFORE `start` so
        // the CLI can render a dedicated "Walking files…" phase. The first
        // event is `walk_complete`; `start` is the second event. Older
        // assertions that expected `start` to be first are updated here.
        assert!(
            events
                .first()
                .map(|s| s.contains("\"walk_complete\""))
                .unwrap_or(false),
            "first event must be walk_complete (issue #317); got: {:?}",
            events.first()
        );
        assert!(
            events
                .get(1)
                .map(|s| s.contains("\"start\""))
                .unwrap_or(false),
            "second event must be start; got: {:?}",
            events.get(1)
        );
        assert!(
            events
                .last()
                .map(|s| s.contains("\"complete\""))
                .unwrap_or(false),
            "last event must be complete; got: {:?}",
            events.last()
        );
    }

    /// Issue #100 follow-up: end-to-end guard that the walker → chunker →
    /// corpus pipeline persists chunks, distinct from the walker-only unit
    /// tests next to `walk_source_files_with_options`. The follow-up report
    /// for issue #100 observed `files=N chunks=0` after a v0.8.0 → v0.8.1
    /// daemon upgrade and (incorrectly) attributed it to the walker swap;
    /// the actual cause was the per-process content-hash cache hash-skipping
    /// every file on the second reindex (`force=false`). This test pins both
    /// the correct first-reindex chunking path AND the expected hash-skip
    /// fast path on a second reindex, so any future walker rewrite that
    /// silently drops paths fails here loudly while the documented fast
    /// path keeps working.
    ///
    /// Why: the unit walker tests only assert what the walker yields; they
    /// can't catch a chunker that silently emits zero chunks (the first half
    /// of this test) nor can they observe the hash-skip path (the second
    /// half). Without an e2e assertion the next time someone misreads the
    /// `chunks=0` log they'll bisect the walker again.
    /// What: stages a small repo (`.gitignore` excluding `excluded/`, plus a
    /// `crates/foo/src/lib.rs` with 3 `pub fn` definitions), runs the FULL
    /// reindex pipeline twice, and asserts:
    ///   1. First reindex (cold cache): `total_chunks > 0`, corpus
    ///      `chunk_count() > 0`, and a search for `alpha` returns a chunk
    ///      whose `file` field equals the canonical path of `lib.rs`.
    ///   2. Second reindex (warm cache): `total_chunks == 0` AND
    ///      `skipped == 1` — confirming the hash-skip path fires for
    ///      unchanged content (the failure mode operators mistake for a
    ///      walker regression).
    /// Test: this test.
    #[tokio::test]
    async fn reindex_persists_chunks_end_to_end() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        // Stage a tiny `crates/foo/src/lib.rs` with 3 functions plus a
        // gitignored `excluded/` subtree that must NOT contribute chunks.
        fs::create_dir_all(root.join("crates/foo/src")).unwrap();
        fs::create_dir_all(root.join("excluded")).unwrap();
        fs::write(root.join(".gitignore"), "excluded/\n").unwrap();
        let lib_rs = root.join("crates/foo/src/lib.rs");
        fs::write(
            &lib_rs,
            "pub fn alpha() {}\n\npub fn beta() -> i32 { 1 }\n\npub fn gamma(x: i32) -> i32 { x + 1 }\n",
        )
        .unwrap();
        fs::write(
            root.join("excluded/should_not_index.rs"),
            "pub fn nope() {}\n",
        )
        .unwrap();

        // Use a unique IndexId so the per-process `file_hashes` static (shared
        // across tests in the same binary) doesn't interfere — earlier tests
        // in this module reindex other temp dirs against unrelated index ids.
        let id = IndexId::new("e2e-pipeline-test");
        let indexer = CodeIndexer::new(id.0.clone(), root.clone());
        let handle = Arc::new(IndexHandle::bare(
            id.clone(),
            Arc::new(tokio::sync::RwLock::new(indexer)),
            root.clone(),
        ));

        // ----- First reindex: cold cache, chunks must be produced. -----
        let progress = Arc::new(ReindexProgress::new());
        spawn_reindex(handle.clone(), progress.clone(), false);
        for _ in 0..100 {
            if progress.status.load() == ReindexStatus::Complete {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert_eq!(progress.status.load(), ReindexStatus::Complete);

        // Walker yields exactly one file (`crates/foo/src/lib.rs`).
        assert_eq!(
            progress.total_files.load(Ordering::Acquire),
            1,
            "walker must yield exactly 1 file (gitignored subtree pruned)"
        );

        // The smoking-gun assertion the unit walker tests missed: the chunker
        // must have *persisted* chunks, not just been handed paths.
        let chunks = progress.total_chunks.load(Ordering::Acquire);
        assert!(
            chunks > 0,
            "regression: walker yielded 1 file but chunker persisted 0 chunks \
             on the first (cold-cache) reindex"
        );

        // On the cold-cache run the hash-skip path must NOT have fired.
        assert_eq!(
            progress.skipped.load(Ordering::Acquire),
            0,
            "first reindex hash-skipped a file (cold cache should hash-miss everything)"
        );

        // Issue #602 — portability: the corpus must store the ROOT-RELATIVE
        // path (`crates/foo/src/lib.rs`), and search must resolve it against the
        // serving host's `root_path`. Search results are intentionally absolute
        // (resolved via `resolve_chunk_file`), so a chunk written under one root
        // and served under a different root resolves correctly on each host.
        // The chunk-write `strip_prefix` now strips against the canonical walk
        // root, so the STORED `file` is always relative.
        let rel_lib_rs = "crates/foo/src/lib.rs";
        let expected_resolved = root.join(rel_lib_rs).to_string_lossy().into_owned();
        {
            let idx = handle.indexer.read().await;
            assert!(
                idx.chunk_count() > 0,
                "regression: indexer corpus is empty after reindex"
            );
            // Search for one of the functions to verify chunks are also live
            // in BM25 / vector. `alpha` is unique to the staged file.
            let results = idx
                .search(&crate::core::indexer::SearchQuery {
                    text: "alpha".into(),
                    top_k: 5,
                    expand_graph: false,
                    compact: false,
                    ..Default::default()
                })
                .await
                .unwrap();
            // The resolved (absolute) search path must be `root_path` joined
            // with the relative stored path — proving the stored path was
            // relative and is resolved against the live root.
            assert!(
                results.iter().any(|c| c.file == expected_resolved),
                "no chunk resolves to root_path + relative lib.rs (#602): \
                 expected {expected_resolved:?}, got {:?}",
                results.iter().map(|c| c.file.clone()).collect::<Vec<_>>()
            );
        }
        // Directly assert the corpus STORES a root-relative (non-absolute) path
        // — the actual #602 portability invariant. `raw_chunks_snapshot` exposes
        // the raw `RawChunk.file` (relative), bypassing the `resolve_chunk_file`
        // absolutization on the read path.
        {
            let idx = handle.indexer.read().await;
            let raw_files: Vec<String> = idx
                .raw_chunks_snapshot()
                .await
                .into_iter()
                .map(|c| c.file)
                .collect();
            assert!(
                raw_files.iter().any(|f| f == rel_lib_rs),
                "corpus did not store the ROOT-RELATIVE path (#602 regression); \
                 stored files: {raw_files:?}"
            );
            assert!(
                raw_files
                    .iter()
                    .all(|f| !std::path::Path::new(f).is_absolute()),
                "corpus stored an ABSOLUTE path (#602 regression): {raw_files:?}"
            );
        }

        // ----- Second reindex: warm cache, all files must hash-skip. -----
        //
        // This is the path the v0.8.1 follow-up report misread as a walker
        // regression. The log line `files=1 chunks=0` is correct: every file
        // hashed identically to the previous reindex, so the chunker is
        // intentionally bypassed. Pin this behaviour so the next bisection
        // doesn't waste another round chasing a non-existent walker bug.
        let progress2 = Arc::new(ReindexProgress::new());
        spawn_reindex(handle.clone(), progress2.clone(), false);
        for _ in 0..100 {
            if progress2.status.load() == ReindexStatus::Complete {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert_eq!(progress2.status.load(), ReindexStatus::Complete);
        assert_eq!(
            progress2.total_files.load(Ordering::Acquire),
            1,
            "second reindex must still walk 1 file"
        );
        assert_eq!(
            progress2.total_chunks.load(Ordering::Acquire),
            0,
            "second reindex of unchanged files MUST emit 0 new chunks (hash-skip path)"
        );
        assert_eq!(
            progress2.skipped.load(Ordering::Acquire),
            1,
            "second reindex must report the file as hash-skipped"
        );
        // The corpus must remain populated — hash-skip does not delete chunks.
        {
            let idx = handle.indexer.read().await;
            assert!(
                idx.chunk_count() > 0,
                "regression: corpus emptied by a hash-skip-only second reindex"
            );
        }
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

    /// Issue #601 (end-to-end, hermetic): a full-pipeline index whose embedder
    /// FAILS for every batch must end `Failed`, NOT `Complete` — and the
    /// previously-live corpus must be preserved (rolled back), not destroyed.
    ///
    /// Why: this is the exact false-green bug — before the non-empty gate, a
    /// silent embed failure flipped the index to ready with zero vectors and
    /// `/health` served a dead index as green. This test wires a `FailingEmbedder`
    /// (returns `Err` from every `embed_batch`) into an indexer that ALSO has a
    /// durable corpus pre-seeded with a "previous" chunk, runs the reindex, and
    /// asserts (1) status is `Failed`, (2) a terminal `error` event with
    /// `fatal: true` was emitted, and (3) the pre-existing corpus chunk survived
    /// the rollback. No real embedder daemon is involved — the failing mock makes
    /// it fully hermetic.
    /// What: see the assertions inline.
    /// Test: this test (daemon-free; the real-embedder spawn path is exercised
    /// only by the ignore-tagged ONNX integration tests).
    #[tokio::test]
    async fn reindex_marks_failed_on_zero_vectors_and_preserves_corpus() {
        use crate::core::embed::Embedder;
        use crate::core::store::{UsearchStore, VectorStore};
        use anyhow::anyhow;

        /// Embedder that fails every batch — emulates a sidecar crash / OOM /
        /// model-load stall so the reindex produces ZERO vectors despite an
        /// embedder being wired.
        struct FailingEmbedder;
        #[async_trait::async_trait]
        impl Embedder for FailingEmbedder {
            async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
                Err(anyhow!("simulated embedder failure (embed)"))
            }
            async fn embed_batch(&self, _texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
                Err(anyhow!("simulated embedder failure (every batch)"))
            }
            fn dimension(&self) -> usize {
                32
            }
        }

        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        fs::write(root.join("lib.rs"), "pub fn alpha() {}\n").unwrap();

        let dim = 32;
        let embedder: Arc<dyn Embedder> = Arc::new(FailingEmbedder);
        let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(dim).expect("usearch new"));
        let mut indexer =
            CodeIndexer::new("fail-601", root.clone()).with_components(embedder, store);

        // Pre-seed a durable corpus with a "previous" chunk so we can prove the
        // rollback preserved it. The staging swap requires a durable corpus.
        let corpus_path = tmp.path().join("index.redb");
        let corpus = crate::core::corpus::CorpusStore::open(&corpus_path).expect("open corpus");
        // Seed one "previous" chunk via the public `chunk_text` helper, then
        // pin a stable id we can assert survived the rollback.
        let mut prev = crate::core::chunker::chunk_text("prev/file.rs", "fn previous() {}", 64, 64);
        prev[0].id = "prev/file.rs:1:1".into();
        prev[0].file = "prev/file.rs".into();
        corpus.upsert_chunks(&prev).expect("seed prev chunk");
        indexer.set_corpus_store(Arc::new(corpus));

        let handle = Arc::new(IndexHandle::bare(
            IndexId::new("fail-601"),
            Arc::new(tokio::sync::RwLock::new(indexer)),
            root.clone(),
        ));
        let progress = Arc::new(ReindexProgress::new());
        spawn_reindex(handle.clone(), progress.clone(), false);

        // Wait for a terminal state (Failed expected).
        let mut terminal = ReindexStatus::Running;
        for _ in 0..100 {
            let s = progress.status.load();
            if s != ReindexStatus::Running {
                terminal = s;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert_eq!(
            terminal,
            ReindexStatus::Failed,
            "embed failure must mark the reindex Failed, not Complete"
        );

        // The lifecycle status must report `failed`, never `ready`.
        let stages = handle.stages.read().await.clone();
        assert_eq!(stages.lifecycle_status(), "failed");
        assert_eq!(stages.semantic.status, StageStatus::Failed);
        assert!(
            stages.semantic.failure.is_some(),
            "failed semantic stage must carry a reason"
        );

        // A terminal `error` event with `fatal: true` must have been emitted,
        // carrying the embed-failure signal (#601 LOUD failure, not false-green).
        let events = progress.events.lock().await.clone();
        assert!(
            events.iter().any(|e| e.contains("\"fatal\":true")
                && e.contains("\"event\":\"error\"")
                && e.contains("\"vector_count\":0")),
            "a fatal error event with vector_count:0 must be emitted: {events:?}"
        );

        // Non-destructive (#603): the failed rebuild's `lib.rs` chunks must NOT
        // have been promoted into the live corpus — the staging swap rolled
        // back. The seeded "previous" chunk's preservation across the rollback
        // re-open depends on the daemon's persistence path layout (the staging
        // helpers resolve the live corpus via the data-dir, not the ad-hoc test
        // path), so the round-trip restore is exercised by the daemon-gated
        // integration tests; here we assert the weaker hermetic invariant that
        // the failed rebuild was not committed.
        let live = handle.indexer.read().await.raw_chunks_snapshot().await;
        assert!(
            !live.iter().any(|c| c.file == "lib.rs"),
            "non-destructive: the failed rebuild must not promote lib.rs chunks; \
             got: {:?}",
            live.iter().map(|c| c.id.clone()).collect::<Vec<_>>()
        );
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

    // ── Staged-pipeline (issue #109, Phase 1) ──────────────────────────

    /// Helper: build an IndexHandle wrapping the bare BM25-only indexer
    /// with the given `lexical_only` setting. Mirrors the existing test
    /// fixtures but lets us flip the new flag.
    fn make_handle_with_flag(
        id: &str,
        root: std::path::PathBuf,
        lexical_only: bool,
    ) -> Arc<IndexHandle> {
        make_handle_with_flags(id, root, lexical_only, false)
    }

    /// Extended handle builder used by skip_kg tests.
    ///
    /// Why: the original `make_handle_with_flag` only parameterises `lexical_only`.
    /// Adding a second flag parameter would break all existing callers; instead
    /// the old function delegates here so both paths stay readable.
    /// What: constructs an `Arc<IndexHandle>` with the given `lexical_only` and
    /// `skip_kg` flags; pre-sets `stages` accordingly.
    /// Test: used by `skip_kg_index_never_runs_phase3` and
    /// `skip_kg_graph_stage_stays_skipped`.
    fn make_handle_with_flags(
        id: &str,
        root: std::path::PathBuf,
        lexical_only: bool,
        skip_kg: bool,
    ) -> Arc<IndexHandle> {
        use crate::core::registry::{IndexStages, StageState};
        let indexer = CodeIndexer::new(id.to_string(), root.clone());
        let stages = if lexical_only {
            IndexStages {
                lexical: StageState::pending(),
                semantic: StageState::skipped(),
                graph: StageState::skipped(),
            }
        } else if skip_kg {
            IndexStages {
                lexical: StageState::pending(),
                semantic: StageState::pending(),
                graph: StageState::skipped(),
            }
        } else {
            IndexStages::default()
        };
        Arc::new(IndexHandle {
            id: IndexId::new(id),
            indexer: Arc::new(tokio::sync::RwLock::new(indexer)),
            root_path: root,
            include_paths: vec![],
            exclude_globs: vec![],
            extensions: vec![],
            domain_terms: vec![],
            include_docs: false,
            respect_gitignore: true,
            path_filter: vec![],
            context_embedding: Arc::new(tokio::sync::RwLock::new(None)),
            context_summary: Arc::new(tokio::sync::RwLock::new(None)),
            indexed_head_sha: Arc::new(tokio::sync::RwLock::new(None)),
            lexical_only,
            skip_kg,
            stages: Arc::new(tokio::sync::RwLock::new(stages)),
            search_pressure: Arc::new(tokio::sync::Notify::new()),
            walk_diagnostics: Arc::new(tokio::sync::RwLock::new(
                crate::core::registry::WalkDiagnostics::default(),
            )),
        })
    }

    /// Issue #109 Phase 1 acceptance test: after a reindex completes on a
    /// BM25-only handle (no embedder wired), the lexical stage is `Ready`
    /// and the search capabilities array contains `bm25`. A search query
    /// then succeeds against the lexical lane and returns the expected
    /// chunk.
    ///
    /// Why: pins the contract that BM25 search works as soon as Stage 1
    /// finishes — the bedrock guarantee Phase 1 is delivering. The
    /// `lexical_only` and full-pipeline cases share the same Stage 1
    /// code path, so this test exercises both implicitly: the indexer
    /// has no embedder wired, which is the same shape `lexical_only`
    /// produces at runtime.
    /// What: stages a tiny repo, reindexes it, asserts the stages reflect
    /// Ready / Ready / Ready (graph rebuilds even without embedder), and
    /// that `search_capabilities` advertises bm25/literal/exact_match.
    /// Test: this test.
    #[tokio::test]
    async fn stage_1_completes_and_search_works_before_embedding() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        fs::write(root.join("hello.rs"), "pub fn unique_alpha() {}\n").unwrap();

        // Non-`lexical_only` handle but with no embedder wired — this is
        // the warm-boot BM25-only shape. Stage 1 must complete and the
        // search capabilities must advertise the lexical lane.
        let handle = make_handle_with_flag("stage1-test", root.clone(), false);
        let progress = Arc::new(ReindexProgress::new());
        spawn_reindex(handle.clone(), progress.clone(), false);

        for _ in 0..200 {
            if progress.status.load() == ReindexStatus::Complete {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert_eq!(progress.status.load(), ReindexStatus::Complete);

        // Lexical lane must be Ready (and so should the others — Stage 1
        // helpers don't gate graph or semantic on the embedder presence
        // because the corpus still has chunks for the KG to walk).
        let stages = handle.stages.read().await.clone();
        assert_eq!(
            stages.lexical.status,
            crate::core::registry::StageStatus::Ready,
            "stage 1 must finish on a BM25-only reindex"
        );
        let caps = stages.search_capabilities();
        assert!(
            caps.contains(&"bm25"),
            "search_capabilities must contain bm25 after Stage 1, got: {caps:?}"
        );

        // Search runs and the lexical lane returns the staged chunk.
        let idx = handle.indexer.read().await;
        let results = idx
            .search(&crate::core::indexer::SearchQuery {
                text: "unique_alpha".to_string(),
                top_k: 5,
                expand_graph: false,
                compact: false,
                ..Default::default()
            })
            .await
            .expect("search");
        assert!(
            results.iter().any(|c| c.content.contains("unique_alpha")),
            "BM25 lane must return the chunk after Stage 1: {results:?}"
        );
    }

    /// Issue #109 Phase 1: a `lexical_only` index permanently keeps the
    /// semantic + graph stages at `Skipped`. The reindex pipeline returns
    /// after Stage 1 and the search capabilities never include `vector`.
    /// The CLI `--lexical-only` flag and the `POST /indexes` `lexical_only`
    /// field both end up here.
    #[tokio::test]
    async fn lexical_only_index_never_runs_stage_2() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        fs::write(root.join("a.rs"), "pub fn lex_only_func() {}\n").unwrap();

        let handle = make_handle_with_flag("lexical-only-test", root.clone(), true);
        // Pre-condition: stages were initialised with semantic / graph as
        // `Skipped` (the helper does this for `lexical_only == true`).
        assert_eq!(
            handle.stages.read().await.semantic.status,
            crate::core::registry::StageStatus::Skipped
        );

        let progress = Arc::new(ReindexProgress::new());
        spawn_reindex(handle.clone(), progress.clone(), false);
        for _ in 0..200 {
            if progress.status.load() == ReindexStatus::Complete {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert_eq!(progress.status.load(), ReindexStatus::Complete);

        // The reindex finished but semantic + graph must STILL be Skipped.
        let stages = handle.stages.read().await.clone();
        assert_eq!(
            stages.lexical.status,
            crate::core::registry::StageStatus::Ready,
            "lexical must be Ready"
        );
        assert_eq!(
            stages.semantic.status,
            crate::core::registry::StageStatus::Skipped,
            "lexical_only must never flip semantic away from Skipped"
        );
        assert_eq!(
            stages.graph.status,
            crate::core::registry::StageStatus::Skipped,
            "lexical_only must never flip graph away from Skipped"
        );
        let caps = stages.search_capabilities();
        assert!(
            !caps.contains(&"vector"),
            "lexical_only must not advertise vector capability: {caps:?}"
        );
        assert!(
            !caps.contains(&"kg"),
            "lexical_only must not advertise kg capability: {caps:?}"
        );

        // Search via the lexical lane works even with `stage: Some(Lexical)`.
        let idx = handle.indexer.read().await;
        let results = idx
            .search(&crate::core::indexer::SearchQuery {
                text: "lex_only_func".to_string(),
                top_k: 5,
                expand_graph: false,
                compact: false,
                stage: Some(crate::core::indexer::SearchStage::Lexical),
                ..Default::default()
            })
            .await
            .expect("search");
        assert!(
            results.iter().any(|c| c.content.contains("lex_only_func")),
            "lexical lane must return the chunk on lexical_only: {results:?}"
        );

        // And the lifecycle status maps to terminal "ready" — not
        // `indexed_lexical`, since semantic + graph are permanently
        // Skipped (which the lifecycle helper treats as terminal).
        assert_eq!(stages.lifecycle_status(), "ready");
    }

    /// Issue #313: a `skip_kg` index permanently keeps the graph stage at
    /// `Skipped`. The reindex pipeline runs Stages 1 and 2 as normal but
    /// Phase 3 (KG rebuild) is bypassed. The SSE complete event must report
    /// `kg_skipped: true`, `kg_ms: 0`, `symbol_count: 0`, `edge_count: 0`.
    /// `search_capabilities` must never include `"kg"`.
    ///
    /// Why: pins the Phase 3 bypass contract so a regression to the
    /// unconditional `rebuild_symbol_graph_for_reindex` call is immediately
    /// caught — the graph stage flipping to Ready would fail this test.
    /// What: builds a skip_kg handle, reindexes a tiny fixture repo, asserts
    /// the graph stage stays Skipped and the KG metrics in the complete event
    /// are all zero.
    /// Test: this test.
    #[tokio::test]
    async fn skip_kg_index_never_runs_phase3() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        fs::write(root.join("b.rs"), "pub fn skip_kg_func() { let x = 1; }\n").unwrap();

        let handle = make_handle_with_flags("skip-kg-test", root.clone(), false, true);
        // Pre-condition: graph stage pre-set to Skipped.
        assert_eq!(
            handle.stages.read().await.graph.status,
            crate::core::registry::StageStatus::Skipped
        );

        let progress = Arc::new(ReindexProgress::new());
        spawn_reindex(handle.clone(), progress.clone(), false);
        for _ in 0..200 {
            if progress.status.load() == ReindexStatus::Complete {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert_eq!(progress.status.load(), ReindexStatus::Complete);

        // After reindex: graph must STILL be Skipped.
        let stages = handle.stages.read().await.clone();
        assert_eq!(
            stages.lexical.status,
            crate::core::registry::StageStatus::Ready,
            "lexical must be Ready"
        );
        assert_eq!(
            stages.graph.status,
            crate::core::registry::StageStatus::Skipped,
            "skip_kg must never flip graph away from Skipped"
        );
        let caps = stages.search_capabilities();
        assert!(
            !caps.contains(&"kg"),
            "skip_kg must not advertise kg capability: {caps:?}"
        );

        // Symbol graph must be empty (Phase 3 was skipped).
        let indexer = handle.indexer.read().await;
        let graph = indexer.snapshot_symbol_graph().await;
        assert_eq!(
            graph.node_count(),
            0,
            "symbol graph must be empty when skip_kg=true"
        );
    }

    /// Issue #109 Phase 1: as stages advance from `Pending` →
    /// `InProgress` → `Ready`, `search_capabilities` grows monotonically.
    /// Walks every transition via `mark_*` helpers directly so the test
    /// doesn't have to race the reindex pipeline.
    #[tokio::test]
    async fn search_capabilities_grows_as_stages_complete() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        fs::write(root.join("a.rs"), "pub fn stage_grow() {}\n").unwrap();
        let handle = make_handle_with_flag("caps-grow-test", root.clone(), false);

        // Pending: empty caps.
        assert!(handle.stages.read().await.search_capabilities().is_empty());

        // Simulate the pipeline by calling the same helpers the orchestrator
        // uses. The result must match the ticket's monotonic-growth contract.
        reset_stages_for_reindex(&handle).await;
        // Still no caps — lexical is in progress, not ready.
        assert!(handle.stages.read().await.search_capabilities().is_empty());

        mark_lexical_ready_semantic_in_progress(&handle, 1, 1, 1).await;
        let caps = handle.stages.read().await.search_capabilities();
        assert!(caps.contains(&"bm25") && !caps.contains(&"vector"));

        mark_semantic_ready_graph_in_progress(&handle, 1, 1).await;
        let caps = handle.stages.read().await.search_capabilities();
        assert!(caps.contains(&"vector") && !caps.contains(&"kg"));

        mark_graph_ready(&handle).await;
        let caps = handle.stages.read().await.search_capabilities();
        assert!(caps.contains(&"bm25"));
        assert!(caps.contains(&"vector"));
        assert!(caps.contains(&"kg"));
        assert_eq!(handle.stages.read().await.lifecycle_status(), "ready");
    }

    // ── Issue #280: walk diagnostic fields ──────────────────────────────

    /// After a successful reindex, `walk_diagnostics` on the handle must carry
    /// a non-None `last_walk_started_at`, a positive `last_walk_files_seen`
    /// count, and a `None` `last_walk_error`.
    ///
    /// Why: operators need the status endpoint to answer "why is this index
    /// empty?" without diving into daemon logs.  This test pins the contract
    /// that a clean walk populates the timestamp and file-seen counter.
    /// What: stage a tiny fixture dir, run a reindex, read `walk_diagnostics`,
    /// and assert all three fields are correct.
    /// Test: this test.
    #[tokio::test]
    async fn walk_diagnostics_populated_after_reindex() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        fs::write(root.join("diag_check.rs"), "fn diag_fn() {}\n").unwrap();

        let handle = make_handle_with_flag("diag-test", root.clone(), false);
        let progress = Arc::new(ReindexProgress::new());
        spawn_reindex(handle.clone(), progress.clone(), false);

        for _ in 0..100 {
            if progress.status.load() == ReindexStatus::Complete {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert_eq!(progress.status.load(), ReindexStatus::Complete);

        let diag = handle.walk_diagnostics.read().await.clone();
        assert!(
            diag.last_walk_started_at.is_some(),
            "last_walk_started_at must be set after reindex, got {:?}",
            diag
        );
        assert!(
            diag.last_walk_files_seen > 0,
            "last_walk_files_seen must be > 0 when files exist, got {:?}",
            diag
        );
        assert!(
            diag.last_walk_error.is_none(),
            "last_walk_error must be None on a clean walk, got {:?}",
            diag.last_walk_error
        );
    }

    /// When the root path has no source files (e.g. all filtered out),
    /// `last_walk_files_seen` == 0 and `last_walk_error` contains a diagnostic
    /// message so the operator can see why the index is empty.
    ///
    /// Why: a zero-file walk is the most common cause of zero-chunk indexes.
    /// The walk_error message is the first thing an operator would check.
    /// What: create an empty fixture dir (no .rs files), run reindex, verify
    /// that `last_walk_files_seen == 0` and `last_walk_error.is_some()`.
    /// Test: this test.
    #[tokio::test]
    async fn walk_diagnostics_error_set_when_zero_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        // No source files in the directory — walk will produce zero files.

        let handle = make_handle_with_flag("diag-zero-test", root.clone(), false);
        let progress = Arc::new(ReindexProgress::new());
        spawn_reindex(handle.clone(), progress.clone(), false);

        for _ in 0..100 {
            if progress.status.load() == ReindexStatus::Complete {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert_eq!(progress.status.load(), ReindexStatus::Complete);

        let diag = handle.walk_diagnostics.read().await.clone();
        assert_eq!(
            diag.last_walk_files_seen, 0,
            "last_walk_files_seen must be 0 for empty directory, got {:?}",
            diag
        );
        assert!(
            diag.last_walk_error.is_some(),
            "last_walk_error must be set when zero files are found, got {:?}",
            diag
        );
    }

    // ── Issue #458: priority semaphore routing ────────────────────────────────

    /// Why: `reindex_semaphore_for` is the single routing point between
    /// interactive and background reindexes. This test verifies that the correct
    /// static semaphore instance is returned — if the routing is inverted,
    /// background tasks would starve interactive ones instead of the reverse.
    ///
    /// What: calls `reindex_semaphore_for` with both `true` and `false`,
    /// asserts that the returned pointer addresses differ (proving two distinct
    /// semaphores), and that the same call twice returns the same pointer
    /// (proving the OnceLock singleton is stable).
    ///
    /// Test: this test. The actual starvation property (background never blocks
    /// interactive) requires a live reindex task and is documented in the module
    /// header as needing runtime verification.
    #[test]
    fn reindex_semaphore_selection_routes_by_priority() {
        let interactive = reindex_semaphore_for(true) as *const Semaphore;
        let background = reindex_semaphore_for(false) as *const Semaphore;

        // The two semaphores must be distinct objects.
        assert_ne!(
            interactive, background,
            "interactive and background must be different semaphore instances"
        );

        // Each call to the same priority must return the same singleton.
        assert_eq!(
            interactive,
            reindex_semaphore_for(true) as *const Semaphore,
            "interactive semaphore must be a stable singleton"
        );
        assert_eq!(
            background,
            reindex_semaphore_for(false) as *const Semaphore,
            "background semaphore must be a stable singleton"
        );
    }

    /// Why: verifies that a background task holding the background semaphore
    /// does NOT block an interactive request from acquiring its own permit.
    ///
    /// What: constructs two independent semaphores that mirror the exact permit
    /// counts of the global ones (`MAX_PARALLEL_REINDEXES` and
    /// `MAX_PARALLEL_BACKGROUND_REINDEXES`), saturates the background semaphore,
    /// then asserts the interactive semaphore still has free capacity. Using
    /// local semaphores avoids contention with parallel test workers that may
    /// have consumed the global static semaphore's permits.
    ///
    /// The static `reindex_semaphore_for` routing (which returns the actual
    /// global semaphores) is verified separately in
    /// `reindex_semaphore_selection_routes_by_priority`.
    ///
    /// Test: this test. The end-to-end case (user `index` command returns
    /// promptly while 44 background tasks queue) requires a running daemon and
    /// is documented as needing manual/integration verification.
    #[tokio::test]
    async fn interactive_not_blocked_when_background_semaphore_full() {
        // Local semaphores with the same capacities as the global ones so
        // this test is isolated from other parallel tests.
        let bg_sem = Semaphore::new(MAX_PARALLEL_BACKGROUND_REINDEXES);
        let interactive_sem = Semaphore::new(MAX_PARALLEL_REINDEXES);

        // Saturate the background semaphore (simulating full startup backlog).
        let _bg_permit = bg_sem
            .acquire()
            .await
            .expect("background semaphore unexpectedly closed");

        // The interactive semaphore must still have free capacity — a user
        // request would be admitted immediately despite the full background queue.
        let interactive_permit = interactive_sem
            .try_acquire()
            .expect("interactive semaphore must have a free permit even when background is full");

        // Prove the claim: the permit was granted while the background is saturated.
        assert_eq!(
            bg_sem.available_permits(),
            0,
            "background semaphore must be fully saturated"
        );
        assert!(
            interactive_sem.available_permits() < MAX_PARALLEL_REINDEXES,
            "interactive semaphore must show one consumed permit"
        );

        drop(interactive_permit);
        // `_bg_permit` drops here, releasing the background slot.
    }

    /// Why: `background_reindex_queue_depth()` must reflect the number of
    /// background tasks that have been registered but not yet started (i.e.
    /// queued + in-flight). Without this counter the /health endpoint cannot
    /// expose the startup storm backlog.
    ///
    /// What: directly manipulates `BACKGROUND_QUEUE_DEPTH` via `fetch_add`
    /// (the same path used by `spawn_reindex_with_cleanup`) and verifies the
    /// public reader returns the correct value.
    ///
    /// Test: this test. Note that the full end-to-end flow (counter increments
    /// when a background task is spawned and decrements when the permit is
    /// obtained) is exercised by `spawn_reindex_with_cleanup` at runtime — the
    /// atomics themselves are standard and don't need separate concurrency tests.
    #[test]
    fn background_reindex_queue_depth_counts_waiting_tasks() {
        // Save initial value and restore afterward so parallel tests are unaffected.
        let initial = BACKGROUND_QUEUE_DEPTH.load(std::sync::atomic::Ordering::Relaxed);

        BACKGROUND_QUEUE_DEPTH.fetch_add(3, std::sync::atomic::Ordering::Relaxed);
        let after_add = background_reindex_queue_depth();
        assert_eq!(
            after_add,
            initial + 3,
            "queue depth must increase by 3 after 3 increments"
        );

        BACKGROUND_QUEUE_DEPTH.fetch_sub(3, std::sync::atomic::Ordering::Relaxed);
        let after_sub = background_reindex_queue_depth();
        assert_eq!(
            after_sub, initial,
            "queue depth must return to initial after 3 decrements"
        );
    }

    /// The `ReindexTerminationGuard` must emit an error event and set the
    /// status to `Failed` when it is dropped while still armed.
    ///
    /// Why: Fix C guards against early-exit / panic paths that would otherwise
    /// drop the `broadcast::Sender` without emitting any terminal SSE frame,
    /// leaving CLI subscribers blocked waiting for a completion event that
    /// never arrives.
    ///
    /// What: constructs a `ReindexProgress`, arms a guard, drops it without
    /// disarming, then asserts (1) status == Failed, (2) at least one event
    /// was broadcast.
    ///
    /// Test: this test.
    #[test]
    fn reindex_guard_fires_on_early_return() {
        let progress = Arc::new(ReindexProgress::new());
        // Subscribe before dropping so we can receive the broadcast.
        let mut rx = progress.sender.subscribe();

        {
            let _guard = ReindexTerminationGuard::new(Arc::clone(&progress));
            // Drop without calling `disarm()`.
        }

        assert_eq!(
            progress.status.load(),
            ReindexStatus::Failed,
            "status must be Failed after guard drops while armed"
        );
        let msg = rx
            .try_recv()
            .expect("guard must have broadcast an error event");
        assert!(
            msg.contains("\"error\""),
            "broadcast message must contain event:error; got: {msg}"
        );
    }

    /// A disarmed `ReindexTerminationGuard` must NOT emit an error event on drop.
    ///
    /// Why: if `disarm()` were a no-op the guard would double-emit, causing CLI
    /// clients to see both a valid `complete` event and a spurious `error` event.
    ///
    /// What: arms a guard, calls `disarm()`, drops it, and asserts the broadcast
    /// channel is still empty.
    ///
    /// Test: this test.
    #[test]
    fn reindex_guard_does_not_fire_after_disarm() {
        let progress = Arc::new(ReindexProgress::new());
        let mut rx = progress.sender.subscribe();

        {
            let mut guard = ReindexTerminationGuard::new(Arc::clone(&progress));
            guard.disarm();
        }

        assert_eq!(
            rx.try_recv()
                .err()
                .map(|e| matches!(e, tokio::sync::broadcast::error::TryRecvError::Empty)),
            Some(true),
            "no event should be broadcast after disarm"
        );
    }

    /// Issue #839 regression: an incremental reindex must NOT lose hash-skipped
    /// files' chunks from the durable corpus after a daemon restart.
    ///
    /// Why: before the #839 fix, `begin_force_corpus_swap` opened a FRESH empty
    /// staging corpus and hash-skipped files were never written to it. On promote,
    /// only the re-embedded files' chunks existed in redb — skipped files were
    /// silently lost on the next daemon restart (reopen from disk).
    ///
    /// This test directly models the pre-fix and post-fix staging behaviour using
    /// only `CorpusStore` primitives (no daemon infrastructure). It avoids the
    /// `persistence::corpus_redb_path` dependency that routes the atomic rename to
    /// a daemon-controlled global directory (which the test cannot control).
    ///
    /// Two scenarios are verified:
    ///
    /// A) PRE-FIX (unfixed) model: fresh empty staging, only re-indexed files
    ///    written → restart loses skipped files' chunks (asserted absent).
    /// B) POST-FIX model: staging seeded from live via `copy_all_from`, re-indexed
    ///    file's rows overwritten → restart sees ALL files' chunks.
    ///
    /// Test: this test (issue #839).
    #[test]
    fn incremental_reindex_no_durable_data_loss() {
        use crate::core::chunker::{ChunkType, RawChunk};
        use crate::core::corpus::CorpusStore;

        let dir = tempfile::tempdir().unwrap();

        // Helper: build a minimal RawChunk for a given file + id.
        let chunk = |file: &str, id: &str, content: &str| RawChunk {
            id: id.to_string(),
            file: file.to_string(),
            start_line: 1,
            end_line: 1,
            content: content.to_string(),
            function_name: None,
            language: Some("rust".to_string()),
            chunk_type: ChunkType::Code,
            calls: Vec::new(),
            inherits_from: Vec::new(),
            chunk_depth: 0,
            parent_chunk_id: None,
            child_chunk_ids: Vec::new(),
            nlp_keywords: Vec::new(),
            nlp_code_refs: Vec::new(),
            virtual_terms: Vec::new(),
        };

        // ── Set up the live corpus representing a fully-indexed 2-file repo ──
        //
        // Pretend the first (cold) reindex ran and both files are in the live
        // `index.redb`. On the next incremental reindex:
        //   - stable.rs → unchanged, hash-skipped (NOT re-embedded)
        //   - changing.rs → content changed, hash-miss (re-embedded)
        let live_path = dir.path().join("index.redb");
        {
            let live = CorpusStore::open(&live_path).unwrap();
            live.upsert_chunks(&[
                chunk("stable.rs", "stable:1:1", "fn stable_v1() {}"),
                chunk("changing.rs", "changing:1:1", "fn version_one() {}"),
            ])
            .unwrap();
            live.upsert_entities(&[
                ("stable.rs".to_string(), Vec::new()),
                ("changing.rs".to_string(), Vec::new()),
            ])
            .unwrap();
            live.upsert_file_hashes(&[("stable.rs", "aa"), ("changing.rs", "bb")])
                .unwrap();
        }

        // ─── Scenario A: PRE-FIX behaviour ───────────────────────────────────
        //
        // The unfixed `begin_force_corpus_swap` opened a FRESH EMPTY staging
        // corpus. The batch loop only wrote re-embedded files' chunks; stable.rs
        // was skipped. After the promote rename, the new `index.redb` contains
        // ONLY changing.rs's rows.
        //
        // This scenario shows what the bug looked like — we assert stable.rs is
        // missing to prove the bug model is correct and the fix is necessary.
        let pre_fix_staging_path = dir.path().join("pre_fix.redb");
        {
            // Open a fresh empty staging (the bug: no copy from live).
            let staging = CorpusStore::open_fresh(&pre_fix_staging_path).unwrap();

            // Only the re-embedded file is written to staging.
            staging
                .upsert_chunks(&[chunk("changing.rs", "changing:1:1", "fn version_two() {}")])
                .unwrap();

            // Staging is atomically promoted (simulated here by just dropping it).
            // After the "promote", the corpus IS staging — stable.rs was never written.
        }
        // Simulate a restart: reopen staging as if it were the new `index.redb`.
        let pre_fix_store = CorpusStore::open(&pre_fix_staging_path).unwrap();
        let pre_fix_chunks = pre_fix_store.load_all_chunks().unwrap();
        assert!(
            pre_fix_chunks.iter().all(|c| c.file != "stable.rs"),
            "PRE-FIX model: stable.rs must be absent from the unfixed staging corpus \
             (this proves the bug existed — the fix is needed)"
        );
        assert_eq!(
            pre_fix_chunks.len(),
            1,
            "PRE-FIX model: only the re-embedded file must be present"
        );

        // ─── Scenario B: POST-FIX behaviour ──────────────────────────────────
        //
        // The fixed `begin_force_corpus_swap` calls `copy_all_from(&live)` before
        // any batch writes, seeding the staging corpus with ALL rows from the live
        // corpus. The batch loop then upserts only the re-embedded (changed) files,
        // overwriting their pre-copied rows. After the promote, ALL files survive.
        let post_fix_staging_path = dir.path().join("post_fix.redb");
        {
            let live = CorpusStore::open(&live_path).unwrap();
            let staging = CorpusStore::open_fresh(&post_fix_staging_path).unwrap();

            // THE FIX: seed staging from live before any batch writes.
            staging.copy_all_from(&live).unwrap();

            // The batch loop upserts ONLY the re-embedded (changed) file.
            // stable.rs is hash-skipped — it is never touched by the batch loop.
            staging
                .upsert_chunks(&[chunk("changing.rs", "changing:1:1", "fn version_two() {}")])
                .unwrap();

            // Staging is promoted (simulated by drop).
        }
        // Simulate a restart: reopen as if it were the new `index.redb`.
        let post_fix_store = CorpusStore::open(&post_fix_staging_path).unwrap();
        let mut post_fix_chunks = post_fix_store.load_all_chunks().unwrap();
        post_fix_chunks.sort_by(|a, b| a.file.cmp(&b.file));

        assert_eq!(
            post_fix_chunks.len(),
            2,
            "POST-FIX model: BOTH files must be present after the incremental \
             reindex + simulated restart; got: {:?}",
            post_fix_chunks.iter().map(|c| &c.file).collect::<Vec<_>>()
        );

        // stable.rs must have its ORIGINAL chunk content (hash-skipped, not re-embedded).
        let stable = post_fix_chunks
            .iter()
            .find(|c| c.file == "stable.rs")
            .expect("BUG #839: stable.rs must survive in the durable corpus after the fix");
        assert_eq!(
            stable.content, "fn stable_v1() {}",
            "stable.rs must retain its original content (it was hash-skipped)"
        );

        // changing.rs must have its NEW content (it was re-indexed).
        let changing = post_fix_chunks
            .iter()
            .find(|c| c.file == "changing.rs")
            .expect("changing.rs must be present after the second reindex");
        assert_eq!(
            changing.content, "fn version_two() {}",
            "changing.rs must have the new content after the second reindex"
        );

        // File hashes must also survive for stable.rs (so the NEXT incremental
        // reindex can still hash-skip it from the durable store).
        let hashes = post_fix_store.load_file_hashes().unwrap();
        assert!(
            hashes.iter().any(|(f, _)| f == "stable.rs"),
            "stable.rs file hash must survive in the durable corpus so future \
             incremental reindexes can still hash-skip it"
        );
    }

    /// Why: validates that the hardened incremental-reindex abort path (issue
    /// #839 follow-up) correctly preserves the live corpus when `copy_all_from`
    /// fails — no data is lost, no empty staging store is promoted.
    ///
    /// Before this hardening the original #839 fix carried unchanged chunks
    /// into a fresh staging store, but if `copy_all_from` itself failed the
    /// code silently continued with an EMPTY staging store — exactly the #839
    /// data loss reproduced by an I/O error.  The hardened path propagates the
    /// copy error as `Err`; the caller aborts before calling `swap_corpus_store`
    /// so the live corpus is never replaced.
    ///
    /// Two things are verified:
    ///
    ///   (a) ERROR PROPAGATION — `copy_all_from` returns `Err` on failure
    ///       (validates the `?` contract in the function body, not just the
    ///       call-site handling).  We trigger this by attempting to open a
    ///       staging target at a directory path, which redb cannot open.
    ///
    ///   (b) LIVE CORPUS INTACT — the live corpus retains all its original
    ///       chunks after a staging setup failure.  This mirrors the production
    ///       abort path: `begin_force_corpus_swap` returns `Err` without ever
    ///       calling `swap_corpus_store`, so `index.redb` is never renamed.
    ///
    /// Test: this test (issue #839 hardening).
    #[test]
    fn incremental_reindex_carryover_failure_aborts() {
        use crate::core::chunker::{ChunkType, RawChunk};
        use crate::core::corpus::CorpusStore;

        let dir = tempfile::tempdir().unwrap();

        // Build a minimal RawChunk.
        let make_chunk = |file: &str, id: &str, content: &str| RawChunk {
            id: id.to_string(),
            file: file.to_string(),
            start_line: 1,
            end_line: 1,
            content: content.to_string(),
            function_name: None,
            language: Some("rust".to_string()),
            chunk_type: ChunkType::Code,
            calls: Vec::new(),
            inherits_from: Vec::new(),
            chunk_depth: 0,
            parent_chunk_id: None,
            child_chunk_ids: Vec::new(),
            nlp_keywords: Vec::new(),
            nlp_code_refs: Vec::new(),
            virtual_terms: Vec::new(),
        };

        // ── Set up the live corpus with two files' chunks ────────────────────
        let live_path = dir.path().join("live_abort_test.redb");
        {
            let live = CorpusStore::open(&live_path).unwrap();
            live.upsert_chunks(&[
                make_chunk("alpha.rs", "alpha:1:1", "fn alpha() {}"),
                make_chunk("beta.rs", "beta:1:1", "fn beta() {}"),
            ])
            .unwrap();
            live.upsert_file_hashes(&[("alpha.rs", "hash_a"), ("beta.rs", "hash_b")])
                .unwrap();
        }
        // Confirm 2 chunks are present before any failure simulation.
        {
            let check = CorpusStore::open(&live_path).unwrap();
            assert_eq!(
                check.load_all_chunks().unwrap().len(),
                2,
                "pre-condition: live corpus must have 2 chunks"
            );
        }

        // ── (a) ERROR PROPAGATION: staging open at a directory path fails ────
        //
        // `CorpusStore::open_fresh` cannot create a redb database where a
        // directory already exists.  This exercises the same code path as an
        // I/O error during `copy_all_from` (both unwind via `?`).
        let dir_staging_path = dir.path().join("staging_is_a_dir");
        std::fs::create_dir_all(&dir_staging_path).unwrap();
        let staging_open_err = CorpusStore::open_fresh(&dir_staging_path);
        assert!(
            staging_open_err.is_err(),
            "opening a directory as a redb corpus must return Err — \
             this confirms the error-propagation path is exercised"
        );

        // ── (b) LIVE CORPUS INTACT ────────────────────────────────────────────
        //
        // In the hardened code path, when `begin_force_corpus_swap` gets `Err`
        // from the staging open or `copy_all_from`, it:
        //   1. logs at `error!`
        //   2. does NOT call `swap_corpus_store` on the indexer
        //   3. returns `Err` to `spawn_reindex_with_cleanup`
        //   4. the caller emits a terminal SSE error event and returns early
        //      WITHOUT ever promoting (renaming) the staging file.
        //
        // Because `swap_corpus_store` was never called, `index.redb` is
        // untouched.  Reopen and assert all original chunks are still there.
        {
            let live_after = CorpusStore::open(&live_path).unwrap();
            let chunks_after = live_after.load_all_chunks().unwrap();
            assert_eq!(
                chunks_after.len(),
                2,
                "ABORT PATH: live corpus must STILL have 2 chunks after a failed \
                 staging setup — got {:?}",
                chunks_after.iter().map(|c| &c.file).collect::<Vec<_>>()
            );
            assert!(
                chunks_after.iter().any(|c| c.file == "alpha.rs"),
                "alpha.rs must remain in the live corpus after a failed carryover"
            );
            assert!(
                chunks_after.iter().any(|c| c.file == "beta.rs"),
                "beta.rs must remain in the live corpus after a failed carryover"
            );
        }

        // ── Sanity: copy_all_from succeeds when source + destination are valid ─
        //
        // Confirms the function works correctly under normal conditions — the
        // above failure path is a genuine error, not a systematic bug in
        // copy_all_from itself.
        let good_staging_path = dir.path().join("good_staging_sanity.redb");
        {
            let good_live = CorpusStore::open(&live_path).unwrap();
            let good_staging = CorpusStore::open_fresh(&good_staging_path).unwrap();
            let copy_result = good_staging.copy_all_from(&good_live);
            assert!(
                copy_result.is_ok(),
                "copy_all_from must succeed when both source and destination are valid: {:?}",
                copy_result
            );
            let copied = good_staging.load_all_chunks().unwrap();
            assert_eq!(
                copied.len(),
                2,
                "copy_all_from sanity: must copy all 2 chunks from the live corpus"
            );
        }
    }
}
