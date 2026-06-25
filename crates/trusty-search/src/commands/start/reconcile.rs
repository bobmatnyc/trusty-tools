//! Boot-time stale-index reconciliation (issues #1670 / #1672).
//!
//! Why: the live filesystem watcher catches changes made while the daemon is
//! running, but there is no hook for changes made while the daemon was DOWN.
//! After a warm-boot restore, every index may be stale. This module closes
//! that gap via two complementary paths:
//!
//! **Git path** (issue #1670): indexes whose `indexed_head_sha` is set are
//! reconciled via `git diff --name-only <stored>..HEAD`. Per-file or full-reindex
//! depending on delta size vs. `FULL_REINDEX_THRESHOLD`.
//!
//! **mtime path** (issue #1672): indexes whose root is not a git repo (or whose
//! `indexed_head_sha` is None) are reconciled by walking the index root and
//! collecting all files whose mtime exceeds the persisted `last_indexed_unix`
//! timestamp from `indexes.toml`. Same threshold / full-reindex fallback applies.
//! Non-git indexes with no `last_indexed_unix` AND no corpus are skipped (never
//! indexed — nothing to catch up).
//!
//! Both paths reuse the walker's `path_in_skipped_dir` / `should_skip_path`
//! predicates so exclusion rules are applied consistently.
//!
//! Boot-reconcile progress is surfaced on `GET /health` via `ReconcileSummary`
//! on `SearchAppState` (issue #1672).
//!
//! Gated by `TRUSTY_NO_BOOT_RECONCILE=1` (disables) following the pattern of
//! `TRUSTY_DISABLE_WATCHER` and `TRUSTY_NO_AUTO_DISCOVER`.
//!
//! Test: unit + integration tests in `commands/start/reconcile_tests.rs`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::core::registry::IndexHandle;
use crate::service::reindex::{spawn_reindex_with_cleanup, ReindexProgress};
use crate::service::server::ReconcileSummary;
use crate::service::walker::{path_in_skipped_dir, should_skip_path};
use crate::service::SearchAppState;

/// Maximum number of changed files before we fall back to a full reindex
/// instead of per-file reconciliation.
///
/// Why: per-file reconciliation is cheap for a handful of modified files
/// (the common case after a short daemon downtime) but becomes expensive and
/// thrash-prone when catching up weeks of commits on a large repo. A full
/// background reindex is more efficient and uses the existing semaphore/queue
/// machinery for backpressure.
pub const FULL_REINDEX_THRESHOLD: usize = 250;

/// Environment variable that disables boot-time reconciliation entirely.
///
/// Why: operators on very large or frequently-reindexed repos may prefer to
/// kick off explicit reindexes rather than pay the git-diff cost at boot.
/// Set to `"1"` to disable; any other value (or unset) enables reconciliation.
const NO_BOOT_RECONCILE_ENV: &str = "TRUSTY_NO_BOOT_RECONCILE";

/// Pure gate function for the reconcile opt-out: returns `true` only for
/// `Some("1")`.
///
/// Why: unit-testable without mutating global env state, following the pattern
/// of `watcher_disabled_for_value` in `watcher_manager.rs`.
/// What: returns `true` only when `val == Some("1")`; any other value (unset,
/// `"0"`, `"true"`, empty) leaves reconciliation enabled.
/// Test: `reconcile_disabled_for_value_only_matches_one` in reconcile_tests.rs.
pub(crate) fn reconcile_disabled_for_value(val: Option<&str>) -> bool {
    val == Some("1")
}

