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
use std::path::PathBuf;
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

/// The outcome of one multi-root walk (#7434).
///
/// Why: `collect_files_to_index` used to return a bare `WalkResult` because
/// there was exactly one root and "does it exist" was a question `runner.rs`
/// could ask afterwards. With N roots the walk is the only place that knows
/// WHICH root was missing, and a missing additional root must degrade that
/// root's coverage loudly rather than silently contributing zero files.
/// What: the merged walk plus the roots that were absent or not directories.
/// Only a missing PRIMARY root keeps the existing hard-failure path in
/// `runner.rs` — this list carries the additional ones.
/// Test: `walk_records_a_missing_additional_root` in `multi_root_tests.rs`.
pub(super) struct CollectedFiles {
    pub walk: crate::service::walker::WalkResult,
    /// Absolute paths of index roots that did not exist (or were not
    /// directories) at walk time, in table order.
    pub missing_roots: Vec<PathBuf>,
}

/// Walk every index root of `handle`, apply repo-config filters
/// (`exclude_globs`, `extensions`, `path_filter`), and de-duplicate.
///
/// Why: extracted from `spawn_reindex_with_cleanup` (issue #98) so the
/// orchestrator body is dominated by control flow rather than walker plumbing.
/// #7434 widened it from one root to the index's whole root table so an index
/// can span an OKG tree plus one tree per project (#7429).
/// What: adapts [`crate::service::index_admission::walk_roots`], which is where
/// the walk itself lives since #7379 so that the reindex walk, the live watcher
/// admission and the dropped-event rescan cannot disagree about which files an
/// index holds. A missing additional root arrives here in
/// [`CollectedFiles::missing_roots`]; a missing primary root arrives as the
/// empty walk `runner.rs` already turns into a failure.
/// Test: `reindex_honours_include_paths_filter` below;
/// `walk_covers_every_index_root` and `walk_records_a_missing_additional_root`
/// in `multi_root_tests.rs`.
pub(super) fn collect_files_to_index(handle: &IndexHandle) -> CollectedFiles {
    let walked = crate::service::index_admission::walk_roots(handle);
    CollectedFiles {
        walk: walked.result,
        missing_roots: walked.missing_roots,
    }
}

/// Record this walk's per-root coverage gaps on the handle's diagnostics (#7434).
///
/// Why: `runner.rs` sits at 491 of its 500-SLOC cap, and the per-root
/// bookkeeping belongs beside the walk that produced it. Keeping the write here
/// costs `runner.rs` one call instead of a block.
/// What: stores the missing roots as display strings and, when any are missing,
/// logs one `warn!` naming them — a reindex that quietly covers fewer trees
/// than the operator configured is the failure this makes visible. A missing
/// PRIMARY root is deliberately NOT special-cased here: it produces a zero-file
/// walk, and `runner.rs`'s existing `last_walk_error` path already reports it.
/// Test: `walk_records_a_missing_additional_root` in `multi_root_tests.rs`.
pub(super) async fn record_root_diagnostics(
    handle: &IndexHandle,
    index_id: &IndexId,
    missing_roots: &[PathBuf],
) {
    let missing: Vec<String> = missing_roots
        .iter()
        .map(|p| p.display().to_string())
        .collect();
    if !missing.is_empty() {
        tracing::warn!(
            "reindex[{}]: {} index root(s) absent at walk time — coverage is \
             degraded for: {}",
            index_id.0,
            missing.len(),
            missing.join(", "),
        );
    }
    handle.walk_diagnostics.write().await.missing_index_roots = missing;
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
