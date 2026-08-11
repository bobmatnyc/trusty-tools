//! Boot-time stale-index reconciliation (issues #1670 / #1672).
//!
//! Why: the live filesystem watcher catches changes made while the daemon is
//! running, but there is no hook for changes made while the daemon was DOWN.
//! After a warm-boot restore, every index may be stale. This module closes
//! that gap via three complementary paths:
//!
//! **Never-walked path** (issue #4680, checked first): an index whose lexical
//! stage is still owed and which no walk has touched in this daemon's lifetime
//! is stuck, not up-to-date — it gets a full (non-force, data-preserving)
//! background reindex without consulting either staleness marker below, since
//! both markers describe drift and neither can describe "was never populated".
//!
//! **Git path** (issue #1670): indexes whose `indexed_head_sha` is set are
//! reconciled via `git diff --name-only <stored>..HEAD`. Per-file or full-reindex
//! depending on delta size vs. `FULL_REINDEX_THRESHOLD`.
//!
//! **mtime path** (issue #1672): indexes whose root is CORROBORATED not to be a
//! git repository are reconciled by walking the index root and collecting all
//! files whose mtime exceeds the persisted `last_indexed_unix` timestamp from
//! `indexes.toml`. Same threshold / full-reindex fallback applies. Non-git
//! indexes with no `last_indexed_unix` AND no corpus are skipped (never indexed
//! — nothing to catch up).
//!
//! 🔴 The mtime walk consults only `SKIP_DIRS` and the walker's skip predicates
//! — it does NOT honour `.gitignore`, unlike the reindex walk. Reaching it
//! because a git probe merely FAILED (issue #4733: a stale worktree gitlink,
//! `detected dubious ownership`, an unreadable `.git`) indexed files the repo
//! deliberately excludes and made them retrievable through the `search` and
//! `grep` MCP tools. `reconcile_one_index` therefore gates it behind
//! `core::git::probe_work_tree` and never takes it without a corroborated
//! `NoRepo`.
//!
//! The full reindex is the safe alternative for a reason that does NOT depend on
//! git working: `service::walker` drives the `ignore` crate with
//! `require_git(false)`, so `.gitignore` is applied even when git itself cannot
//! read the repository at all. That is what makes "reindex instead of
//! mtime-walk" a real fix rather than a swap of one blind walk for another.
//!
//! It is not, however, always available: `finish_reindex` re-stamps
//! `indexed_head_sha` from `git::head_sha`, so when THAT is `None` (an unborn
//! HEAD, or a repo git declined to read) a reindex cannot converge — the next
//! boot re-enters the same arm and re-walks the tree, forever. Those indexes are
//! therefore left untouched and counted as `skipped_unresolvable_git`.
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

