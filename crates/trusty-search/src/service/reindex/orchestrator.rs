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
use crate::service::walker::{walk_source_files_with_options, WalkOptions};
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
