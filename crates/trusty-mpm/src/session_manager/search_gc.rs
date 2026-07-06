//! Trusty-search index lifecycle for managed-session teardown + orphan sweep (#2033).
//!
//! Why: `core::session_launch::search_index::register_project_index` already
//! find-or-creates (and starts a persistent file watcher for) a trusty-search
//! index at session launch — but nothing ever removed that index again. Every
//! decommissioned managed-worktree session left a permanently-orphaned,
//! never-queried index registered in the daemon, and worktrees created/torn
//! down before population left behind 0-chunk stub indexes. This module is
//! the missing teardown half of the lifecycle: [`disposable_workspace_index_id`]
//! together with [`delete_search_index_best_effort`] remove a single
//! session's index at decommission time (used by
//! `decommission::decommission_with_root`), and
//! [`SessionManager::sweep_orphaned_search_indexes`] periodically GCs any
//! worktree-scoped index left behind with no matching live session.
//! What: split out of `decommission.rs`/`prune.rs` (both near/at the 500-SLOC
//! production cap) rather than growing either file — this module's `impl
//! SessionManager` block is additive, exactly like `adopt.rs`/`prune.rs`.
//! Test: `disposable_index_id_*`, `is_orphan_index_*` in `tests` below (pure,
//! no live daemon required); the live-HTTP paths are best-effort/fail-soft by
//! design and are exercised the same way `search_index::best_effort_create_index`'s
//! daemon-down path is (integration-only).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use tracing::{debug, info, warn};

use super::decommission::is_session_worktree;
use super::manager::SessionManager;

/// Per-request timeout for trusty-search index-lifecycle HTTP calls.
///
/// Why: these calls run off the interactive request path (decommission,
/// periodic GC) but must still never hang the daemon indefinitely if
/// trusty-search is wedged.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// Connect timeout, tighter than the overall request timeout so an
/// unreachable host fails fast.
const CONNECT_TIMEOUT: Duration = Duration::from_millis(750);

/// Whether — and under which id — a session's workspace should lose its
/// trusty-search index on decommission (#2033).
///
/// Why: pure predicate so the safety rule is unit-testable without a live
/// daemon or a real git checkout. The rule intentionally mirrors
/// `decommission_with_root`'s FILESYSTEM-removal guard exactly: only a
/// disposable workspace (one the SM provisioned by clone, or an in-project
/// `.worktrees/<leaf>` worktree) ever loses its index. A local-path/adopted
/// session's real, long-lived directory is untouched on disk, so its index
/// must be untouched too.
/// What: returns `None` when `workspace_path` is `None`, or when the session
/// is neither `workspace_owned` nor [`is_session_worktree`]. Otherwise
/// resolves the git-root of `workspace_path` via
/// `trusty_common::resolve_project_root` (a worktree's OWN `.git` file makes
/// it its own root — a no-op walk in the common case) and derives the id via
/// `trusty_common::derive_index_id`, returning `None` if that yields an empty
/// string. CRITICAL (caller contract): must be invoked BEFORE the workspace
/// directory is removed from disk — once the directory is gone,
/// `resolve_project_root`'s upward walk could resolve to an ancestor's (e.g.
/// the shared base clone's) `.git` and derive the WRONG id.
/// Test: `disposable_index_id_none_when_no_workspace`,
/// `disposable_index_id_none_for_unowned_non_worktree`,
/// `disposable_index_id_derives_from_worktree_path`,
/// `disposable_index_id_derives_from_owned_clone_path`.
pub(super) fn disposable_workspace_index_id(
    workspace_path: Option<&Path>,
    workspace_owned: bool,
) -> Option<String> {
    let ws = workspace_path?;
    if !(workspace_owned || is_session_worktree(ws)) {
        return None;
    }
    let root = trusty_common::resolve_project_root(ws);
    let id = trusty_common::derive_index_id(&root);
    if id.trim().is_empty() { None } else { Some(id) }
}