/// Compute the set of files changed between `old_sha` and HEAD.
///
/// Why: extracted for testability and to isolate the git fallback decision.
/// Returns `None` when git is unavailable, the repo is not a git repo, or
/// `old_sha` is unknown to local history (history rewrite / forced push) —
/// callers fall back to a full reindex on `None`.
/// What: shells out asynchronously to
/// `git diff --name-only --diff-filter=ACDMRT <old_sha>..HEAD` in `root_path`.
/// Uses `tokio::process::Command` so the Tokio worker thread is not blocked
/// while git runs (fix #1671 reviewer finding — was `std::process::Command`).
/// The `--diff-filter` keeps Added, Copied, Deleted, Modified, Renamed, and
/// Type-changed files; Unmerged (U) and Unknown (X) are excluded.
/// Returns the non-empty lines as a `Vec<String>` of repo-root-relative paths.
/// Test: `changed_files_between_returns_none_outside_git_repo`,
///       `changed_files_between_finds_modified_file`,
///       `changed_files_between_returns_none_for_unknown_sha`
///       in reconcile_tests.rs.
pub(crate) async fn changed_files_between(root_path: &Path, old_sha: &str) -> Option<Vec<String>> {
    let out = tokio::process::Command::new("git")
        .args([
            "diff",
            "--name-only",
            "--diff-filter=ACDMRT",
            &format!("{old_sha}..HEAD"),
        ])
        .current_dir(root_path)
        .output()
        .await
        .ok()?;

    if !out.status.success() {
        tracing::debug!(
            "reconcile: git diff --name-only failed (old_sha={}) in {}: exit={:?}",
            old_sha,
            root_path.display(),
            out.status.code(),
        );
        return None;
    }
    let body = std::str::from_utf8(&out.stdout).ok()?;
    let files: Vec<String> = body
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect();
    Some(files)
}

/// Walk `root_path` and collect relative paths of files whose mtime > `since_unix`.
///
/// Why: for non-git indexes the only staleness signal is file mtime vs. the
/// last-indexed timestamp persisted in `indexes.toml`. This is bounded by the
/// same `FULL_REINDEX_THRESHOLD` used by the git path so large trees cannot
/// cause boot thrash.
/// What: recursively walks the root using `walkdir`, skips directories and
/// file paths excluded by the walker predicates (`path_in_skipped_dir` /
/// `should_skip_path`), compares each file's `mtime` (seconds since Unix epoch)
/// to `since_unix`. Returns repo-root-relative paths as strings, capped at
/// `FULL_REINDEX_THRESHOLD + 1` (callers check `> FULL_REINDEX_THRESHOLD` for
/// the fallback decision, so one extra entry is sufficient to trigger it without
/// walking the whole tree).
/// Test: `mtime_walk_finds_newer_file`, `mtime_walk_skips_older_file`,
///       `mtime_walk_skips_excluded_dir` in reconcile_tests.rs.
pub(crate) fn collect_stale_files_by_mtime(root_path: &Path, since_unix: u64) -> Vec<String> {
    use walkdir::WalkDir;

    let mut stale: Vec<String> = Vec::new();
    let cap = FULL_REINDEX_THRESHOLD + 1;

    for entry in WalkDir::new(root_path)
        .follow_links(false)
        .into_iter()
        .flatten()
    {
        if stale.len() >= cap {
            break;
        }

        let ft = entry.file_type();
        if ft.is_dir() {
            // Prune excluded directories so walkdir doesn't descend into them.
            let dir_name = entry.path().file_name().and_then(|n| n.to_str());
            if let Some(name) = dir_name {
                if crate::service::walker::SKIP_DIRS.contains(&name) {
                    // Note: WalkDir `into_iter().filter_entry()` is the preferred
                    // way, but `flatten()` doesn't compose with it; we rely on the
                    // basename check here instead. The `path_in_skipped_dir` check
                    // below catches nested excluded dirs.
                    continue;
                }
            }
            continue;
        }
        if !ft.is_file() {
            continue;
        }

        // Build root-relative path for skip predicate checks.
        let rel = match entry.path().strip_prefix(root_path) {
            Ok(r) => r,
            Err(_) => continue,
        };

        if should_skip_for_reconcile(rel) {
            continue;
        }

        let mtime_secs = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);

        if mtime_secs > since_unix {
            if let Some(s) = rel.to_str() {
                stale.push(s.to_owned());
            }
        }
    }

    stale
}

/// Returns `true` when `path` should be excluded from incremental reconciliation.
///
/// Why: a file that appears in the git diff but lives inside an excluded subtree
/// (e.g. `node_modules/`, `target/`, minified JS) must not enter the index.
/// We share the walker's `should_skip_path` / `path_in_skipped_dir` predicates
/// so the exclusion rules are applied consistently with the live watcher.
/// What: delegates to the walker's two public skip predicates.
/// Test: `reconcile_skip_excluded_path` in reconcile_tests.rs.
pub(crate) fn should_skip_for_reconcile(path: &Path) -> bool {
    path_in_skipped_dir(path) || should_skip_path(path)
}

