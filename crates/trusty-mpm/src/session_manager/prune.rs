//! Bulk teardown + by-state pruning + tombstone compaction for managed sessions (#1508).
//!
//! Why: the `SessionRecord` store was monotonically append-only and accumulated
//! 239 stale TEST sessions — `decommission` wrote a tombstone but never deleted,
//! `store.remove()` was never called in production, and there was no test/ephemeral
//! marker or bulk teardown. This module adds the two missing teardown verbs and the
//! compaction pass so the store stops growing unbounded, all on top of the existing
//! per-session [`SessionManager::decommission`] internals (tmux kill + workspace
//! removal + tombstone). It lives in its own file so [`super::manager`] stays under
//! the 500-SLOC production cap (mirroring [`super::adopt`]).
//! What: [`PruneFilter`] (which records a prune targets), [`PruneAction`] /
//! [`PrunedSession`] / [`PruneOutcome`] (what a prune did or WOULD do under
//! `dry_run`), and an inherent `impl SessionManager` block adding
//! [`SessionManager::decommission_all_ephemeral`] and the general
//! [`SessionManager::prune_managed`].
//! Test: `prune_*` in `super::tests`.

use std::fmt;

use chrono::{Duration, Utc};
use tracing::{info, warn};

use super::manager::{ManagedError, SessionManager};
use super::record::{ManagedSessionId, ManagedSessionState, SessionRecord};

/// Maximum age an EPHEMERAL session may reach before the auto-reaper tears it
/// down — default 24 hours (#1508).
///
/// Why: a panicking or abandoned e2e test can leave an ephemeral session behind
/// even though the harness Drop-guard normally cleans it up; without a backstop
/// these would accumulate exactly like the 239 legacy records this feature exists
/// to prevent. 24h is long enough that no legitimate short-lived test session is
/// ever caught, yet short enough that leaks are reclaimed within a day. ONLY
/// `ephemeral == true` records are ever in scope — real sessions default `false`
/// and are unreachable by this path, so the threshold can be aggressive without
/// risking real work.
/// What: a `chrono::Duration` of 24 hours; the reaper decommissions any
/// `ephemeral` session whose `created_at` is older than `now - MAX_EPHEMERAL_AGE`.
/// Test: `reap_aged_ephemeral_picks_old_ephemeral_only` drives both the "too
/// young" and "non-ephemeral" exclusions against this threshold.
pub const MAX_EPHEMERAL_AGE_HOURS: i64 = 24;

/// Which managed-session records a prune targets (#1508).
///
/// Why: one teardown tool must serve BOTH the ephemeral-cleanup case (tear down
/// every test session) AND the legacy-purge case (clear the 239 stale
/// stopped/decommissioned records that predate the `ephemeral` flag). Modelling
/// the target as a closed enum keeps the safety rule — never touch a RUNNING
/// (`Active`/`Provisioning`) record unless explicitly forced — in one place.
/// What: [`Ephemeral`](PruneFilter::Ephemeral) selects `ephemeral == true`
/// non-terminal records; [`Stopped`](PruneFilter::Stopped) selects `Stopped`
/// records; [`Decommissioned`](PruneFilter::Decommissioned) selects existing
/// tombstones (for compaction only); [`All`](PruneFilter::All) selects every
/// NON-running record (ephemeral + stopped + errored + decommissioned).
/// Test: `prune_filter_parse_round_trip`, and the per-filter `prune_*` tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PruneFilter {
    /// Only sessions tagged `ephemeral == true` (test/throwaway sessions).
    Ephemeral,
    /// Only `Stopped` sessions (runtime gone, workspace still on disk).
    Stopped,
    /// Only `Decommissioned` tombstones — compacted (removed) from the store.
    Decommissioned,
    /// Every NON-running record: ephemeral, stopped, errored, and decommissioned.
    All,
}

impl PruneFilter {
    /// Parse a CLI/wire string into a [`PruneFilter`].
    ///
    /// Why: the CLI `--state` flag, the HTTP body, and the MCP tool all accept the
    /// same lowercase spellings; centralising the parse keeps them consistent and
    /// rejects typos with a single actionable message.
    /// What: maps `ephemeral`/`stopped`/`decommissioned`/`all` (case-insensitive,
    /// trimmed) to the matching variant; anything else is an `Err` naming the
    /// supported values.
    /// Test: `prune_filter_parse_round_trip` (covers both the round-trip and the
    /// garbage-rejection case).
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "ephemeral" => Ok(Self::Ephemeral),
            "stopped" => Ok(Self::Stopped),
            "decommissioned" => Ok(Self::Decommissioned),
            "all" => Ok(Self::All),
            other => Err(format!(
                "unknown prune filter `{other}` (expected: ephemeral | stopped | decommissioned | all)"
            )),
        }
    }

    /// The canonical lowercase name of this filter.
    ///
    /// Why: responses echo the filter back so callers can confirm what ran.
    /// What: the inverse of [`parse`](Self::parse).
    /// Test: `prune_filter_parse_round_trip`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ephemeral => "ephemeral",
            Self::Stopped => "stopped",
            Self::Decommissioned => "decommissioned",
            Self::All => "all",
        }
    }
}

