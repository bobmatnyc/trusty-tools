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
//! no live daemon required); the daemon-facing paths by
//! `sweep_orphaned_search_indexes_noop_when_daemon_unreachable`,
//! `sweep_skips_a_candidate_whose_status_probe_failed` and
//! `sweep_makes_no_request_under_a_test_harness` in `search_gc_guard_tests`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use tracing::{debug, info, warn};

use super::decommission::is_session_worktree;
use super::index_delete_guard::{DeleteOutcome, DestructiveIndexDelete};
use super::manager::SessionManager;
use crate::daemon::search_rpc::{self, SearchRpcError};

/// Per-request timeout for the READ-ONLY trusty-search calls in this module
/// (the sweep's index listing and status probes).
///
/// Why: these calls run off the interactive request path (decommission,
/// periodic GC) but must still never hang the daemon indefinitely if
/// trusty-search is wedged. The destructive delete carries its own timeout
/// inside [`DestructiveIndexDelete`] (#4743).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// How old a `.worktrees` root must be before a 0-chunk index rooted at it is
/// eligible for deletion (#5065 review).
///
/// Why: `chunk_count == 0` used to mean "created and abandoned", because the
/// index was minted at session LAUNCH, by which point the session record
/// already claimed the path and [`is_orphan_index`]'s claimed-by-a-live-session
/// check covered the whole population window. #5060 mints it at worktree
/// CREATION instead, before any record exists, and the index legitimately reads
/// 0 chunks for the entire lexical walk — 33.5 s measured on this workspace,
/// longer on a bigger one. The sweep runs every 60 s (`daemon/mod.rs`), so
/// without a grace window the sweep can delete a brand-new worktree's index out
/// from under the walk that is populating it.
///
/// 300 s is one sweep interval plus ~4x the measured walk. Erring long is
/// nearly free: the only cost of an over-long window is that a genuinely
/// abandoned 0-chunk index survives a few extra sweeps, and the sweep runs
/// forever. Erring short deletes live work.
const ORPHAN_GRACE: Duration = Duration::from_secs(300);

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

/// Best-effort `search.index.delete` against the running trusty-search daemon
/// (#2033).
///
/// Why: decommissioning a disposable managed-session workspace removes its
/// directory from disk; leaving its trusty-search index registered is exactly
/// the orphan-index problem this issue reports. Fail-soft by design: an
/// unreachable or erroring search daemon must NEVER block or fail session
/// teardown, so every outcome is logged and swallowed here — the caller
/// (`decommission_with_root`) invokes this unconditionally.
/// What: acquires the [`DestructiveIndexDelete`] capability and, when granted,
/// issues the delete and logs which of the four outcomes it got. A refused
/// capability — no daemon socket bound, or this is a test process (#4743) — is a
/// no-op, logged by `acquire` itself.
/// Test: exercised via the live-daemon decommission integration path;
/// `decommission_issues_no_request_to_a_live_daemon_under_test` pins that a
/// test process reaches the daemon zero times.
pub(super) async fn delete_search_index_best_effort(index_id: &str) {
    // #4743: no socket, no request, no `delete_data` opt-in without this. A
    // test process cannot acquire it, so the call is never built at all.
    let Some(deleter) = DestructiveIndexDelete::acquire() else {
        return;
    };
    match deleter.delete(index_id).await {
        DeleteOutcome::Removed => {
            info!(
                index_id,
                "decommission: removed trusty-search index (#2033)"
            );
        }
        DeleteOutcome::NotRemoved(detail) => {
            warn!(
                index_id,
                "decommission: trusty-search kept the index: {detail}"
            );
        }
        DeleteOutcome::Refused { code, message } => {
            warn!(
                index_id,
                code, "decommission: trusty-search index delete refused: {message}"
            );
        }
        DeleteOutcome::Transport(e) => {
            warn!(
                index_id,
                "decommission: trusty-search index delete failed: {e}"
            );
        }
    }
}

/// Wire shape of one row of `search.indexes.list` with `details`.
#[derive(Debug, Deserialize)]
struct IndexDetailRow {
    id: String,
    root_path: Option<String>,
}