/// Kick off background reconciliation for every registered index.
///
/// Why: called once during daemon startup, after `restore_indexes`, so stale
/// indexes are silently caught up without blocking the HTTP listener or the
/// auto-discover scan.
/// What: checks the `TRUSTY_NO_BOOT_RECONCILE` gate; marks the summary
/// `in_progress`; spawns one independent background `tokio::task` per
/// registered index; clears `in_progress` after all tasks are spawned (tasks
/// themselves run asynchronously, so counts reflect dispatch, not completion).
/// To report completion accurately each spawned task writes into the shared
/// summary directly.
/// Test: `reconcile_disabled_gate` in reconcile_tests.rs.
pub(super) async fn reconcile_stale_indexes(state: &SearchAppState) {
    let raw = std::env::var(NO_BOOT_RECONCILE_ENV).ok();
    if reconcile_disabled_for_value(raw.as_deref()) {
        tracing::info!("boot reconcile: disabled via {NO_BOOT_RECONCILE_ENV}=1 — skipping");
        return;
    }

    let index_ids = state.registry.list();
    if index_ids.is_empty() {
        return;
    }

    tracing::info!(
        "boot reconcile: checking {} registered index(es) for staleness (threshold={} files, \
         disable with {NO_BOOT_RECONCILE_ENV}=1)",
        index_ids.len(),
        FULL_REINDEX_THRESHOLD,
    );

    // Mark reconcile as in-progress before spawning tasks.
    let summary = Arc::clone(&state.reconcile_summary);
    if let Ok(mut s) = summary.lock() {
        s.in_progress = true;
    }

    let total = index_ids.len();
    let mut handles = Vec::with_capacity(total);

    for index_id in index_ids {
        let Some(handle) = state.registry.get(&index_id) else {
            continue;
        };
        let summary_clone = Arc::clone(&summary);
        handles.push(tokio::spawn(reconcile_one_index(handle, summary_clone)));
    }

    // Spawn a task to clear in_progress once all per-index tasks are done.
    tokio::spawn(async move {
        for h in handles {
            let _ = h.await;
        }
        if let Ok(mut s) = summary.lock() {
            s.in_progress = false;
        }
        tracing::debug!("boot reconcile: all tasks complete");
    });
}

/// Reconcile one stale index in the background.
///
/// Why: each index is independent; background tasks let the daemon serve
/// queries while reconciliation proceeds and avoid one slow index blocking others.
/// What: reads the stored + current HEAD SHAs (git path), or falls back to the
/// mtime path for non-git / missing-SHA indexes. Updates `summary` with the
/// outcome so `GET /health` reports boot-reconcile progress.
/// Test: `reconcile_up_to_date_index_is_noop`, `reconcile_stale_index_stamps_new_sha`
/// in reconcile_tests.rs.
async fn reconcile_one_index(
    handle: Arc<IndexHandle>,
    summary: Arc<std::sync::Mutex<ReconcileSummary>>,
) {
    let index_id = handle.id.0.clone();

    // Read the stored indexed SHA and the current HEAD.
    let stored_sha = handle.indexed_head_sha.read().await.clone();
    let current_sha = crate::core::git::head_sha(&handle.root_path);

    match (stored_sha, current_sha) {
        (Some(stored), Some(current)) => {
            reconcile_git_path(&handle, &index_id, &stored, &current, &summary).await;
        }
        _ => {
            // Non-git root or missing stored SHA: fall back to mtime-based catch-up.
            reconcile_mtime_path(&handle, &index_id, &summary).await;
        }
    }
}