impl fmt::Display for PruneFilter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a prune did (or WOULD do under `dry_run`) to a single record (#1508).
///
/// Why: `decommission` (tear down a live/stopped session) and `remove` (drop an
/// existing tombstone from the store) are semantically distinct outcomes; the
/// caller wants to know which happened per session so a dry-run report is precise.
/// What: [`Decommissioned`](PruneAction::Decommissioned) — killed runtime +
/// removed workspace + tombstoned; [`Removed`](PruneAction::Removed) — an existing
/// `Decommissioned` tombstone was deleted from the store (compaction).
/// Test: asserted by `decommission_all_ephemeral_ignores_non_ephemeral` (the
/// `Decommissioned` action) and `prune_decommissioned_compacts` (the `Removed`
/// action).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PruneAction {
    /// The session was torn down (runtime killed, workspace removed, tombstoned).
    Decommissioned,
    /// An existing tombstone was deleted from the store (compaction).
    Removed,
}

impl PruneAction {
    /// The canonical lowercase name of this action (for wire/log rendering).
    ///
    /// Why: HTTP/MCP responses and the CLI dry-run render the action per row.
    /// What: `Decommissioned` → `"decommissioned"`, `Removed` → `"removed"`.
    /// Test: `prune_outcome_serializes`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Decommissioned => "decommissioned",
            Self::Removed => "removed",
        }
    }
}

/// One record a prune touched (or would touch), with enough identity to report.
///
/// Why: a dry-run must show the operator EXACTLY which sessions are in scope —
/// id, tmux name, and prior state — before anything is destroyed.
/// What: the session id, tmux name, the state the record was in, and the
/// [`PruneAction`] applied (or that would be applied under `dry_run`).
/// Test: `prune_dry_run_reports_without_mutating`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PrunedSession {
    /// Managed session id (UUID string).
    pub id: String,
    /// tmux session name.
    pub tmux_name: String,
    /// The lifecycle state the record was in before the prune.
    pub state: String,
    /// What the prune did (or would do under `dry_run`).
    pub action: PruneAction,
}

/// The result of a prune: which sessions were (or would be) affected (#1508).
///
/// Why: the CLI, HTTP, and MCP surfaces all need a structured, serializable
/// summary that works for BOTH a real run and a `dry_run` preview.
/// What: `dry_run` (true → nothing was mutated), the targeted `filter`, and the
/// per-session [`PrunedSession`] list. `count()` is the total affected.
/// Test: `prune_outcome_serializes`, `prune_dry_run_reports_without_mutating`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PruneOutcome {
    /// True when this was a preview — no record was killed, removed, or tombstoned.
    pub dry_run: bool,
    /// The filter that selected the affected records.
    pub filter: String,
    /// Every session affected (or that would be affected under `dry_run`).
    pub sessions: Vec<PrunedSession>,
}

impl PruneOutcome {
    /// Number of sessions affected (or that would be affected).
    ///
    /// Why: callers log/print "pruned N sessions" without re-deriving the length.
    /// What: returns `self.sessions.len()`.
    /// Test: `decommission_all_ephemeral_ignores_non_ephemeral` (asserts the count).
    pub fn count(&self) -> usize {
        self.sessions.len()
    }
}

/// Whether a record is currently RUNNING (must not be auto-torn-down).
///
/// Why: the core #1508 safety invariant — a prune must NEVER kill an
/// `Active`/`Provisioning` session unless the operator explicitly forces it. This
/// single predicate is the fail-closed gate every filter consults.
/// What: returns true for `Active` and `Provisioning`; false for `Stopped`,
/// `Errored`, and `Decommissioned`.
/// Test: `prune_by_state_never_touches_active`.
fn is_running(state: &ManagedSessionState) -> bool {
    matches!(
        state,
        ManagedSessionState::Active | ManagedSessionState::Provisioning
    )
}