/// Best-effort `DELETE {base}/indexes/{index_id}` against the discovered
/// trusty-search daemon (#2033).
///
/// Why: decommissioning a disposable managed-session workspace removes its
/// directory from disk; leaving its trusty-search index registered is exactly
/// the orphan-index problem this issue reports. Fail-soft by design: an
/// unreachable or erroring search daemon must NEVER block or fail session
/// teardown, so every outcome is logged and swallowed here — the caller
/// (`decommission_with_root`) invokes this unconditionally.
/// What: resolves the daemon's base URL via
/// `trusty_common::resolve_daemon_base_url`; when discoverable, issues a
/// short-timeout `DELETE` and logs the outcome (removed / non-2xx / transport
/// error) at info/warn. A `None` base (daemon never started) is a debug-logged
/// no-op.
/// Test: exercised via the live-daemon decommission integration path; the
/// daemon-unreachable branch mirrors `search_index::best_effort_create_index`'s
/// (both skip cleanly on a missing address file).
pub(super) async fn delete_search_index_best_effort(index_id: &str) {
    let Some(base) = trusty_common::resolve_daemon_base_url("trusty-search") else {
        debug!(
            index_id,
            "trusty-search daemon not discoverable; skipping index removal on decommission (#2033)"
        );
        return;
    };
    let client = match build_client() {
        Ok(c) => c,
        Err(e) => {
            warn!("trusty-search index-delete client build failed: {e}");
            return;
        }
    };
    let url = format!("{base}/indexes/{index_id}");
    match client.delete(&url).send().await {
        Ok(resp) if resp.status().is_success() => {
            info!(
                index_id,
                "decommission: removed trusty-search index (#2033)"
            );
        }
        Ok(resp) => {
            warn!(
                index_id,
                status = %resp.status(),
                "decommission: trusty-search index delete returned non-2xx"
            );
        }
        Err(e) => {
            warn!(
                index_id,
                "decommission: trusty-search index delete failed: {e}"
            );
        }
    }
}

/// Build the short-timeout `reqwest::Client` shared by every search-index
/// lifecycle call in this module.
fn build_client() -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
}

/// Wire shape of one row of `GET /indexes?details=true`.
#[derive(Debug, Deserialize)]
struct IndexDetailRow {
    id: String,
    root_path: Option<String>,
}

/// Wire shape of `GET /indexes?details=true`.
#[derive(Debug, Deserialize, Default)]
struct IndexDetailsResponse {
    #[serde(default)]
    indexes: Vec<IndexDetailRow>,
}

/// Wire shape of the fields we need from `GET /indexes/:id/status`.
#[derive(Debug, Deserialize, Default)]
struct IndexStatusRow {
    #[serde(default)]
    chunk_count: u64,
}

/// A minimal snapshot of one registered trusty-search index, enough to decide
/// orphan-candidacy (#2033).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct IndexSnapshot {
    pub id: String,
    pub root_path: PathBuf,
    pub chunk_count: u64,
}

/// Pure predicate: is `entry` an orphaned managed-worktree trusty-search index
/// (#2033)?
///
/// Why: isolates the sweep's safety rule from the live HTTP fetch loop so it
/// is exercisable with fabricated data — no daemon, no real worktree.
/// What: `false` unless (a) `entry.root_path`'s immediate parent directory is
/// named `.worktrees` (mirrors [`is_session_worktree`] — the sweep NEVER
/// touches a manually-registered, persistent, non-session project index),
/// (b) neither `entry.root_path` nor its canonicalized form appears in
/// `active_workspace_paths` (a live session still claims this root), and (c)
/// `entry.chunk_count == 0` OR `entry.root_path` no longer exists on disk.
/// Test: `is_orphan_index_false_for_non_worktree_root`,
/// `is_orphan_index_false_when_claimed_by_active_session`,
/// `is_orphan_index_true_for_zero_chunk_unclaimed_worktree`,
/// `is_orphan_index_true_for_deleted_root_path`,
/// `is_orphan_index_false_for_populated_unclaimed_worktree_that_still_exists`.
pub(super) fn is_orphan_index(
    entry: &IndexSnapshot,
    active_workspace_paths: &HashSet<PathBuf>,
) -> bool {
    if !is_session_worktree(&entry.root_path) {
        return false;
    }
    if active_workspace_paths.contains(&entry.root_path) {
        return false;
    }
    let canonical =
        std::fs::canonicalize(&entry.root_path).unwrap_or_else(|_| entry.root_path.clone());
    if active_workspace_paths.contains(&canonical) {
        return false;
    }
    entry.chunk_count == 0 || !entry.root_path.exists()
}

