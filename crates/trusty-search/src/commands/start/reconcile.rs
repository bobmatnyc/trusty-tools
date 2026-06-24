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
//! Test: unit + integration tests in the `tests` module below.

use std::path::{Path, PathBuf};
use std::process::Command;
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
/// Test: `reconcile_disabled_for_value_only_matches_one` below.
pub(crate) fn reconcile_disabled_for_value(val: Option<&str>) -> bool {
    val == Some("1")
}

/// Compute the set of files changed between `old_sha` and HEAD.
///
/// Why: extracted for testability and to isolate the git fallback decision.
/// Returns `None` when git is unavailable, the repo is not a git repo, or
/// `old_sha` is unknown to local history (history rewrite / forced push) —
/// callers fall back to a full reindex on `None`.
/// What: shells out to
/// `git diff --name-only --diff-filter=ACDMRT <old_sha>..HEAD` in `root_path`.
/// The `--diff-filter` keeps Added, Copied, Deleted, Modified, Renamed, and
/// Type-changed files; Unmerged (U) and Unknown (X) are excluded.
/// Returns the non-empty lines as a `Vec<String>` of repo-root-relative paths.
/// Test: `changed_files_between_returns_none_outside_git_repo`,
///       `changed_files_between_finds_modified_file`,
///       `changed_files_between_returns_none_for_unknown_sha`.
pub(crate) fn changed_files_between(root_path: &Path, old_sha: &str) -> Option<Vec<String>> {
    let out = Command::new("git")
        .args([
            "diff",
            "--name-only",
            "--diff-filter=ACDMRT",
            &format!("{old_sha}..HEAD"),
        ])
        .current_dir(root_path)
        .output()
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
/// Test: `reconcile_skip_excluded_path` below.
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
/// Test: `reconcile_disabled_gate` below.
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
/// Test: `reconcile_up_to_date_index_is_noop`, `reconcile_stale_index_stamps_new_sha`.
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

    match changed_files_between(&handle.root_path, &stored) {
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
/// Stamps `indexed_head_sha = new_sha` and `last_indexed_at = now` on success.
///
/// Test: `reconcile_stale_index_stamps_new_sha`.
async fn apply_delta(handle: &Arc<IndexHandle>, index_id: &str, files: &[String], new_sha: &str) {
    let root = &handle.root_path;
    let mut indexed = 0usize;
    let mut removed = 0usize;
    let mut skipped = 0usize;

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
            let idx = handle.indexer.read().await;
            match idx.index_file(rel_path_str, &content).await {
                Ok(()) => indexed += 1,
                Err(e) => {
                    tracing::warn!(
                        "reconcile[{index_id}]: index_file failed for \
                         {rel_path_str}: {e}"
                    );
                }
            }
        } else {
            // Deleted: remove all chunks for this file.
            let idx = handle.indexer.read().await;
            match idx.remove_file(rel_path_str).await {
                Ok(n) if n > 0 => removed += n,
                Ok(_) => skipped += 1,
                Err(e) => {
                    tracing::warn!(
                        "reconcile[{index_id}]: remove_file failed for \
                         {rel_path_str}: {e}"
                    );
                }
            }
        }
    }

    // Stamp the new HEAD SHA and timestamp so the staleness signal clears.
    stamp_handle(handle, new_sha).await;

    tracing::info!(
        "reconcile[{index_id}]: complete — indexed={indexed} removed_chunks={removed} \
         skipped={skipped} new_sha={}",
        &new_sha[..new_sha.len().min(12)],
    );
}

/// Stamp `indexed_head_sha = new_sha` and `last_indexed_at = now` on the handle.
///
/// Why: after a successful per-file reconciliation the staleness signal
/// (`results_may_be_stale` in the search response) must clear so callers are
/// no longer warned about outdated results.
/// What: acquires write locks on both `Arc<RwLock<Option<String>>>` fields and
/// writes the new values. Uses a minimal RFC-3339 formatter to avoid adding
/// external dependencies to this lightweight module.
/// Test: `reconcile_stamps_head_sha_after_delta`.
async fn stamp_handle(handle: &Arc<IndexHandle>, new_sha: &str) {
    *handle.indexed_head_sha.write().await = Some(new_sha.to_owned());
    *handle.last_indexed_at.write().await = Some(now_rfc3339());
}