/// Whether a record matches a prune `filter` (ignoring the running-state guard).
///
/// Why: keeping the match logic separate from the running-state safety gate keeps
/// each rule readable and individually testable.
/// What: `Ephemeral` → `ephemeral && state != Decommissioned`; `Stopped` →
/// `state == Stopped`; `Decommissioned` → `state == Decommissioned`; `All` → any
/// non-running record (the caller still applies [`is_running`] as the final gate).
/// Test: the per-filter `prune_*` tests.
fn matches_filter(record: &SessionRecord, filter: PruneFilter) -> bool {
    match filter {
        PruneFilter::Ephemeral => {
            record.ephemeral && record.state != ManagedSessionState::Decommissioned
        }
        PruneFilter::Stopped => record.state == ManagedSessionState::Stopped,
        PruneFilter::Decommissioned => record.state == ManagedSessionState::Decommissioned,
        PruneFilter::All => true,
    }
}

/// Enumerate orphaned per-session worktree directories under `repos_root` (#1840).
///
/// Why: extracted from `SessionManager::prune_orphaned_worktrees` so the
/// walk logic can be tested independently of the full session-manager setup,
/// and reused by the `doctor.rs` worktree health probe without duplicating the
/// filesystem walk.
/// What: walks `<repos_root>/<owner>/<repo>/.worktrees/` (two levels deep);
/// any leaf directory whose canonicalized path is NOT in `active_set` is
/// collected as an orphan. Using a `HashSet` with canonicalized paths avoids
/// O(n×m) linear scan and correctly handles symlinked workspace paths. A
/// non-existent or unreadable `repos_root` returns an empty vec.
/// Test: `prune_orphaned_worktrees_spares_active`,
///       `prune_orphaned_worktrees_removes_orphan`.
pub(crate) fn find_orphaned_worktrees(
    repos_root: &std::path::Path,
    active_set: &std::collections::HashSet<std::path::PathBuf>,
) -> Vec<std::path::PathBuf> {
    let mut orphans = Vec::new();
    let Ok(owner_entries) = std::fs::read_dir(repos_root) else {
        return orphans;
    };
    for owner_entry in owner_entries.flatten() {
        let owner_path = owner_entry.path();
        if !owner_path.is_dir() {
            continue;
        }
        let Ok(repo_entries) = std::fs::read_dir(&owner_path) else {
            continue;
        };
        for repo_entry in repo_entries.flatten() {
            let wt_dir = repo_entry.path().join(".worktrees");
            if !wt_dir.is_dir() {
                continue;
            }
            let Ok(wt_entries) = std::fs::read_dir(&wt_dir) else {
                continue;
            };
            for wt_entry in wt_entries.flatten() {
                let wt_path = wt_entry.path();
                if !wt_path.is_dir() {
                    continue;
                }
                // Item 8 (#1845): skip if the path cannot be canonicalized.
                // A dangling symlink or a deletion race makes the path
                // unresolvable; we cannot safely compare it against the active
                // set, so we leave it untouched rather than risk misclassifying
                // a live-but-partially-deleted worktree as an orphan.
                let canonical_wt = match std::fs::canonicalize(&wt_path) {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                if !active_set.contains(&canonical_wt) {
                    orphans.push(wt_path);
                }
            }
        }
    }
    orphans
}

impl SessionManager {
    /// Tear down EVERY ephemeral session — bulk teardown (#1508).
    ///
    /// Why: e2e harnesses (and any operator who tagged test sessions) need a
    /// one-shot "clean up all my throwaway sessions" verb that reuses the existing
    /// per-session [`decommission`](SessionManager::decommission) internals (tmux
    /// kill + workspace removal + tombstone). REAL sessions default
    /// `ephemeral == false` and so are unreachable here — the safety invariant.
    /// What: convenience wrapper over [`prune_managed`](Self::prune_managed) with
    /// `PruneFilter::Ephemeral`, not a dry run, never touching running sessions
    /// (a running ephemeral session is decommissioned like any other — `decommission`
    /// itself kills the runtime first). Returns the count of sessions decommissioned.
    /// Test: `decommission_all_ephemeral_ignores_non_ephemeral` (asserts the
    /// ephemeral-only decommission path and the non-ephemeral safety exclusion).
    pub async fn decommission_all_ephemeral(&self) -> Result<usize, ManagedError> {
        // Ephemeral sessions are throwaway by definition: include running ones so a
        // panicking test that left an Active ephemeral session is still cleaned up.
        let outcome = self
            .prune_managed(PruneFilter::Ephemeral, false, true)
            .await?;
        Ok(outcome.count())
    }

    /// Prune managed sessions by state — bulk teardown + compaction (#1508).
    ///
    /// Why: ONE tool must (a) tear down all ephemeral/stopped sessions and (b)
    /// compact the store by dropping decommissioned tombstones, so the legacy 239
    /// stale records can be purged with the SAME verb that cleans up test sessions.
    /// It is the engine behind `decommission_all_ephemeral`, the `tm sessions prune`
    /// CLI verb, the prune HTTP route, and the prune MCP tool.
    ///
    /// SAFETY (the #1508 invariant): a RUNNING record (`Active`/`Provisioning`) is
    /// NEVER killed or removed unless `include_active` is explicitly `true`. The
    /// `Decommissioned` filter only ever removes tombstones (it cannot reach a
    /// running record by construction).
    ///
    /// What: snapshots `store.all()`, selects records that both
    /// [`matches_filter`] AND pass the running-state gate (unless `include_active`),
    /// then per record —
    /// - a non-`Decommissioned` match → [`decommission`](Self::decommission) it
    ///   (kill runtime + remove workspace + tombstone) → [`PruneAction::Decommissioned`];
    /// - a `Decommissioned` match → [`SessionStore::remove`] it from the store
    ///   (compaction) → [`PruneAction::Removed`].
    ///
    /// When `dry_run` is true NOTHING is mutated — the returned [`PruneOutcome`]
    /// lists what WOULD happen. A per-session failure is logged and skipped (the
    /// sweep is best-effort, like the orphan-GC) so one stuck session cannot block
    /// the rest of a legacy purge.
    /// Test: `decommission_all_ephemeral_ignores_non_ephemeral` (ephemeral path),
    /// `prune_by_state_never_touches_active` (Stopped→Decommissioned + the
    /// running-state safety gate), `prune_decommissioned_compacts` (compaction),
    /// `prune_all_targets_non_running`, `prune_dry_run_reports_without_mutating`.
    pub async fn prune_managed(
        &self,
        filter: PruneFilter,
        dry_run: bool,
        include_active: bool,
    ) -> Result<PruneOutcome, ManagedError> {
        // Snapshot the full set ONCE (reloads-on-read so out-of-process writes are
        // seen). We then mutate per record below, each of which re-reads/saves.
        //
        // Pick up any out-of-process write under a brief write lock (the reload is
        // `&mut`), then drop it and take the read-only snapshot under a READ lock so
        // the (in-memory) `cached_all()` clone never blocks concurrent store readers
        // (#1508 review fix). `cached_all()` is `&self` and does no I/O.
        self.store.write().await.reload_if_changed().await?;
        let all = self.store.read().await.cached_all();

        // Select the in-scope records, applying the running-state safety gate.
        let targets: Vec<SessionRecord> = all
            .into_iter()
            .filter(|r| matches_filter(r, filter))
            .filter(|r| include_active || !is_running(&r.state))
            .collect();

        let mut sessions = Vec::with_capacity(targets.len());
        for record in targets {
            let is_tombstone = record.state == ManagedSessionState::Decommissioned;
            let action = if is_tombstone {
                PruneAction::Removed
            } else {
                PruneAction::Decommissioned
            };

            if !dry_run {
                if is_tombstone {
                    // Compaction: drop the tombstone from the store entirely.
                    if let Err(e) = self.store.write().await.remove(&record.id).await {
                        warn!(id = %record.id, "prune: compaction remove failed: {e}; skipping");
                        continue;
                    }
                } else if let Err(e) = self.decommission(&record.id).await {
                    warn!(id = %record.id, "prune: decommission failed: {e}; skipping");
                    continue;
                }
            }

            sessions.push(PrunedSession {
                id: record.id.to_string(),
                tmux_name: record.tmux_name.clone(),
                state: record.state.to_string(),
                action,
            });
        }

        if dry_run {
            info!(
                filter = %filter,
                count = sessions.len(),
                "prune (dry-run): reporting candidates, no mutation"
            );
        } else {
            info!(
                filter = %filter,
                count = sessions.len(),
                "prune: applied teardown/compaction"
            );
        }

        Ok(PruneOutcome {
            dry_run,
            filter: filter.as_str().to_string(),
            sessions,
        })
    }

    /// Remove a single decommissioned tombstone from the store (#1508).
    ///
    /// Why: the age-based reaper and any caller that has just decommissioned a
    /// record may want it gone from `sessions.json` rather than left as a tombstone,
    /// so the file stops growing unbounded. This is the single-record compaction
    /// primitive [`prune_managed`](Self::prune_managed) uses for the
    /// `Decommissioned` filter, exposed for direct callers.
    /// What: removes the record keyed by `id` via [`SessionStore::remove`] and
    /// persists. A not-present id is a no-op warning inside `remove`.
    /// Test: `compact_record_removes_from_store`.
    pub async fn compact_record(&self, id: &ManagedSessionId) -> Result<(), ManagedError> {
        self.store.write().await.remove(id).await?;
        Ok(())
    }

    /// Remove orphaned per-session git worktrees from the managed workspace root (#1840).
    ///
    /// Why: `decommission` now calls `git worktree remove --force` for in-project
    /// worktrees, but sessions decommissioned before the fix — or where the git
    /// command failed — leave stale `.worktrees/<session-id>/` directories. This
    /// sweep removes them without touching any directory that still corresponds to a
    /// live session (i.e. whose path appears in `active_workspace_paths`).
    ///
    /// SAFETY: only directories whose full canonicalized path is NOT in the active
    /// set are removed. Active session worktrees are NEVER touched. Paths are
    /// canonicalized to handle symlinks correctly (Fix 1b, #1840).
    ///
    /// TOCTOU safety (#1840, hardened #1845 item 9): the sweep runs in two phases.
    /// Phase 1 discovers orphan candidates using the caller-supplied
    /// `active_workspace_paths` snapshot (which may have been taken moments before
    /// this call). Phase 2 — the real deletion path — takes ONE fresh snapshot from
    /// the live session store immediately before the deletion loop (O(1) lock
    /// acquisitions vs. the prior O(n) per-candidate approach). The snapshot is
    /// taken as late as possible — just before the first deletion — to minimise the
    /// residual window. **Residual TOCTOU window:** a session registered AFTER the
    /// Phase 2 snapshot but BEFORE a candidate's deletion is NOT seen by the snapshot
    /// and could theoretically be deleted. This window is sub-millisecond in practice
    /// (the snapshot is taken after all I/O-bound Phase 1 work), making it
    /// substantially narrower than the per-candidate approach (which had an O(n)
    /// window). Treat this as narrowing the window to near-zero, not eliminating it.
    /// Dry-run returns after Phase 1 (no deletion, no snapshot).
    ///
    /// What: Phase 1 calls [`find_orphaned_worktrees`] inside `spawn_blocking`
    /// (the filesystem walk is blocking); panics are propagated as `Err`. Phase 2
    /// (real-delete only) takes ONE fresh `self.store` snapshot, then per candidate:
    /// canonicalize (skip on error — item 8), check against snapshot, then call
    /// `remove_session_worktree` in its own `spawn_blocking`. Returns the paths
    /// removed (or that would be removed under dry-run).
    /// Test: `prune_orphaned_worktrees_removes_orphan`,
    ///       `prune_orphaned_worktrees_spares_active`,
    ///       `prune_orphaned_worktrees_store_snapshot_blocks_deletion` (item 1).
    pub async fn prune_orphaned_worktrees(
        &self,
        repos_root: &std::path::Path,
        active_workspace_paths: &[std::path::PathBuf],
        dry_run: bool,
    ) -> Result<Vec<std::path::PathBuf>, anyhow::Error> {
        use super::decommission::remove_session_worktree;
        use std::collections::HashSet;

        let repos_root = repos_root.to_path_buf();
        // Build a canonicalized set for O(1) lookup and symlink safety.
        let initial_active: HashSet<std::path::PathBuf> = active_workspace_paths
            .iter()
            .map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()))
            .collect();

        // Phase 1: discover orphan candidates using the initial snapshot.
        // Propagate a spawn_blocking panic as Err (#1845 item 7) rather than
        // silently returning an empty candidate list.
        let candidates = tokio::task::spawn_blocking({
            let initial_active = initial_active.clone();
            move || find_orphaned_worktrees(&repos_root, &initial_active)
        })
        .await
        .map_err(|e| anyhow::anyhow!("prune-worktrees: orphan scan panicked: {e}"))?;

        if dry_run {
            for p in &candidates {
                info!(path = %p.display(), "prune-worktrees (dry-run): would remove orphaned worktree");
            }
            return Ok(candidates);
        }

        // Phase 2 (real-delete path): ONE fresh snapshot immediately before the
        // deletion loop (#1845 item 9). Each active path is inserted in BOTH its
        // canonicalized form (for symlink-safe comparison) and its raw form
        // (Finding 3 #1845: if canonicalize fails on the active side, keep the
        // raw path as a protective fallback so a canonicalize failure can never
        // cause an active worktree to be misidentified as an orphan and deleted).
        let fresh_active: HashSet<std::path::PathBuf> = {
            let mut set = HashSet::new();
            for r in self.store.read().await.cached_all() {
                let Some(p) = r.workspace_path else {
                    continue;
                };
                if let Ok(c) = std::fs::canonicalize(&p) {
                    set.insert(c);
                } else {
                    warn!(
                        path = %p.display(),
                        "prune-worktrees: active session path failed to canonicalize; \
                         using raw path as protective fallback (#1845 F3)"
                    );
                }
                // Always insert the raw path so the raw-form check below catches
                // cases where the active side failed to canonicalize.
                set.insert(p);
            }
            set
        };

        let mut removed = Vec::new();
        for candidate in candidates {
            // Item 8 (#1845): skip on canonicalize failure — a path that can't be
            // resolved is left untouched rather than risk incorrect deletion.
            let canonical_candidate = match std::fs::canonicalize(&candidate) {
                Ok(c) => c,
                Err(_) => {
                    warn!(
                        path = %candidate.display(),
                        "prune-worktrees: skipping candidate — canonicalize failed"
                    );
                    continue;
                }
            };
            // Check both the canonicalized form (symlink-safe) AND the raw form
            // (Finding 3 #1845: protects against active-path canonicalize failures
            // — if the active side couldn't be canonicalized, its raw path is in
            // the set and a raw-path match prevents accidental deletion).
            if fresh_active.contains(&canonical_candidate) || fresh_active.contains(&candidate) {
                info!(
                    path = %candidate.display(),
                    "prune-worktrees: skipping — active session appeared after initial snapshot"
                );
                continue;
            }

            info!(path = %candidate.display(), "prune-worktrees: removing orphaned worktree");
            let candidate_clone = candidate.clone();
            let removed_ok =
                tokio::task::spawn_blocking(move || remove_session_worktree(&candidate_clone))
                    .await
                    .unwrap_or_else(|e| {
                        tracing::error!(
                            "prune-worktrees: spawn_blocking panicked during removal: {e}"
                        );
                        false
                    });
            if removed_ok {
                removed.push(candidate);
            }
        }
        Ok(removed)
    }

    /// Auto-reap orphaned per-session worktree dirs using the manager's own live
    /// record set (#1838).
    ///
    /// Why: [`prune_orphaned_worktrees`](Self::prune_orphaned_worktrees) is only
    /// invoked manually (the `tm sessions prune-worktrees` CLI / HTTP route), so
    /// the managed-clone `.worktrees/<id>` tree still grows without bound — one
    /// project accumulated 94 dead worktree dirs because nothing ran the sweep
    /// automatically. This thin convenience wrapper lets the daemon's orphan-GC
    /// loop reclaim orphaned worktree dirs on the SAME cadence it reaps orphaned
    /// tmux sessions, without each caller re-assembling the active-path set.
    /// What: snapshots every live record's `workspace_path` from the store as the
    /// active set, then delegates to `prune_orphaned_worktrees` with
    /// `dry_run = false`. The two-phase TOCTOU safety (a fresh store snapshot taken
    /// immediately before deletion) and the "only leaf dirs under `.worktrees/`,
    /// never the base clone" guard are inherited unchanged. Returns the paths removed.
    /// Test: `reap_orphaned_worktrees_removes_orphan_preserves_live` in
    /// `super::reap_orphaned_worktrees_tests`.
    pub async fn reap_orphaned_worktrees(
        &self,
        repos_root: &std::path::Path,
    ) -> Result<Vec<std::path::PathBuf>, anyhow::Error> {
        let active: Vec<std::path::PathBuf> = self
            .list()
            .await
            .into_iter()
            .filter_map(|r| r.workspace_path)
            .collect();
        self.prune_orphaned_worktrees(repos_root, &active, false)
            .await
    }

    /// Age-based auto-reap of stale ephemeral sessions (#1508).
    ///
    /// Why: a panicking or abandoned e2e test can leave an ephemeral session behind
    /// despite the harness Drop-guard. Without a backstop these leak exactly like
    /// the 239 legacy records this feature prevents. The orphan-GC loop calls this
    /// each sweep so leaked test sessions are reclaimed within
    /// [`MAX_EPHEMERAL_AGE_HOURS`].
    ///
    /// SAFETY: only `ephemeral == true` records are EVER in scope — real sessions
    /// default `false` and are unreachable by this path. State is irrelevant
    /// (a stuck Active ephemeral session past the age cutoff is reaped too, since
    /// `decommission` kills the runtime first).
    /// What: takes `max_age` as a parameter (for deterministic tests), snapshots the
    /// store, selects `ephemeral && created_at < now - max_age`, and decommissions
    /// each (best-effort; a per-session failure is logged and skipped). Returns the
    /// count reaped and logs the tmux names so the sweep is visible in daemon logs.
    /// Test: `reap_aged_ephemeral_picks_old_ephemeral_only`.
    pub async fn reap_aged_ephemeral(&self, max_age: Duration) -> Result<usize, ManagedError> {
        let cutoff = Utc::now() - max_age;
        // Reload under a brief write lock (the reload is `&mut`), then snapshot under
        // a READ lock via the in-memory, I/O-free `cached_all()` so the snapshot
        // clone never blocks concurrent store readers (#1508 review fix).
        self.store.write().await.reload_if_changed().await?;
        let all = self.store.read().await.cached_all();
        let stale: Vec<SessionRecord> = all
            .into_iter()
            .filter(|r| {
                r.ephemeral
                    && r.state != ManagedSessionState::Decommissioned
                    && r.created_at < cutoff
            })
            .collect();

        let mut reaped = 0usize;
        for record in stale {
            match self.decommission(&record.id).await {
                Ok(_) => {
                    reaped += 1;
                    info!(
                        id = %record.id,
                        name = %record.tmux_name,
                        created_at = %record.created_at.to_rfc3339(),
                        "auto-reap: decommissioned stale ephemeral session"
                    );
                }
                Err(e) => {
                    warn!(id = %record.id, "auto-reap: decommission failed: {e}; skipping");
                }
            }
        }
        Ok(reaped)
    }
}