/// Fetch `(id, root_path)` for every index registered with the daemon.
async fn fetch_index_details(
    client: &reqwest::Client,
    base: &str,
) -> anyhow::Result<Vec<(String, PathBuf)>> {
    let url = format!("{base}/indexes?details=true");
    let body: IndexDetailsResponse = client.get(&url).send().await?.json().await?;
    Ok(body
        .indexes
        .into_iter()
        .filter_map(|row| row.root_path.map(|rp| (row.id, PathBuf::from(rp))))
        .collect())
}

/// Fetch `chunk_count` for one index. A non-2xx or transport error is treated
/// as `0` (fail-open toward "looks empty, worth reconsidering") since the
/// caller's [`is_orphan_index`] check ALSO requires the index to be
/// `.worktrees`-scoped and unclaimed by any live session before this value
/// can result in deletion — a genuinely populated, still-claimed index is
/// never at risk from a flaky status probe.
async fn fetch_chunk_count(client: &reqwest::Client, base: &str, id: &str) -> u64 {
    let url = format!("{base}/indexes/{id}/status");
    match client.get(&url).send().await {
        Ok(resp) if resp.status().is_success() => resp
            .json::<IndexStatusRow>()
            .await
            .map(|s| s.chunk_count)
            .unwrap_or(0),
        _ => 0,
    }
}

impl SessionManager {
    /// Snapshot every live managed session's workspace path, canonicalized
    /// (with a raw-path fallback for symlink-canonicalize failures — mirrors
    /// `prune::prune_orphaned_worktrees`'s `fresh_active` construction).
    async fn active_workspace_path_set(&self) -> HashSet<PathBuf> {
        let mut set = HashSet::new();
        for p in self
            .list()
            .await
            .into_iter()
            .filter_map(|r| r.workspace_path)
        {
            if let Ok(canonical) = std::fs::canonicalize(&p) {
                set.insert(canonical);
            }
            set.insert(p);
        }
        set
    }