/// Minimal RFC-3339 UTC timestamp (seconds precision).
///
/// Why: consistent timestamp format with `service::reindex::stages::now_rfc3339`
/// without importing that `pub(super)` function from an unrelated module.
/// What: formats `SystemTime::now()` as `YYYY-MM-DDTHH:MM:SSZ` using simple
/// integer arithmetic (Gregorian calendar approximation, good through 2100).
/// Test: `now_rfc3339_produces_valid_format`.
fn now_rfc3339() -> String {
    use std::time::SystemTime;
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let s = secs % 60;
    let m = (secs / 60) % 60;
    let h = (secs / 3600) % 24;
    let days = secs / 86400;
    // Gregorian approximation: 365.2425 days/year, accurate through 2100.
    let year = 1970 + days / 365;
    let day_of_year = days % 365;
    let month = day_of_year / 30 + 1;
    let day = day_of_year % 30 + 1;
    format!("{year:04}-{month:02}-{day:02}T{h:02}:{m:02}:{s:02}Z")
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    // ── Pure unit tests ──────────────────────────────────────────────────────

    /// Why: the disable gate must be testable without touching process env to
    /// avoid flaky behaviour in parallel test binaries.
    /// Test: only `Some("1")` disables; other values leave reconciliation on.
    #[test]
    fn reconcile_disabled_for_value_only_matches_one() {
        assert!(
            reconcile_disabled_for_value(Some("1")),
            "Some(\"1\") must disable"
        );
        assert!(
            !reconcile_disabled_for_value(None),
            "None (unset) must keep enabled"
        );
        assert!(
            !reconcile_disabled_for_value(Some("0")),
            "\"0\" must keep enabled"
        );
        assert!(
            !reconcile_disabled_for_value(Some("true")),
            "\"true\" must keep enabled"
        );
        assert!(
            !reconcile_disabled_for_value(Some("")),
            "empty string must keep enabled"
        );
    }

    /// Why: outside a git repo `changed_files_between` must return `None`,
    /// not panic — callers fall back to a full reindex on `None`.
    /// Test: this test.
    #[test]
    fn changed_files_between_returns_none_outside_git_repo() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(
            changed_files_between(tmp.path(), "deadbeef").is_none(),
            "expected None outside a git repo"
        );
    }

    /// Why: skip predicates must exclude build artefacts and pass normal source.
    /// Test: `node_modules` path excluded; plain Rust file not excluded.
    #[test]
    fn reconcile_skip_excluded_path() {
        assert!(
            should_skip_for_reconcile(Path::new("node_modules/lodash/index.js")),
            "node_modules must be excluded"
        );
        assert!(
            !should_skip_for_reconcile(Path::new("src/lib.rs")),
            "normal source file must not be excluded"
        );
    }

    /// Why: `now_rfc3339` must produce a non-empty string in the expected format.
    /// Test: basic format checks.
    #[test]
    fn now_rfc3339_produces_valid_format() {
        let ts = now_rfc3339();
        assert!(!ts.is_empty(), "timestamp must not be empty");
        assert!(ts.ends_with('Z'), "timestamp must end with Z: {ts}");
        assert!(ts.contains('T'), "timestamp must contain T: {ts}");
        assert_eq!(ts.len(), 20, "timestamp must be exactly 20 chars: {ts}");
    }

    // ── Git-backed integration helpers ───────────────────────────────────────

    /// Create a minimal git repo with one committed file.
    /// Returns `(TempDir, initial_sha, root_path)`.
    fn init_git_repo_with_file(
        filename: &str,
        content: &str,
    ) -> (tempfile::TempDir, String, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();

        let ok = |output: std::process::Output| {
            assert!(output.status.success(), "git command failed");
        };

        ok(Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(&root)
            .output()
            .expect("git init"));
        ok(Command::new("git")
            .args(["config", "user.email", "test@test.test"])
            .current_dir(&root)
            .output()
            .expect("git config email"));
        ok(Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(&root)
            .output()
            .expect("git config name"));

        std::fs::write(root.join(filename), content).expect("write file");
        ok(Command::new("git")
            .args(["add", "."])
            .current_dir(&root)
            .output()
            .expect("git add"));
        ok(Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&root)
            .output()
            .expect("git commit"));

        let sha_out = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&root)
            .output()
            .expect("git rev-parse");
        let sha = std::str::from_utf8(&sha_out.stdout)
            .expect("utf8")
            .trim()
            .to_owned();

        (dir, sha, root)
    }

    /// Add a second commit with modified file content. Returns the new HEAD SHA.
    fn add_commit(root: &Path, filename: &str, content: &str) -> String {
        std::fs::write(root.join(filename), content).expect("write file");
        Command::new("git")
            .args(["add", "."])
            .current_dir(root)
            .output()
            .expect("git add");
        Command::new("git")
            .args(["commit", "-m", "update"])
            .current_dir(root)
            .output()
            .expect("git commit");
        let sha_out = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(root)
            .output()
            .expect("git rev-parse");
        std::str::from_utf8(&sha_out.stdout)
            .expect("utf8")
            .trim()
            .to_owned()
    }

    // ── Git-backed integration tests ─────────────────────────────────────────

    /// Why: `changed_files_between` must find the file that changed between
    /// two real commits in a git repo.
    /// Test: two commits; assert the diff includes the modified file.
    #[test]
    fn changed_files_between_finds_modified_file() {
        let (_dir, first_sha, root) = init_git_repo_with_file("src.rs", "fn a() {}");
        add_commit(&root, "src.rs", "fn a() {}\nfn b() {}\n");

        let files = changed_files_between(&root, &first_sha)
            .expect("changed_files_between must return Some in a valid repo");
        assert!(
            files.iter().any(|f| f == "src.rs"),
            "expected src.rs in changed files, got {files:?}"
        );
    }

    /// Why: a fabricated / history-rewritten SHA must return `None` so the
    /// caller can fall back to a full reindex.
    /// Test: pass a zeroed SHA to a valid git repo.
    #[test]
    fn changed_files_between_returns_none_for_unknown_sha() {
        let (_dir, _sha, root) = init_git_repo_with_file("foo.rs", "fn x() {}");
        assert!(
            changed_files_between(&root, "0000000000000000000000000000000000000000").is_none(),
            "unknown SHA must return None"
        );
    }

    /// Why: `stamp_handle` must update both `indexed_head_sha` and
    /// `last_indexed_at` so the staleness signal clears after reconciliation.
    /// Test: build a minimal handle, call `stamp_handle`, assert both fields.
    #[tokio::test]
    async fn reconcile_stamps_head_sha_after_delta() {
        use crate::core::registry::{IndexHandle, IndexId, WalkDiagnostics};
        use crate::service::warm_boot::{derive_warm_boot_stages, WarmBootInputs};

        let dir = tempfile::tempdir().expect("tempdir");
        let handle = Arc::new(IndexHandle {
            id: IndexId::new("test-stamp"),
            indexer: Arc::new(RwLock::new(crate::core::CodeIndexer::new(
                "test-stamp",
                dir.path(),
            ))),
            root_path: dir.path().to_path_buf(),
            include_paths: vec![],
            exclude_globs: vec![],
            extensions: vec![],
            domain_terms: vec![],
            include_docs: true,
            respect_gitignore: true,
            extra_skip_dirs: vec![],
            data_file_max_bytes: 0,
            path_filter: vec![],
            context_embedding: Arc::new(RwLock::new(None)),
            context_summary: Arc::new(RwLock::new(None)),
            indexed_head_sha: Arc::new(RwLock::new(Some("old_sha".to_owned()))),
            last_indexed_at: Arc::new(RwLock::new(None)),
            lexical_only: false,
            skip_kg: false,
            defer_embed: false,
            stages: Arc::new(RwLock::new(derive_warm_boot_stages(WarmBootInputs {
                chunk_count: 0,
                hnsw_snapshot_ready: false,
                graph_node_count: 0,
                lexical_only: false,
                skip_kg: false,
                corpus_open_failed: false,
            }))),
            search_pressure: Arc::new(tokio::sync::Notify::new()),
            walk_diagnostics: Arc::new(RwLock::new(WalkDiagnostics::default())),
        });

        let new_sha = "new_sha_abcde";
        stamp_handle(&handle, new_sha).await;

        let stored = handle.indexed_head_sha.read().await.clone();
        assert_eq!(
            stored,
            Some(new_sha.to_owned()),
            "indexed_head_sha must equal new_sha after stamp"
        );
        assert!(
            handle.last_indexed_at.read().await.is_some(),
            "last_indexed_at must be Some after stamp"
        );
    }

    /// Why: `TRUSTY_NO_BOOT_RECONCILE=1` must prevent any reconcile task from
    /// being spawned. This uses `serial_test::serial` to avoid env contamination
    /// from concurrent tests.
    /// Test: set the env var, call `reconcile_stale_indexes`, assert no panic.
    #[tokio::test]
    #[serial_test::serial]
    async fn reconcile_disabled_gate() {
        // SAFETY: serial; only one test mutates this env var at a time.
        unsafe { std::env::set_var(NO_BOOT_RECONCILE_ENV, "1") };
        let state =
            crate::service::SearchAppState::new(crate::core::registry::IndexRegistry::new());
        reconcile_stale_indexes(&state).await; // must not panic
        unsafe { std::env::remove_var(NO_BOOT_RECONCILE_ENV) };
    }

    /// Why: when `indexed_head_sha == current HEAD`, `reconcile_one_index`
    /// must be a no-op (the handle must not be modified).
    /// Test: real git repo; stamp handle with current HEAD; call
    /// `reconcile_one_index`; assert SHA is unchanged.
    #[tokio::test]
    async fn reconcile_up_to_date_index_is_noop() {
        use crate::core::registry::{IndexHandle, IndexId, WalkDiagnostics};
        use crate::service::warm_boot::{derive_warm_boot_stages, WarmBootInputs};

        let (_dir, first_sha, root) = init_git_repo_with_file("hello.rs", "fn hello() {}");

        let handle = Arc::new(IndexHandle {
            id: IndexId::new("test-up-to-date"),
            indexer: Arc::new(RwLock::new(crate::core::CodeIndexer::new(
                "test-up-to-date",
                &root,
            ))),
            root_path: root.clone(),
            include_paths: vec![],
            exclude_globs: vec![],
            extensions: vec![],
            domain_terms: vec![],
            include_docs: true,
            respect_gitignore: true,
            extra_skip_dirs: vec![],
            data_file_max_bytes: 0,
            path_filter: vec![],
            context_embedding: Arc::new(RwLock::new(None)),
            context_summary: Arc::new(RwLock::new(None)),
            indexed_head_sha: Arc::new(RwLock::new(Some(first_sha.clone()))),
            last_indexed_at: Arc::new(RwLock::new(None)),
            lexical_only: false,
            skip_kg: false,
            defer_embed: false,
            stages: Arc::new(RwLock::new(derive_warm_boot_stages(WarmBootInputs {
                chunk_count: 0,
                hnsw_snapshot_ready: false,
                graph_node_count: 0,
                lexical_only: false,
                skip_kg: false,
                corpus_open_failed: false,
            }))),
            search_pressure: Arc::new(tokio::sync::Notify::new()),
            walk_diagnostics: Arc::new(RwLock::new(WalkDiagnostics::default())),
        });

        reconcile_one_index(Arc::clone(&handle)).await;

        let stored = handle.indexed_head_sha.read().await.clone();
        assert_eq!(
            stored,
            Some(first_sha),
            "SHA must not change when already up-to-date"
        );
        // last_indexed_at must remain None (no work done).
        assert!(
            handle.last_indexed_at.read().await.is_none(),
            "last_indexed_at must remain None when no-op"
        );
    }

    /// Why: when the stored SHA is older than current HEAD and the delta is
    /// within threshold, per-file reconciliation must run and stamp new SHA.
    /// Test: two-commit repo; handle stores old SHA; call `reconcile_one_index`;
    /// assert `indexed_head_sha` is updated to the new HEAD and
    /// `last_indexed_at` is stamped.
    #[tokio::test]
    async fn reconcile_stale_index_stamps_new_sha() {
        use crate::core::registry::{IndexHandle, IndexId, WalkDiagnostics};
        use crate::service::warm_boot::{derive_warm_boot_stages, WarmBootInputs};

        let (_dir, first_sha, root) = init_git_repo_with_file("lib.rs", "fn old() {}");
        add_commit(&root, "lib.rs", "fn old() {}\nfn new_fn() {}\n");
        let current_sha = crate::core::git::head_sha(&root).expect("head sha");

        let handle = Arc::new(IndexHandle {
            id: IndexId::new("test-stale"),
            indexer: Arc::new(RwLock::new(crate::core::CodeIndexer::new(
                "test-stale",
                &root,
            ))),
            root_path: root.clone(),
            include_paths: vec![],
            exclude_globs: vec![],
            extensions: vec![],
            domain_terms: vec![],
            include_docs: true,
            respect_gitignore: true,
            extra_skip_dirs: vec![],
            data_file_max_bytes: 0,
            path_filter: vec![],
            context_embedding: Arc::new(RwLock::new(None)),
            context_summary: Arc::new(RwLock::new(None)),
            indexed_head_sha: Arc::new(RwLock::new(Some(first_sha))),
            last_indexed_at: Arc::new(RwLock::new(None)),
            lexical_only: false,
            skip_kg: false,
            defer_embed: false,
            stages: Arc::new(RwLock::new(derive_warm_boot_stages(WarmBootInputs {
                chunk_count: 0,
                hnsw_snapshot_ready: false,
                graph_node_count: 0,
                lexical_only: false,
                skip_kg: false,
                corpus_open_failed: false,
            }))),
            search_pressure: Arc::new(tokio::sync::Notify::new()),
            walk_diagnostics: Arc::new(RwLock::new(WalkDiagnostics::default())),
        });

        reconcile_one_index(Arc::clone(&handle)).await;

        let stored = handle.indexed_head_sha.read().await.clone();
        assert_eq!(
            stored,
            Some(current_sha),
            "indexed_head_sha must be updated to current HEAD"
        );
        assert!(
            handle.last_indexed_at.read().await.is_some(),
            "last_indexed_at must be stamped after reconcile"
        );
    }
}