use crate::core::git::WorkTree;
use crate::core::registry::IndexHandle;
use crate::service::reindex::{spawn_reindex_with_cleanup, ReindexProgress};
use crate::service::server::ReconcileSummary;
use crate::service::walker::{path_in_skipped_dir, should_skip_path};
use crate::service::warm_boot::index_is_stuck_unwalked;
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
/// What: recursively walks the root using `walkdir::filter_entry` to PRUNE
/// excluded directory subtrees (SKIP_DIRS + should_skip_for_reconcile) before
/// descending — directories are never statted beyond the basename check, so
/// `node_modules/`, `target/`, `.git/`, etc. are skipped without traversal.
/// For accepted files, compares mtime (seconds since Unix epoch) to `since_unix`.
/// Returns repo-root-relative paths as strings, capped at
/// `FULL_REINDEX_THRESHOLD + 1` (callers check `> FULL_REINDEX_THRESHOLD` for
/// the fallback decision, so one extra entry is sufficient to trigger it without
/// walking the whole tree).
/// Test: `mtime_walk_finds_newer_file_and_skips_older`,
///       `mtime_walk_skips_excluded_dir`,
///       `mtime_walk_caps_at_threshold_plus_one` in reconcile_tests.rs.
pub(crate) fn collect_stale_files_by_mtime(root_path: &Path, since_unix: u64) -> Vec<String> {
    use walkdir::WalkDir;

    let mut stale: Vec<String> = Vec::new();
    let cap = FULL_REINDEX_THRESHOLD + 1;

    let iter = WalkDir::new(root_path)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            // For directories: prune by basename against SKIP_DIRS so walkdir
            // never descends into excluded subtrees (node_modules, target, .git,
            // etc.). Without pruning, walkdir stats every file inside before the
            // per-file predicate rejects them — causing the boot-thrash described
            // in #1672 reviewer finding 1.
            if e.file_type().is_dir() {
                let name = e.file_name().to_str().unwrap_or("");
                return !crate::service::walker::SKIP_DIRS.contains(&name);
            }
            // For files: apply the same exclusion rules as the live watcher.
            // path_in_skipped_dir handles nested excluded dirs that share a name
            // not in SKIP_DIRS (shouldn't normally occur, but guards correctly).
            let rel = match e.path().strip_prefix(root_path) {
                Ok(r) => r,
                Err(_) => return false,
            };
            !should_skip_for_reconcile(rel)
        });

    for entry in iter.flatten() {
        if stale.len() >= cap {
            break;
        }
        if !entry.file_type().is_file() {
            continue;
        }

        // Build root-relative path (already filtered above, but strip again for
        // the String conversion).
        let rel = match entry.path().strip_prefix(root_path) {
            Ok(r) => r,
            Err(_) => continue,
        };

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

/// RAII guard that clears `ReconcileSummary::in_progress` on drop.
///
/// Why: the joiner task that clears `in_progress` after all per-index tasks
/// finish can be cancelled (daemon shutdown) or panic. Without a drop guard
/// `in_progress` stays `true` forever and `/health` reports a stale status.
/// What: holds a clone of the `Arc<Mutex<ReconcileSummary>>` and sets
/// `in_progress = false` in `Drop::drop`, guaranteeing the flag is cleared
/// regardless of how the joiner exits (normal completion, panic, or
/// cancellation via `tokio::task::abort`).
/// Test: `reconcile_in_progress_clears_after_tasks_complete` in reconcile_tests.rs.
struct InProgressGuard(Arc<std::sync::Mutex<ReconcileSummary>>);

impl Drop for InProgressGuard {
    fn drop(&mut self) {
        if let Ok(mut s) = self.0.lock() {
            s.in_progress = false;
        }
    }
}

/// Kick off background reconciliation for every registered index.
///
/// Why: called once during daemon startup, after `restore_indexes`, so stale
/// indexes are silently caught up without blocking the HTTP listener or the
/// auto-discover scan.
/// What: checks the `TRUSTY_NO_BOOT_RECONCILE` gate; marks the summary
/// `in_progress`; spawns one independent background `tokio::task` per
/// registered index; spawns a bounded joiner task that awaits all per-index
/// handles and then drops `InProgressGuard`, which clears `in_progress` — this
/// guarantees the flag is cleared even on panic or daemon shutdown (drop guard
/// pattern). The joiner task is detached (fire-and-forget) but its panic path
/// is safe: `Drop` runs regardless. Boot is never blocked; queries are served
/// throughout.
/// Test: `reconcile_disabled_gate`, `reconcile_in_progress_clears_after_tasks_complete`
/// in reconcile_tests.rs.
pub async fn reconcile_stale_indexes(state: &SearchAppState) {
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

    // Spawn a joiner task to clear in_progress once all per-index tasks finish.
    // The InProgressGuard ensures in_progress is cleared even if this task
    // panics or is cancelled at daemon shutdown.
    tokio::spawn(async move {
        // Guard clears in_progress=false when dropped (normal exit OR panic).
        let _guard = InProgressGuard(Arc::clone(&summary));
        for h in handles {
            // Absorb per-task JoinErrors (panics): individual task failures are
            // already logged inside reconcile_one_index; a panic there must not
            // prevent other tasks from completing or the guard from running.
            let _ = h.await;
        }
        tracing::debug!("boot reconcile: all tasks complete");
        // _guard drops here → in_progress = false
    });
}

/// Reconcile one stale index in the background.
///
/// Why: each index is independent; background tasks let the daemon serve
/// queries while reconciliation proceeds and avoid one slow index blocking others.
/// Also called on the query-wake path (`server::search`) when an idle-suspended
/// or lazily-restored index is served without a watcher, to catch edits missed
/// while the watcher was off — hence `pub(crate)`.
/// What: first checks the never-walked guard (issue #4680) and re-drives the
/// walk if it fires; otherwise reads the stored + current HEAD SHAs (git path).
/// When either SHA is missing it asks `core::git::probe_work_tree` WHY, and
/// takes the `.gitignore`-blind mtime path only for a corroborated
/// [`WorkTree::NoRepo`] (issue #4733). Otherwise it splits on whether a full
/// reindex could CONVERGE: with a live HEAD it reindexes (one run stamps the
/// SHA); with no HEAD at all — unborn HEAD, or a repo git declined to read —
/// it refuses, because `finish_reindex` would re-stamp the same `None` and
/// re-walk every boot forever. Updates `summary` so `GET /health` reports
/// reconcile progress.
/// Test: `reconcile_up_to_date_index_is_noop`, `reconcile_stale_index_stamps_new_sha`,
/// `stuck_unwalked_git_index_is_retried_not_marked_up_to_date`,
/// `stuck_unwalked_non_git_index_is_retried_not_skipped`,
/// `broken_git_repo_refuses_rather_than_mtime_walking`,
/// `unborn_head_repo_is_refused_once_not_reindexed_every_boot`,
/// `never_stamped_git_repo_with_commits_full_reindexes_and_converges` in
/// reconcile_tests.rs.
pub(crate) async fn reconcile_one_index(
    handle: Arc<IndexHandle>,
    summary: Arc<std::sync::Mutex<ReconcileSummary>>,
) {
    let index_id = handle.id.0.clone();

    // #4680: a never-walked index is stuck, not up-to-date — re-drive the walk
    // before any staleness marker is consulted.
    //
    // Why this must come FIRST: both marker paths below answer "has the source
    // moved since we last looked?", and both answer "no" for an index that was
    // never populated at all. The git path compares a HEAD SHA that restore
    // re-derives from live git (#4391), so `stored == current` holds
    // unconditionally → `up_to_date`. The mtime path reads `last_indexed_unix`,
    // whose writer has no production callers (#4391), so it is always `None` →
    // `skipped_no_data`. Either way the index is recorded as needing nothing,
    // forever: 221 of 222 production indexes sat at `chunk_count = 0` for 44
    // days behind exactly this. Emptiness has to be a reconcile INPUT, not a
    // conclusion drawn from markers that only describe drift.
    //
    // Data safety: `trigger_full_reindex` runs with `force = false`, so the
    // incremental hash cache and the staged-corpus carryover both apply — this
    // re-walks and re-adds, it never clears or rebuilds a corpus from scratch.
    // Recovery here is strictly additive.
    //
    // Bounded: the predicate keys off `last_walk_started_at`, which the reindex
    // runner stamps before it walks, so an index gets at most ONE retry per
    // daemon lifetime and a walk that legitimately finds zero indexable files
    // (`last_walk_error` set, corpus still empty) is never re-driven in a loop.
    if index_is_stuck_unwalked(
        handle.stages.read().await.lexical.status,
        handle
            .walk_diagnostics
            .read()
            .await
            .last_walk_started_at
            .is_some(),
    ) {
        tracing::warn!(
            "reconcile[{index_id}]: index has never been walked in this daemon's \
             lifetime and its lexical stage is still owed — triggering a full \
             background reindex instead of comparing staleness markers (issue #4680)"
        );
        trigger_full_reindex(&handle);
        if let Ok(mut s) = summary.lock() {
            s.stuck_retried += 1;
        }
        return;
    }

    // Read the stored indexed SHA and the current HEAD.
    let stored_sha = handle.indexed_head_sha.read().await.clone();
    let current_sha = crate::core::git::head_sha(&handle.root_path);

    match (stored_sha, current_sha) {
        (Some(stored), Some(current)) => {
            reconcile_git_path(&handle, &index_id, &stored, &current, &summary).await;
        }
        // #4733: the mtime walk does not honour `.gitignore` — only take it once
        // git has CORROBORATED that no repository exists here.
        (_, current) => match crate::core::git::probe_work_tree(&handle.root_path) {
            WorkTree::NoRepo => {
                reconcile_mtime_path(&handle, &index_id, &summary).await;
            }
            // A work tree with commits but no stored SHA: one full reindex
            // stamps `indexed_head_sha` and the next boot converges.
            state if current.is_some() => {
                tracing::warn!(
                    "reconcile[{index_id}]: never stamped, but a git repository could \
                     not be ruled out ({state:?}) — full background reindex (which \
                     honours .gitignore) instead of the mtime walk (issue #4733)"
                );
                trigger_full_reindex(&handle);
                if let Ok(mut s) = summary.lock() {
                    s.fell_back_to_full += 1;
                }
            }
            // #4733: NO HEAD to stamp — an unborn-HEAD repo, or one git declined
            // to read. A full reindex here CANNOT converge: `finish_reindex`
            // re-stamps `indexed_head_sha` from the same `head_sha` that just
            // returned `None`, so the next boot re-enters this arm identically
            // and re-walks the whole tree, forever. Refusing costs nothing the
            // mtime path would have delivered — its only input,
            // `last_indexed_unix`, has no production writer (#4391), so this
            // case has always reported "skipped" in production.
            state => {
                tracing::warn!(
                    "reconcile[{index_id}]: no HEAD SHA and a git repository could not \
                     be ruled out ({state:?}) — leaving this index untouched. The mtime \
                     walk does not honour .gitignore, and a full reindex could not stamp \
                     a SHA so it would repeat every boot. The live watcher still tracks \
                     this index; run `trusty-search index` to rebuild it (issue #4733)"
                );
                if let Ok(mut s) = summary.lock() {
                    s.skipped_unresolvable_git += 1;
                }
            }
        },
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
                // #3049: teardown-lock read side, per call. Reconcile
                // deliberately releases between files, so the guard is scoped
                // the same way — a DELETE waits at most one file, not the
                // whole reconcile pass.
                let _teardown_guard =
                    crate::service::reindex::acquire_index_teardown_read(&handle.id).await;
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
                // #3049: see the index_file arm above.
                let _teardown_guard =
                    crate::service::reindex::acquire_index_teardown_read(&handle.id).await;
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