/// Wire shape of `search.indexes.list` with `details`.
#[derive(Debug, Deserialize, Default)]
struct IndexDetailsResponse {
    #[serde(default)]
    indexes: Vec<IndexDetailRow>,
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
/// `in_use_workspace_paths` (a live session still claims this root), and (c)
/// `entry.root_path` no longer exists on disk, OR `entry.chunk_count == 0` and
/// the root is at least `grace` old.
///
/// The grace window (#5065 review) applies ONLY to the 0-chunk half: an index
/// whose root is gone from disk is unambiguously an orphan at any age, and
/// there is nothing left to stat. See [`ORPHAN_GRACE`] for why the 0-chunk half
/// needs one at all. Age is read from the root directory's creation time,
/// falling back to its mtime where the filesystem has no birth time; the
/// fallback's failure mode is a slightly longer window, never a shorter one.
/// Test: `is_orphan_index_false_for_non_worktree_root`,
/// `is_orphan_index_false_when_claimed_by_active_session`,
/// `is_orphan_index_true_for_zero_chunk_unclaimed_worktree`,
/// `is_orphan_index_true_for_deleted_root_path`,
/// `is_orphan_index_false_for_populated_unclaimed_worktree_that_still_exists`,
/// `is_orphan_index_spares_a_freshly_created_worktree_root`,
/// `is_orphan_index_grace_does_not_protect_a_root_that_is_gone`.
pub(super) fn is_orphan_index(
    entry: &IndexSnapshot,
    active_workspace_paths: &HashSet<PathBuf>,
    grace: Duration,
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
    if !entry.root_path.exists() {
        return true;
    }
    entry.chunk_count == 0 && !root_is_within_grace(&entry.root_path, grace)
}

/// Is `root` younger than `grace` — i.e. too new for a 0-chunk reading to mean
/// anything (#5065 review)?
///
/// Why: isolated so the age rule is one testable place rather than a clause
/// buried in [`is_orphan_index`], and so the metadata fallback order is stated
/// once.
/// What: prefers the directory's creation time and falls back to its mtime when
/// the platform or filesystem has no birth time. Returns `false` when neither
/// is readable — an unreadable root gets no protection, matching the rest of
/// this module's bias toward reclaiming what it cannot account for.
/// Test: `is_orphan_index_spares_a_freshly_created_worktree_root`.
fn root_is_within_grace(root: &Path, grace: Duration) -> bool {
    let Ok(meta) = std::fs::metadata(root) else {
        return false;
    };
    let Ok(stamp) = meta.created().or_else(|_| meta.modified()) else {
        return false;
    };
    stamp.elapsed().map(|age| age < grace).unwrap_or(true) // a future timestamp (clock skew) reads as brand new
}

/// Fetch `(id, root_path)` for every index registered with the daemon.
///
/// # Errors
///
/// When the daemon cannot be reached, refuses the call, or answers a body this
/// cannot decode. The caller decides what each means — see
/// [`SessionManager::sweep_orphaned_search_indexes`], which treats an
/// unanswered listing as "no daemon, nothing to sweep" and a refusal as a fault
/// worth reporting.
/// Test: `sweep_skips_a_candidate_whose_status_probe_failed` drives the success
/// path; `sweep_orphaned_search_indexes_noop_when_daemon_unreachable` the
/// transport failure.
async fn fetch_index_details(socket: &Path) -> anyhow::Result<Vec<(String, PathBuf)>> {
    let value = search_rpc::call_at(
        socket,
        search_rpc::METHOD_INDEXES_LIST,
        serde_json::json!({ "details": true }),
        REQUEST_TIMEOUT,
    )
    .await?;
    let body: IndexDetailsResponse = serde_json::from_value(value)?;
    Ok(body
        .indexes
        .into_iter()
        .filter_map(|row| row.root_path.map(|rp| (row.id, PathBuf::from(rp))))
        .collect())
}

/// Read `chunk_count` for one index, or `None` when the daemon did not tell us.
///
/// Why `Option` (#6285): this used to answer `0` for a failed probe, on the
/// reasoning that [`is_orphan_index`]'s other conditions would catch anything
/// important. They do not. `0` is the value that makes an aged, unclaimed
/// `.worktrees` index collectable, so a wedged daemon, a timed-out call or a
/// one-off refusal read as "this index is empty" and licensed a delete on
/// evidence that was never gathered. An unreachable daemon must never read as an
/// absent index.
/// What: `Some(n)` only when the daemon answered with a `chunk_count`; `None` for
/// a refusal, a transport failure, or a result that omits the field. The caller
/// skips a `None` candidate entirely rather than substituting a value for it.
/// Test: `sweep_skips_a_candidate_whose_status_probe_failed`.
async fn probe_chunk_count(socket: &Path, id: &str) -> Option<u64> {
    match search_rpc::call_at(
        socket,
        search_rpc::METHOD_INDEX_STATUS,
        serde_json::json!({ "index_id": id }),
        REQUEST_TIMEOUT,
    )
    .await
    {
        Ok(body) => body.get("chunk_count").and_then(serde_json::Value::as_u64),
        Err(e) => {
            debug!(index_id = %id, "search-index-gc: status probe failed: {e:#}");
            None
        }
    }
}

impl SessionManager {
    /// Snapshot every live managed session's workspace path, canonicalized
    /// (with a raw-path fallback for symlink-canonicalize failures — mirrors
    /// `prune::prune_orphaned_worktrees`'s `fresh_in_use` construction).
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
    /// What: lists every registered index (`search.indexes.list` with
    /// `details`), keeps only `.worktrees`-scoped candidates
    /// ([`is_session_worktree`]) not claimed by `active_workspace_path_set`,
    /// reads each candidate's `chunk_count`, and applies [`is_orphan_index`].
    /// Under `dry_run` the matching ids are logged and returned without deleting
    /// anything (mirrors `PruneWorktreesRequest`'s dry-run-by-default
    /// convention); otherwise each is removed via the
    /// [`DestructiveIndexDelete`] capability, logged individually (no silent
    /// truncation), and returned. Returns `Ok(vec![])` without error when no
    /// daemon answers — the sweep is best-effort, like the rest of the
    /// orphan-GC loop.
    ///
    /// #6285: an unreachable daemon and an absent index are held apart at both
    /// steps. A listing that goes unanswered ends the sweep with nothing
    /// collected; a candidate whose status probe goes unanswered is skipped
    /// rather than assumed empty. Neither can produce a delete.
    ///
    /// #4743: a non-dry-run sweep acquires the destructive capability BEFORE
    /// listing anything, so a test process returns `Ok(vec![])` having made no
    /// request at all rather than enumerating the operator's real indexes.
    /// Test: `is_orphan_index_*` cover the pure decision; this async
    /// orchestration is exercised by
    /// `sweep_orphaned_search_indexes_noop_when_daemon_unreachable`,
    /// `sweep_skips_a_candidate_whose_status_probe_failed`, and the test-process
    /// refusal by `sweep_makes_no_request_under_a_test_harness`.
    pub async fn sweep_orphaned_search_indexes(
        &self,
        dry_run: bool,
    ) -> anyhow::Result<Vec<String>> {
        // #4743: acquire the destructive capability BEFORE doing any work, not
        // at the delete. A sweep that lists indexes and probes each one only to
        // discover at the last step that it may not delete has already spent a
        // round-trip per index against a daemon it has no business acting on.
        // `dry_run` needs no capability — it deletes nothing by definition.
        let deleter = if dry_run {
            None
        } else {
            match DestructiveIndexDelete::acquire() {
                Some(d) => Some(d),
                None => return Ok(Vec::new()),
            }
        };

        let socket = match search_rpc::search_socket() {
            Ok(socket) => socket,
            Err(e) => {
                debug!(
                    "cannot resolve the trusty-search socket; skipping index orphan sweep: {e:#}"
                );
                return Ok(Vec::new());
            }
        };

        let details = match fetch_index_details(&socket).await {
            Ok(details) => details,
            // #6285: an unanswered listing is "no daemon", the no-op this sweep
            // has always been on a machine without one. A REFUSAL is different —
            // a daemon that answered and said no is a fault the GC loop should
            // report, not a quiet skip.
            Err(e) if e.downcast_ref::<SearchRpcError>().is_none() => {
                debug!("trusty-search daemon unreachable; skipping index orphan sweep: {e:#}");
                return Ok(Vec::new());
            }
            Err(e) => return Err(e),
        };
        let active_workspace_paths = self.active_workspace_path_set().await;

        let mut candidates = Vec::new();
        for (id, root_path) in details {
            // Cheap scope filter before paying for a status round-trip: never
            // even consider a non-worktree (persistent project) index.
            if !is_session_worktree(&root_path) {
                continue;
            }
            // #6285: no chunk count, no verdict. Substituting a value here is
            // what let an unreachable daemon read as an empty index — see
            // `probe_chunk_count`.
            let Some(chunk_count) = probe_chunk_count(&socket, &id).await else {
                continue;
            };
            let snapshot = IndexSnapshot {
                id: id.clone(),
                root_path,
                chunk_count,
            };
            if is_orphan_index(&snapshot, &active_workspace_paths, ORPHAN_GRACE) {
                candidates.push(id);
            }
        }

        // No capability means `dry_run` — the only other way to get here
        // without one is the early return above.
        let Some(deleter) = deleter else {
            for id in &candidates {
                info!(index_id = %id, "search-index-gc (dry-run): would remove orphaned/0-chunk index");
            }
            return Ok(candidates);
        };

        let mut removed = Vec::new();
        for id in candidates {
            // #4743: the `delete_data` opt-in that used to be formatted inline
            // here lives inside the capability, so this loop and the
            // decommission-time delete share one destructive door instead of
            // two independently-guarded ones. Its #4123 rationale is unchanged:
            // every candidate is a worktree-scoped index with no live session,
            // so its data is unreachable garbage a bare delete would leak.
            match deleter.delete(&id).await {
                DeleteOutcome::Removed => {
                    info!(index_id = %id, "search-index-gc: removed orphaned/0-chunk index");
                    removed.push(id);
                }
                // Every arm below leaves `id` OUT of `removed`: the sweep
                // reports what the daemon confirmed it removed, never what it
                // was asked to remove.
                DeleteOutcome::NotRemoved(detail) => {
                    warn!(index_id = %id, "search-index-gc: trusty-search kept the index: {detail}");
                }
                DeleteOutcome::Refused { code, message } => {
                    warn!(index_id = %id, code, "search-index-gc: delete refused: {message}");
                }
                DeleteOutcome::Transport(e) => {
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

    /// A PM session launched ON the project's main checkout must never have
    /// that checkout's trusty-search index reclaimed (ADR-0036).
    ///
    /// Why: ADR-0036 made the main checkout the DEFAULT workspace for a PM
    /// session rather than a rarely-taken opt-out, so the population reaching
    /// this predicate with a real, long-lived directory went from a handful of
    /// projects to all of them. The index rooted at a main checkout is the
    /// PROJECT's index — the one every search in that repo resolves to — not a
    /// disposable per-session index, and decommissioning a session must not
    /// reclaim it. The existing guard covers this by construction
    /// (`spawn_managed_on_main` records `workspace_owned = false` and a path
    /// that is not a `.worktrees/` leaf), but nothing asserted it for a real
    /// git checkout: `disposable_index_id_none_for_unowned_non_worktree` uses a
    /// bare non-existent path, so it would pass even if the predicate returned
    /// early on "no `.git` here" rather than on the ownership rule.
    /// What: builds a real checkout — a `.git` DIRECTORY at its root, which is
    /// what `resolve_project_root` looks for and what would otherwise yield a
    /// derivable index id — and asserts the predicate still returns `None`.
    /// Deleting either half of the `workspace_owned || is_session_worktree`
    /// guard turns this red.
    /// Test: itself.
    #[test]
    fn disposable_index_id_none_for_a_main_checkout_pm_session() {
        let tmp = tempfile::tempdir().unwrap();
        let main_checkout = tmp.path().join("bobmatnyc").join("trusty-tools");
        std::fs::create_dir_all(main_checkout.join(".git")).unwrap();

        // Precondition: this path IS resolvable to an index id — so a `None`
        // below is the ownership guard firing, not a failure to resolve.
        assert_eq!(
            disposable_workspace_index_id(Some(&main_checkout), true).as_deref(),
            Some("trusty-tools"),
            "fixture precondition: the path must be index-id-resolvable, or the \
             assertion below would pass for the wrong reason"
        );

        assert_eq!(
            disposable_workspace_index_id(Some(&main_checkout), false),
            None,
            "a PM session on the project's main checkout must never have the \
             project's own search index reclaimed (ADR-0036)"
        );
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
    //
    // A fixture root is created moments before the assertion, so every test
    // asserting the pre-#5065 "0 chunks means orphan" rule passes
    // `Duration::ZERO` to opt out of the age check. The two grace tests below
    // pass the real `ORPHAN_GRACE` — that split is the point: the first group
    // pins the rule for an AGED root, the second pins that a NEW one is spared.

    #[test]
    fn is_orphan_index_false_for_non_worktree_root() {
        let entry = IndexSnapshot {
            id: "persistent-project".into(),
            root_path: PathBuf::from("/Users/dev/persistent-project"),
            chunk_count: 0,
        };
        assert!(!is_orphan_index(&entry, &HashSet::new(), Duration::ZERO));
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
        assert!(!is_orphan_index(&entry, &active, Duration::ZERO));
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
        assert!(is_orphan_index(&entry, &HashSet::new(), Duration::ZERO));
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
        assert!(is_orphan_index(&entry, &HashSet::new(), Duration::ZERO));
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
        assert!(!is_orphan_index(&entry, &HashSet::new(), Duration::ZERO));
    }

    /// Regression for the #5060 creation-time race (#5065 review): a
    /// just-created worktree root is spared even though its index reads 0
    /// chunks and no session record claims it yet.
    ///
    /// Why: this is the exact state #5060 introduces and holds for the whole
    /// ~33.5 s lexical walk. Worktree creation registers the index BEFORE any
    /// session record carries `workspace_path`, so the sweep's
    /// claimed-by-a-live-session check — which covered this window when the
    /// index was minted at LAUNCH — no longer does. The sweep runs every 60 s,
    /// so it lands inside the window and deletes the index out from under the
    /// walk populating it. The consequence is not just a lost index: session
    /// launch then re-creates it with the vector lane on, undoing the
    /// BM25+KG-only ruling.
    /// What: builds the precise racing state (fresh `.worktrees` root, 0
    /// chunks, unclaimed) and asserts it is NOT an orphan under the real
    /// `ORPHAN_GRACE`. `is_orphan_index_true_for_zero_chunk_unclaimed_worktree`
    /// above pins that the same shape IS still reclaimed once aged, so this is
    /// a delay, not a blanket exemption.
    /// Test: this test.
    #[test]
    fn is_orphan_index_spares_a_freshly_created_worktree_root() {
        let tmp = tempfile::tempdir().unwrap();
        let wt = worktree_path(tmp.path(), "just-created");
        std::fs::create_dir_all(&wt).unwrap();
        let entry = IndexSnapshot {
            id: "just-created".into(),
            root_path: wt,
            chunk_count: 0,
        };
        assert!(
            !is_orphan_index(&entry, &HashSet::new(), ORPHAN_GRACE),
            "a worktree created seconds ago reads 0 chunks because its walk is \
             still running — deleting it here is the #5060 race"
        );
    }

    /// The grace window protects a POPULATING index, never a vanished one.
    ///
    /// Why: the obvious way to write the grace check is to gate the whole
    /// orphan verdict on age. That would make a worktree whose directory was
    /// removed moments ago unreclaimable for the whole window, and — because
    /// the root cannot be stat'd at all — the age read would have to guess.
    /// The window belongs to the 0-chunk half only.
    /// Test: this test.
    #[test]
    fn is_orphan_index_grace_does_not_protect_a_root_that_is_gone() {
        let entry = IndexSnapshot {
            id: "vanished".into(),
            root_path: PathBuf::from("/nonexistent/owner/repo/.worktrees/vanished"),
            chunk_count: 0,
        };
        assert!(is_orphan_index(&entry, &HashSet::new(), ORPHAN_GRACE));
    }
}