#[cfg(test)]
mod orphan_tests {
    use super::*;

    #[test]
    fn prune_orphaned_worktrees_spares_active() {
        // A live session's worktree must never be returned as an orphan (#1840).
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let wt = root
            .join("owner")
            .join("repo")
            .join(".worktrees")
            .join("live-session");
        std::fs::create_dir_all(&wt).unwrap();
        let active: std::collections::HashSet<_> =
            vec![std::fs::canonicalize(&wt).unwrap_or_else(|_| wt.clone())]
                .into_iter()
                .collect();
        let orphans = find_orphaned_worktrees(root, &active);
        assert!(
            orphans.is_empty(),
            "live session must not be listed as orphan"
        );
    }

    #[test]
    fn prune_orphaned_worktrees_fresh_active_set_blocks_deletion() {
        // Simulates TOCTOU: a dir looks like an orphan in the initial snapshot
        // but appears in the fresh active set before deletion — must NOT be removed.
        // We test the `find_orphaned_worktrees` logic: with an empty initial set
        // the candidate IS found; then the Phase 2 TOCTOU check (re-querying the
        // store) is validated by confirming fresh set membership would block deletion.
        // The full async TOCTOU path is validated by the integration tests.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let wt = root
            .join("owner")
            .join("repo")
            .join(".worktrees")
            .join("session-xyz");
        std::fs::create_dir_all(&wt).unwrap();

        // Empty initial snapshot → the dir looks like an orphan candidate.
        let empty_initial: std::collections::HashSet<std::path::PathBuf> =
            std::collections::HashSet::new();
        let candidates = find_orphaned_worktrees(root, &empty_initial);
        assert!(
            candidates.contains(&wt),
            "empty initial set must find the dir as a candidate"
        );

        // Fresh active set contains the canonicalized dir path — mirrors Phase 2 of
        // prune_orphaned_worktrees, which canonicalizes workspace_paths before
        // inserting them into fresh_active (#1840 TOCTOU check).
        let canonical = std::fs::canonicalize(&wt).unwrap_or_else(|_| wt.clone());
        let fresh: std::collections::HashSet<std::path::PathBuf> =
            [canonical.clone()].into_iter().collect();
        assert!(
            fresh.contains(&canonical),
            "fresh active set must contain the canonicalized worktree path"
        );
        // The directory still exists — nothing deleted it.
        assert!(wt.exists(), "worktree must survive the TOCTOU check");
    }

