//! Boot-time stale-index reconciliation (issue #1670).
//!
//! Why: the live filesystem watcher catches changes made while the daemon is
//! running, but there is no hook for changes made while the daemon was DOWN.
//! After a warm-boot restore, every index that has a stored `indexed_head_sha`
//! may be stale — the working tree advanced past that commit and the index was
//! never told. This module closes that gap by computing the git diff between the
//! stored SHA and the current HEAD and reindexing only the changed files, or
//! falling back to a full `spawn_reindex` when the delta is too large or git
//! history is unavailable.
//!
//! What: `reconcile_stale_indexes` iterates over every registered index and
//! spawns one background `tokio::task` per index that:
//!   1. Reads `indexed_head_sha` and calls `core::git::head_sha`.
//!   2. If both SHAs are present and differ, calls `changed_files_between` to
//!      compute the delta via `git diff --name-only <old>..HEAD`.
//!   3. If the delta exceeds `FULL_REINDEX_THRESHOLD` files, falls back to
//!      `spawn_reindex_with_cleanup` (full background reindex).
//!   4. Otherwise, walks the delta: modified/added files get `indexer.index_file`;
//!      deleted files get `indexer.remove_file` (same API the HTTP handler uses).
//!   5. Stamps `indexed_head_sha` = current HEAD and `last_indexed_at` = now.
//!
//! Gated by `TRUSTY_NO_BOOT_RECONCILE=1` (disables) following the pattern of
//! `TRUSTY_DISABLE_WATCHER` and `TRUSTY_NO_AUTO_DISCOVER`.
//!
//! Non-git indexes (or indexes whose `indexed_head_sha` is `None`) are skipped
//! with a `tracing::debug!` log. A follow-up ticket (#1671) should add
//! mtime-based reconciliation for non-git indexes.
//!
//! Test: unit + integration tests in `commands/start/reconcile_tests.rs`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::core::registry::IndexHandle;
use crate::service::reindex::{spawn_reindex_with_cleanup, ReindexProgress};
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
/// What: checks the `TRUSTY_NO_BOOT_RECONCILE` gate; then spawns one
/// independent background `tokio::task` per registered index.
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

    for index_id in index_ids {
        let Some(handle) = state.registry.get(&index_id) else {
            continue;
        };
        tokio::spawn(reconcile_one_index(handle));
    }
}

/// Reconcile one stale index in the background.
///
/// Why: each index is independent; background tasks let the daemon serve
/// queries while reconciliation proceeds and avoid one slow index blocking others.
/// What: reads the stored + current HEAD SHAs, computes the diff, then either
/// runs per-file reconciliation or falls back to a full background reindex.
/// Test: `reconcile_up_to_date_index_is_noop`, `reconcile_stale_index_stamps_new_sha`
/// in reconcile_tests.rs.
async fn reconcile_one_index(handle: Arc<IndexHandle>) {
    let index_id = handle.id.0.clone();

    // Read the stored indexed SHA and the current HEAD.
    let stored_sha = handle.indexed_head_sha.read().await.clone();
    let current_sha = crate::core::git::head_sha(&handle.root_path);

    let (stored, current) = match (stored_sha, current_sha) {
        (Some(s), Some(c)) => (s, c),
        (None, _) => {
            tracing::debug!(
                "reconcile[{index_id}]: skipping — no stored indexed_head_sha \
                 (non-git index or never indexed; follow-up #1671 for mtime-based scan)"
            );
            return;
        }
        (_, None) => {
            tracing::debug!(
                "reconcile[{index_id}]: skipping — root_path is not a git repo \
                 ({})",
                handle.root_path.display()
            );
            return;
        }
    };

    if stored == current {
        tracing::debug!(
            "reconcile[{index_id}]: up to date (sha={})",
            &current[..current.len().min(12)]
        );
        return;
    }

    tracing::info!(
        "reconcile[{index_id}]: stale — stored={} current={}; computing delta",
        &stored[..stored.len().min(12)],
        &current[..current.len().min(12)],
    );

    match changed_files_between(&handle.root_path, &stored).await {
        None => {
            tracing::warn!(
                "reconcile[{index_id}]: git diff unavailable for old_sha={} \
                 (unknown to history, or git not available) — full background reindex",
                &stored[..stored.len().min(12)],
            );
            trigger_full_reindex(&handle);
        }
        Some(files) if files.len() > FULL_REINDEX_THRESHOLD => {
            tracing::info!(
                "reconcile[{index_id}]: delta too large ({} files > threshold {}) \
                 — full background reindex",
                files.len(),
                FULL_REINDEX_THRESHOLD,
            );
            trigger_full_reindex(&handle);
        }
        Some(files) => {
            tracing::info!(
                "reconcile[{index_id}]: applying per-file delta ({} file(s))",
                files.len()
            );
            apply_delta(&handle, &index_id, &files, &current).await;
        }
    }
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
///
/// Test: `reconcile_stale_index_stamps_new_sha`,
///       `apply_delta_total_failure_does_not_stamp` in reconcile_tests.rs.
async fn apply_delta(handle: &Arc<IndexHandle>, index_id: &str, files: &[String], new_sha: &str) {
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
            "reconcile[{index_id}]: total failure — {failed} error(s),              {skipped} skipped — SHA NOT stamped; next boot will retry              (new_sha={})",
            &new_sha[..new_sha.len().min(12)],
        );
        return;
    }

    if failed > 0 {
        tracing::warn!(
            "reconcile[{index_id}]: partial failure — {failed} error(s);              stamping SHA anyway (indexed={indexed} removed_chunks={removed})"
        );
    }

    // Stamp the new HEAD SHA and timestamp so the staleness signal clears.
    stamp_handle(handle, new_sha).await;

    tracing::info!(
        "reconcile[{index_id}]: complete — indexed={indexed} removed_chunks={removed} \
         skipped={skipped} failed={failed} new_sha={}",
        &new_sha[..new_sha.len().min(12)],
    );
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