    /// Sweep orphaned / 0-chunk trusty-search indexes for managed worktrees (#2033).
    ///
    /// Why: `decommission_with_root` removes a session's OWN index, but
    /// sessions decommissioned before this fix (or torn down by a crashed
    /// daemon) leave `.worktrees/`-rooted indexes registered forever, and
    /// worktrees created/deleted before ever being populated leave 0-chunk
    /// stub indexes — both accumulate unboundedly (`trusty-search doctor`
    /// flagged hundreds of these). This is the periodic backstop, mirroring
    /// `prune::prune_orphaned_worktrees`'s "orphan == not claimed by any live
    /// session" shape.
    /// What: lists every registered index (`GET /indexes?details=true`),
    /// keeps only `.worktrees`-scoped candidates ([`is_session_worktree`]) not
    /// claimed by [`active_workspace_path_set`], fetches each candidate's
    /// `chunk_count`, and applies [`is_orphan_index`]. Under `dry_run` the
    /// matching ids are logged and returned without deleting anything
    /// (mirrors `PruneWorktreesRequest`'s dry-run-by-default convention);
    /// otherwise each is removed via `DELETE /indexes/:id`, logged
    /// individually (no silent truncation), and returned. Returns `Ok(vec![])`
    /// without error when the daemon is undiscoverable — the sweep is
    /// best-effort, like the rest of the orphan-GC loop.
    /// Test: `is_orphan_index_*` cover the pure decision; this async
    /// orchestration is exercised via the daemon-unreachable no-op path in
    /// `sweep_orphaned_search_indexes_noop_when_daemon_unreachable`.
    pub async fn sweep_orphaned_search_indexes(
        &self,
        dry_run: bool,
    ) -> anyhow::Result<Vec<String>> {
        let Some(base) = trusty_common::resolve_daemon_base_url("trusty-search") else {
            debug!("trusty-search daemon not discoverable; skipping index orphan sweep (#2033)");
            return Ok(Vec::new());
        };
        let client = build_client()?;

        let details = fetch_index_details(&client, &base).await?;
        let active_workspace_paths = self.active_workspace_path_set().await;

        let mut candidates = Vec::new();
        for (id, root_path) in details {
            // Cheap scope filter before paying for a status round-trip: never
            // even consider a non-worktree (persistent project) index.
            if !is_session_worktree(&root_path) {
                continue;
            }
            let chunk_count = fetch_chunk_count(&client, &base, &id).await;
            let snapshot = IndexSnapshot {
                id: id.clone(),
                root_path,
                chunk_count,
            };
            if is_orphan_index(&snapshot, &active_workspace_paths) {
                candidates.push(id);
            }
        }

        if dry_run {
            for id in &candidates {
                info!(index_id = %id, "search-index-gc (dry-run): would remove orphaned/0-chunk index");
            }
            return Ok(candidates);
        }

        let mut removed = Vec::new();
        for id in candidates {
            let url = format!("{base}/indexes/{id}");
            match client.delete(&url).send().await {
                Ok(resp) if resp.status().is_success() => {
                    info!(index_id = %id, "search-index-gc: removed orphaned/0-chunk index");
                    removed.push(id);
                }
                Ok(resp) => {
                    warn!(
                        index_id = %id,
                        status = %resp.status(),
                        "search-index-gc: delete returned non-2xx"
                    );
                }
                Err(e) => {
                    warn!(index_id = %id, "search-index-gc: delete failed: {e}");
                }
            }
        }
        Ok(removed)
    }