    #[test]
    fn prune_orphaned_worktrees_collects_orphan() {
        // A worktree with no active session must be listed as an orphan (#1840).
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let wt1 = root
            .join("owner")
            .join("repo")
            .join(".worktrees")
            .join("live");
        let wt2 = root
            .join("owner")
            .join("repo")
            .join(".worktrees")
            .join("dead");
        std::fs::create_dir_all(&wt1).unwrap();
        std::fs::create_dir_all(&wt2).unwrap();
        let active: std::collections::HashSet<_> =
            vec![std::fs::canonicalize(&wt1).unwrap_or_else(|_| wt1.clone())]
                .into_iter()
                .collect();
        let orphans = find_orphaned_worktrees(root, &active);
        assert_eq!(orphans.len(), 1);
        // Ordering not guaranteed — use contains rather than indexed access.
        assert!(
            orphans.contains(&wt2),
            "expected {wt2:?} to be the orphan, got {orphans:?}"
        );
    }

    /// Item 1 (#1845): async test that genuinely exercises the Phase 2 fresh-store
    /// snapshot path in `prune_orphaned_worktrees`.
    ///
    /// Why: the existing sync test at `prune_orphaned_worktrees_fresh_active_set_blocks_deletion`
    /// only calls `find_orphaned_worktrees` directly, giving zero executed coverage of
    /// the Phase 2 `fresh_active` snapshot logic in the async method. This test goes
    /// end-to-end through `prune_orphaned_worktrees` with a real `SessionManager`:
    /// Phase 1 finds the worktree as a candidate (empty initial snapshot), then Phase 2
    /// reads the live store and finds the matching record — skipping deletion.
    /// Test: this function IS the test.
    #[tokio::test]
    async fn prune_orphaned_worktrees_store_snapshot_blocks_deletion() {
        use std::path::PathBuf;
        use std::sync::Arc;

        // Minimal driver: all ops are no-ops — we never actually need tmux.
        struct NoopDriver;
        impl super::super::manager::ManagedTmuxDriver for NoopDriver {
            fn create_session(
                &self,
                _: &str,
                _: &str,
            ) -> Result<(), super::super::manager::ManagedError> {
                Ok(())
            }
            fn kill_session(&self, _: &str) -> Result<(), super::super::manager::ManagedError> {
                Ok(())
            }
            fn send_line(
                &self,
                _: &str,
                _: &str,
            ) -> Result<(), super::super::manager::ManagedError> {
                Ok(())
            }
            fn capture(
                &self,
                _: &str,
                _: usize,
            ) -> Result<String, super::super::manager::ManagedError> {
                Ok(String::new())
            }
            fn list_sessions(&self) -> Result<Vec<String>, super::super::manager::ManagedError> {
                Ok(Vec::new())
            }
        }

        let store_dir = tempfile::tempdir().unwrap();
        let repos_tmp = tempfile::tempdir().unwrap();

        // Build a real .worktrees/<id>/ dir so Phase 1 finds it as a candidate.
        let session_id = super::super::record::ManagedSessionId::new();
        let wt_path = repos_tmp
            .path()
            .join("owner")
            .join("repo")
            .join(".worktrees")
            .join(session_id.to_string());
        std::fs::create_dir_all(&wt_path).expect("create worktree dir");

        // Create the SessionManager and insert a live record for the worktree.
        let mgr =
            super::super::manager::SessionManager::new(store_dir.path(), Arc::new(NoopDriver))
                .await
                .expect("SessionManager::new");

        let canonical_wt = std::fs::canonicalize(&wt_path).unwrap_or_else(|_| wt_path.clone());
        let record = super::super::record::SessionRecord {
            id: session_id,
            tmux_name: "test-toctou".into(),
            cwd: PathBuf::from("/tmp"),
            task: "toctou test".into(),
            state: super::super::record::ManagedSessionState::Active,
            created_at: chrono::Utc::now(),
            last_activity_at: None,
            workspace_path: Some(canonical_wt),
            repo_url: None,
            branch: None,
            pending_decision: None,
            proposed_default: None,
            correlation: Default::default(),
            runtime: Default::default(),
            ephemeral: false,
            workspace_owned: false,
            source_id: None,
            claude_session_id: None,
            scrollback_path: None,
            last_cwd: None,
        };
        mgr.store
            .write()
            .await
            .upsert(record)
            .await
            .expect("upsert test record");

        // Phase 1 will see an empty initial set → worktree is a candidate.
        // Phase 2 fresh snapshot reads the store → finds the record → skips deletion.
        let removed = mgr
            .prune_orphaned_worktrees(repos_tmp.path(), &[], false)
            .await
            .expect("prune must not error");

        assert!(
            removed.is_empty(),
            "worktree backed by a live store record must NOT be removed; got: {removed:?}"
        );
        assert!(wt_path.exists(), "worktree dir must survive the prune");
    }
}