/// Git-backed reconciliation: compare stored SHA vs. current HEAD.
///
/// Why: the authoritative fast path for git-backed indexes; git diff is O(delta)
/// and doesn't require a full tree walk.
/// What: if SHAs match → up-to-date; else compute diff and apply delta or
/// trigger full reindex.
/// Test: `reconcile_up_to_date_index_is_noop`, `reconcile_stale_index_stamps_new_sha`.
async fn reconcile_git_path(
    handle: &Arc<IndexHandle>,
    index_id: &str,
    stored: &str,
    current: &str,
    summary: &Arc<std::sync::Mutex<ReconcileSummary>>,
) {
    if stored == current {
        tracing::debug!(
            "reconcile[{index_id}]: up to date (sha={})",
            &current[..current.len().min(12)]
        );
        if let Ok(mut s) = summary.lock() {
            s.up_to_date += 1;
        }
        return;
    }

    tracing::info!(
        "reconcile[{index_id}]: stale — stored={} current={}; computing delta",
        &stored[..stored.len().min(12)],
        &current[..current.len().min(12)],
    );

    match changed_files_between(&handle.root_path, stored).await {
        None => {
            tracing::warn!(
                "reconcile[{index_id}]: git diff unavailable for old_sha={} \
                 (unknown to history, or git not available) — full background reindex",
                &stored[..stored.len().min(12)],
            );
            trigger_full_reindex(handle);
            if let Ok(mut s) = summary.lock() {
                s.fell_back_to_full += 1;
            }
        }
        Some(files) if files.len() > FULL_REINDEX_THRESHOLD => {
            tracing::info!(
                "reconcile[{index_id}]: delta too large ({} files > threshold {}) \
                 — full background reindex",
                files.len(),
                FULL_REINDEX_THRESHOLD,
            );
            trigger_full_reindex(handle);
            if let Ok(mut s) = summary.lock() {
                s.fell_back_to_full += 1;
            }
        }
        Some(files) => {
            let file_count = files.len();
            tracing::info!(
                "reconcile[{index_id}]: applying per-file delta ({} file(s))",
                file_count
            );
            let ok = apply_delta(handle, index_id, &files, current).await;
            if let Ok(mut s) = summary.lock() {
                if ok {
                    s.delta_reindexed += 1;
                    s.files_reconciled += file_count;
                } else {
                    s.degraded = true;
                }
            }
        }
    }
}

/// mtime-based reconciliation for non-git / missing-SHA indexes.
///
/// Why: non-git directories (archived tarballs, vendored trees, mounted docs)
/// don't have a SHA timeline but their files do have mtimes. Comparing file
/// mtime to the persisted `last_indexed_unix` (from `indexes.toml`) gives a
/// bounded catch-up without a full reindex on every boot.
/// What: reads `last_indexed_unix` from the persisted registry. If None, skips
/// with a log (never indexed). Otherwise walks the root collecting files newer
/// than `last_indexed_unix`; falls back to full reindex when count exceeds
/// `FULL_REINDEX_THRESHOLD`; else applies a per-file delta.
/// Test: `mtime_reconcile_finds_newer_files`, `mtime_reconcile_skips_never_indexed`
/// and `mtime_reconcile_falls_back_on_large_delta` in reconcile_tests.rs.
async fn reconcile_mtime_path(
    handle: &Arc<IndexHandle>,
    index_id: &str,
    summary: &Arc<std::sync::Mutex<ReconcileSummary>>,
) {
    // Read persisted last_indexed_unix from indexes.toml.
    let last_indexed_unix = read_last_indexed_unix_for_index(index_id);

    match last_indexed_unix {
        None => {
            // No persisted last_indexed_unix: this index was never fully indexed
            // (pre-dates timestamp persistence, or was indexed with an older
            // daemon that did not write the field). Skip with a clear log so
            // operators know to re-run `trusty-search index` explicitly.
            tracing::debug!(
                "reconcile[{index_id}]: skipping non-git index with no \
                 last_indexed_unix — run `trusty-search index` to build an \
                 initial index and enable future mtime-based catch-ups"
            );
            if let Ok(mut s) = summary.lock() {
                s.skipped_no_data += 1;
            }
        }
        Some(since_unix) => {
            tracing::debug!(
                "reconcile[{index_id}]: mtime scan (non-git); last_indexed_unix={}",
                since_unix
            );
            // Walk the root synchronously (blocking I/O in a spawn_blocking).
            let root = handle.root_path.clone();
            let stale_files = tokio::task::spawn_blocking(move || {
                collect_stale_files_by_mtime(&root, since_unix)
            })
            .await
            .unwrap_or_default();

            if stale_files.len() > FULL_REINDEX_THRESHOLD {
                tracing::info!(
                    "reconcile[{index_id}]: mtime delta too large ({} files > threshold {}) \
                     — full background reindex",
                    stale_files.len(),
                    FULL_REINDEX_THRESHOLD,
                );
                trigger_full_reindex(handle);
                if let Ok(mut s) = summary.lock() {
                    s.fell_back_to_full += 1;
                }
            } else if stale_files.is_empty() {
                tracing::debug!("reconcile[{index_id}]: mtime scan — no stale files found");
                if let Ok(mut s) = summary.lock() {
                    s.up_to_date += 1;
                }
            } else {
                let file_count = stale_files.len();
                tracing::info!(
                    "reconcile[{index_id}]: mtime catch-up: {} stale file(s)",
                    file_count
                );
                // Use now as the new timestamp sentinel; stamp_handle will
                // write last_indexed_at but not last_indexed_unix (that is
                // written by the full reindex finish path). For mtime-reconcile
                // we stamp last_indexed_at to mark the handle as recently-synced
                // so the stale-results signal clears.
                let now_sha = format!("mtime:{}", since_unix);
                let ok = apply_delta(handle, index_id, &stale_files, &now_sha).await;
                if let Ok(mut s) = summary.lock() {
                    if ok {
                        s.delta_reindexed += 1;
                        s.files_reconciled += file_count;
                    } else {
                        s.degraded = true;
                    }
                }
            }
        }
    }
}