    /// Auto-reap convenience wrapper for the periodic orphan-GC loop (#2033).
    ///
    /// Why: mirrors [`prune::reap_orphaned_worktrees`](super::prune) — the
    /// daemon's `orphan_gc_loop` calls one no-args method per sweep kind
    /// rather than assembling `dry_run`/path arguments itself.
    /// What: delegates to [`sweep_orphaned_search_indexes`](Self::sweep_orphaned_search_indexes)
    /// with `dry_run = false`.
    /// Test: covered transitively by the `sweep_orphaned_search_indexes` tests.
    pub async fn reap_orphaned_search_indexes(&self) -> anyhow::Result<Vec<String>> {
        self.sweep_orphaned_search_indexes(false).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worktree_path(root: &Path, leaf: &str) -> PathBuf {
        root.join("owner")
            .join("repo")
            .join(".worktrees")
            .join(leaf)
    }

    // ---- disposable_workspace_index_id -----------------------------------

    #[test]
    fn disposable_index_id_none_when_no_workspace() {
        assert_eq!(disposable_workspace_index_id(None, true), None);
        assert_eq!(disposable_workspace_index_id(None, false), None);
    }

    #[test]
    fn disposable_index_id_none_for_unowned_non_worktree() {
        // Local-path spawn / adopted session: not owned, not a `.worktrees/`
        // leaf — the real user directory must never lose its index.
        let path = Path::new("/Users/dev/my-real-project");
        assert_eq!(disposable_workspace_index_id(Some(path), false), None);
    }

    #[test]
    fn disposable_index_id_derives_from_worktree_path() {
        // In-project worktree (workspace_owned = false, but IS a `.worktrees/`
        // leaf) — must derive the id from the worktree leaf's OWN basename,
        // proving derivation is from the path, never session_id/tmux_name.
        let tmp = tempfile::tempdir().unwrap();
        let wt = worktree_path(tmp.path(), "my-feature-worktree");
        std::fs::create_dir_all(&wt).unwrap();
        // A git worktree has a `.git` FILE (not dir) at its own root.
        std::fs::write(wt.join(".git"), "gitdir: /somewhere/else").unwrap();

        let id = disposable_workspace_index_id(Some(&wt), false);
        assert_eq!(id.as_deref(), Some("my-feature-worktree"));
    }

    #[test]
    fn disposable_index_id_derives_from_owned_clone_path() {
        // SM-owned clone (workspace_owned = true), not under `.worktrees/`.
        let tmp = tempfile::tempdir().unwrap();
        let clone_root = tmp.path().join("owner").join("cloned-repo");
        std::fs::create_dir_all(clone_root.join(".git")).unwrap();

        let id = disposable_workspace_index_id(Some(&clone_root), true);
        assert_eq!(id.as_deref(), Some("cloned-repo"));
    }

    // ---- is_orphan_index ---------------------------------------------------

    #[test]
    fn is_orphan_index_false_for_non_worktree_root() {
        let entry = IndexSnapshot {
            id: "persistent-project".into(),
            root_path: PathBuf::from("/Users/dev/persistent-project"),
            chunk_count: 0,
        };
        assert!(!is_orphan_index(&entry, &HashSet::new()));
    }

    #[test]
    fn is_orphan_index_false_when_claimed_by_active_session() {
        let tmp = tempfile::tempdir().unwrap();
        let wt = worktree_path(tmp.path(), "live-session");
        std::fs::create_dir_all(&wt).unwrap();
        let entry = IndexSnapshot {
            id: "live-session".into(),
            root_path: wt.clone(),
            chunk_count: 0,
        };
        let active: HashSet<PathBuf> = [wt].into_iter().collect();
        assert!(!is_orphan_index(&entry, &active));
    }

    #[test]
    fn is_orphan_index_true_for_zero_chunk_unclaimed_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let wt = worktree_path(tmp.path(), "stub-session");
        std::fs::create_dir_all(&wt).unwrap();
        let entry = IndexSnapshot {
            id: "stub-session".into(),
            root_path: wt,
            chunk_count: 0,
        };
        assert!(is_orphan_index(&entry, &HashSet::new()));
    }

    #[test]
    fn is_orphan_index_true_for_deleted_root_path() {
        // Never existed on disk at all (worktree already removed) — even with
        // a non-zero chunk_count still cached in the daemon's registry, an
        // unclaimed index whose root is gone is a clear orphan.
        let entry = IndexSnapshot {
            id: "gone-session".into(),
            root_path: PathBuf::from("/nonexistent/owner/repo/.worktrees/gone-session"),
            chunk_count: 500,
        };
        assert!(is_orphan_index(&entry, &HashSet::new()));
    }

    #[test]
    fn is_orphan_index_false_for_populated_unclaimed_worktree_that_still_exists() {
        // Conservative: an unclaimed worktree index that STILL has chunks AND
        // whose root still exists on disk is left alone — it might be a
        // session the tracker briefly missed, not a genuine orphan.
        let tmp = tempfile::tempdir().unwrap();
        let wt = worktree_path(tmp.path(), "maybe-orphaned");
        std::fs::create_dir_all(&wt).unwrap();
        let entry = IndexSnapshot {
            id: "maybe-orphaned".into(),
            root_path: wt,
            chunk_count: 42,
        };
        assert!(!is_orphan_index(&entry, &HashSet::new()));
    }
}
