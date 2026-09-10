//! Reindex orchestrator: top-level task spawn + file-tree walk.
//!
//! Why: the daemon spawns one background `tokio::task` per reindex request.
//! This module contains the public entry points (`spawn_reindex`,
//! `spawn_reindex_with_cleanup`) and the helper that walks + filters the
//! source tree (`collect_files_to_index`). The full three-phase async body
//! lives in `runner.rs`; the RSS pollers live in `pollers.rs`.
//!
//! What: thin shell that resolves `include_paths`, applies config filters,
//! de-duplicates the file list, and delegates to `runner::run_reindex`.
//!
//! Test: `reindex_walks_directory_and_emits_events` is the primary end-to-end
//! coverage. Semaphore prioritisation is covered by
//! `interactive_reindex_not_starved_by_background`.

use crate::core::registry::{IndexHandle, IndexId};
use dashmap::DashMap;
use std::sync::atomic::AtomicU32;
use std::sync::Arc;
use std::time::Instant;

use super::progress::ReindexProgress;
use super::quarantine::ReindexQuarantine;
use super::semaphore::BACKGROUND_QUEUE_DEPTH;

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

/// Test-only variant of [`spawn_reindex`] that hands back the task's
/// `JoinHandle` so tests can `.await` deterministic completion.
///
/// Why: production [`spawn_reindex`] is fire-and-forget (returns `()`), so
/// tests historically polled `progress.status` on a fixed wall-clock budget
/// (`for _ in 0..N { sleep(ms) }`). Under a loaded host the reindex task first
/// queues on the process-global 2-permit interactive semaphore
/// (`semaphore::reindex_semaphore`) and is then CPU-starved, so that budget can
/// elapse *before* the task reaches a terminal status — surfacing as spurious
/// `left: Running, right: Complete/Failed` assertion failures (issue #2730).
/// Awaiting the real `JoinHandle` is a rendezvous with no timing assumption.
/// What: spawns exactly the same task as [`spawn_reindex`] (interactive
/// priority, no cleanup / aborted maps / embedderd pid slot / quarantine) but
/// returns the `tokio::task::JoinHandle`. `run_reindex` always sets a terminal
/// status (`Complete`/`Failed`/`AbortedMemory`) and flushes every side effect
/// (prune, corpus swap, terminal SSE event, stage marks) before returning, so
/// once the handle resolves the status and all observable state are final.
/// Test: `root_hijack_tests` and every terminal-wait test in `reindex::tests`.
#[cfg(test)]
pub(crate) fn spawn_reindex_awaitable(
    handle: Arc<IndexHandle>,
    progress: Arc<ReindexProgress>,
    force: bool,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(super::runner::run_reindex(
        handle, progress, force, None, None, None, true, None,
    ))
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
pub(super) fn collect_files_to_index(handle: &IndexHandle) -> crate::service::walker::WalkResult {
    crate::service::index_admission::walk(handle)
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
    tokio::spawn(super::runner::run_reindex(
        handle,
        progress,
        force,
        cleanup_map,
        aborted_map,
        embedderd_pid_slot,
        priority,
        quarantine,
    ));
}