/// Read `last_indexed_unix` from the persisted registry for one index id.
///
/// Why: `IndexHandle` does not carry `last_indexed_unix` in memory (it lives
/// on `PersistedIndex`). At reconcile time (warm-boot) we need the on-disk
/// value. A one-shot load of `indexes.toml` is cheap and doesn't block a hot
/// path.
/// What: calls `load_index_registry`, finds the entry by id, returns
/// `last_indexed_unix`. Returns `None` on any error or if the entry is absent.
/// Test: exercised transitively by `mtime_reconcile_*` tests.
fn read_last_indexed_unix_for_index(index_id: &str) -> Option<u64> {
    let entries = crate::service::persistence::load_index_registry().ok()?;
    entries
        .into_iter()
        .find(|e| e.id == index_id)
        .and_then(|e| e.last_indexed_unix)
}

/// Trigger a full background reindex for a handle.
///
/// Why: when the delta is too large or git history is unavailable, a full
/// reindex is the safest recovery path.
/// What: allocates a fresh `ReindexProgress` and calls
/// `spawn_reindex_with_cleanup` with `priority=false` (background semaphore)
/// so it does not compete with interactive reindex requests.
/// No boot-loop risk (normal path): `spawn_reindex_with_cleanup` →
/// `finish_reindex` stamps `indexed_head_sha = git::head_sha(root)` and
/// `last_indexed_at = now()` on successful completion (see
/// `service/reindex/finish.rs` lines ~321-324). The next boot therefore sees
/// `stored == current` and skips reconciliation.
/// Follow-up (not implemented here): if the spawned full reindex ITSELF
/// persistently fails (corpus locked, indexer broken) the SHA stays unstamped
/// and a full reindex may be retried each boot — this is a pre-existing
/// property of `spawn_reindex_with_cleanup`, tracked as a follow-up.
/// Test: exercised by the `None`/threshold fallback paths in
/// `reconcile_one_index`.
fn trigger_full_reindex(handle: &Arc<IndexHandle>) {
    let progress = Arc::new(ReindexProgress::new());
    spawn_reindex_with_cleanup(
        Arc::clone(handle),
        progress,
        false, // not force — incremental skip-hash still applies
        None,
        None,
        None,
        false, // background priority
        None,
    );
}

/// Apply per-file reconciliation for a small delta.
///
/// Why: for a handful of changed files this is faster and less disruptive than
/// a full reindex — it reuses the same `index_file` / `remove_file` API the
/// HTTP handler and filesystem watcher use.
///
/// What: for each repo-relative path in `files`:
/// if the file exists on disk and passes skip rules → `indexer.index_file`;
/// if the file is gone → `indexer.remove_file` (removes all its chunks).
/// The indexer read-lock is acquired and dropped per-file so concurrent HTTP
/// reindex requests (which need a write lock) are not blocked for the entire
/// batch duration. This mirrors the locking discipline in
/// `service/watch_loop.rs::handle_modified` (acquire → single async call → drop).
/// Stamps `indexed_head_sha = new_sha` and `last_indexed_at = now` only when at
/// least one file operation succeeded (`indexed > 0 || removed > 0`). If the
/// delta was non-empty but every operation errored, the SHA is left unstamped so
/// the next boot retries reconciliation instead of silently marking it complete.
/// Returns `true` if at least one operation succeeded (stamp happened), `false`
/// on total failure.
///
/// Test: `reconcile_stale_index_stamps_new_sha`,
///       `apply_delta_total_failure_does_not_stamp` in reconcile_tests.rs.
async fn apply_delta(
    handle: &Arc<IndexHandle>,
    index_id: &str,
    files: &[String],
    new_sha: &str,
) -> bool {
    let root = &handle.root_path;
    let mut indexed = 0usize;
    let mut removed = 0usize;
    let mut skipped = 0usize;
    let mut failed = 0usize;

    for rel_path_str in files {
        let abs_path = root.join(rel_path_str);
        let rel_as_path = PathBuf::from(rel_path_str);

        // Apply the same exclusion rules as the live watcher.
        if should_skip_for_reconcile(&rel_as_path) {
            tracing::debug!("reconcile[{index_id}]: skip excluded path {rel_path_str}");
            skipped += 1;
            continue;
        }

        if abs_path.exists() && abs_path.is_file() {
            // Modified or added: reindex the file.
            let content = match tokio::fs::read_to_string(&abs_path).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::debug!("reconcile[{index_id}]: skip unreadable {rel_path_str}: {e}");
                    skipped += 1;
                    continue;
                }
            };
            // Acquire and drop per-call: gives writers a window between files.
            let result = {
                let idx = handle.indexer.read().await;
                idx.index_file(rel_path_str, &content).await
            };
            match result {
                Ok(()) => indexed += 1,
                Err(e) => {
                    failed += 1;
                    tracing::warn!(
                        "reconcile[{index_id}]: index_file failed for \
                         {rel_path_str}: {e}"
                    );
                }
            }
        } else {
            // Deleted: remove all chunks for this file.
            // Acquire and drop per-call so write-lock contention is bounded.
            let result = {
                let idx = handle.indexer.read().await;
                idx.remove_file(rel_path_str).await
            };
            match result {
                Ok(n) if n > 0 => removed += n,
                Ok(_) => skipped += 1,
                Err(e) => {
                    failed += 1;
                    tracing::warn!(
                        "reconcile[{index_id}]: remove_file failed for \
                         {rel_path_str}: {e}"
                    );
                }
            }
        }
    }

    // Only stamp the new HEAD SHA when at least one operation succeeded.
    // If every non-skipped file errored (total failure), leave the SHA
    // unstamped so the next boot retries rather than silently marking the
    // reconcile complete with a stale index.
    if !files.is_empty() && indexed == 0 && removed == 0 && failed > 0 {
        tracing::warn!(
            "reconcile[{index_id}]: total failure — {failed} error(s), \
             {skipped} skipped — SHA NOT stamped; next boot will retry \
             (new_sha={})",
            &new_sha[..new_sha.len().min(12)],
        );
        return false;
    }

    if failed > 0 {
        tracing::warn!(
            "reconcile[{index_id}]: partial failure — {failed} error(s); \
             stamping SHA anyway (indexed={indexed} removed_chunks={removed})"
        );
    }

    // Stamp the new HEAD SHA and timestamp so the staleness signal clears.
    stamp_handle(handle, new_sha).await;

    tracing::info!(
        "reconcile[{index_id}]: complete — indexed={indexed} removed_chunks={removed} \
         skipped={skipped} failed={failed} new_sha={}",
        &new_sha[..new_sha.len().min(12)],
    );

    true
}

/// Stamp `indexed_head_sha = new_sha` and `last_indexed_at = now` on the handle.
///
/// Why: after a successful per-file reconciliation the staleness signal
/// (`results_may_be_stale` in the search response) must clear so callers are
/// no longer warned about outdated results.
/// What: acquires write locks on both `Arc<RwLock<Option<String>>>` fields and
/// writes the new values. Uses `chrono::Utc::now().to_rfc3339()` for the
/// timestamp — the same call used by `service::reindex::stages::now_rfc3339` —
/// so the format is consistent with the reindex pipeline and correct for all
/// calendar dates (no hand-rolled Gregorian approximation).
/// Test: `reconcile_stamps_head_sha_after_delta` in reconcile_tests.rs.
pub(crate) async fn stamp_handle(handle: &Arc<IndexHandle>, new_sha: &str) {
    *handle.indexed_head_sha.write().await = Some(new_sha.to_owned());
    *handle.last_indexed_at.write().await = Some(chrono::Utc::now().to_rfc3339());
}

// Tests live in the sibling file so that reconcile.rs stays under the 500-SLOC
// production cap. The `_tests.rs` suffix gives the file the 1500-SLOC test cap.
#[cfg(test)]
#[path = "reconcile_tests.rs"]
mod tests;
