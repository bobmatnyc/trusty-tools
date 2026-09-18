//! The orphaned-worktree sweep, run off the async runtime under a git ceiling
//! (#7965).
//!
//! Why: split from `prune.rs`, which sits at its SLOC cap, when #7965 moved the
//! sweep's blocking work onto the blocking pool. Before that, Phase 1.5's
//! ownership cross-check and dirty gate and Phase 2's canonicalize loop and
//! pre-removal re-check all ran inline on the async task, and none of their git
//! subprocesses had a ceiling. One wedged `git status` held a tokio worker — on a
//! `current_thread` runtime, every task, `/health` included — until git answered.
//! What: the inherent `SessionManager::prune_orphaned_worktrees` and its bounded
//! form `prune_orphaned_worktrees_within`. Only the store reads and the owner
//! resolution stay on the async task; everything that touches the filesystem or
//! spawns git runs in [`off_runtime`], under
//! [`crate::session_manager::git_ceiling::with_git_ceiling`].
//! Test: `a_wedged_git_status_neither_hangs_the_orphan_sweep_nor_delays_health`,
//! and the `prune_orphaned_worktrees_*` tests in `prune_orphan_tests.rs`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tracing::{debug, error, info, warn};

use super::{
    CANONICALIZE_FAILURE_STREAK_THRESHOLD, OrphanSweepOutcome, SweepClassification,
    canonicalize_failure_streaks, find_orphaned_worktrees,
};
use crate::core::bounded_proc::GIT_TIMEOUT;
use crate::session_manager::decommission::{WorktreeRemoval, remove_session_worktree};
use crate::session_manager::git_ceiling::with_git_ceiling;
use crate::session_manager::manager::SessionManager;
use crate::session_manager::record::ManagedSessionId;
use crate::session_manager::worktree_ownership::{SentinelOwner, read_sentinel_owner};
use crate::session_manager::worktree_safety::{
    DirtyWorktree, DirtyWorktreePolicy, dirt_blocks_removal, git_worktree_list_agrees,
};

impl SessionManager {
    /// Remove orphaned per-session git worktrees from the managed workspace root (#1840).
    ///
    /// Why: `decommission` now calls `git worktree remove --force` for in-project
    /// worktrees, but sessions decommissioned before the fix — or where the git
    /// command failed — leave stale `.worktrees/<session-id>/` directories. This
    /// sweep removes them without touching any directory that still corresponds to a
    /// live session (i.e. whose path appears in `in_use_workspace_paths`).
    ///
    /// SAFETY: only directories whose full canonicalized path is NOT in the active
    /// set are removed. Active session worktrees are NEVER touched. Paths are
    /// canonicalized to handle symlinks correctly (Fix 1b, #1840).
    ///
    /// TOCTOU safety (#1840, hardened #1845 item 9): the sweep runs in two phases.
    /// Phase 1 discovers orphan candidates using the caller-supplied
    /// `in_use_workspace_paths` snapshot (which may have been taken moments before
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
    /// What: [`prune_orphaned_worktrees_within`](Self::prune_orphaned_worktrees_within)
    /// with the shared per-call git ceiling, [`GIT_TIMEOUT`] (#7965). Phase 1
    /// calls [`find_orphaned_worktrees`] and reads each candidate's sentinel off
    /// the runtime; panics are propagated as `Err`. Phase 2 (real-delete only)
    /// takes ONE fresh `self.store` snapshot, then — off the runtime — per
    /// candidate: canonicalize (skip on error — item 8), check against snapshot,
    /// re-check dirt, then `remove_session_worktree`. Returns an
    /// [`OrphanSweepOutcome`] rather than a bare path list (#3649) so a caller
    /// can see BOTH what was (or would be) removed AND what was conservatively
    /// skipped for owner-unknown review.
    ///
    /// #3649 OWNERSHIP GATE (applied to every candidate, including under
    /// `dry_run` — so a preview matches what a real run would do): read the
    /// candidate's ownership sentinel via [`read_sentinel_owner`].
    /// - Owner UNKNOWN (legacy zero-byte sentinel, absent sentinel, or
    ///   unparsable content) → NEVER delete; counted in
    ///   [`OrphanSweepOutcome::owner_unknown`] so it keeps surfacing via
    ///   `tm doctor` / `--dry-run` until a human acts (zero-migration, ADR-0020).
    /// - Owner KNOWN → delete only if
    ///   [`SessionManager::resolve_ownerless_with_grace`] says the owner is
    ///   provably ownerless (a resolvable record in a terminal state, OR no
    ///   resolvable record AND the sentinel is older than
    ///   [`crate::session_manager::worktree_ownership::OWNERLESS_GRACE`] — see
    ///   that constant's doc for why an absent-but-YOUNG owner is a creation
    ///   race, not a deletion) AND `git worktree list` on the owning checkout
    ///   agrees the path is a real worktree ([`git_worktree_list_agrees`]) — a
    ///   disagreement, or a listing that timed out (#7965), is skipped
    ///   conservatively, never deleted.
    ///
    /// #4091 DIRTY-TREE GATE (applied AFTER the #3649 gate, additively — it
    /// never widens what ownership already approved, only narrows it): every
    /// candidate that survived the ownership gate is passed to
    /// [`dirt_blocks_removal`], which fails toward DIRTY on any error — a timed-out
    /// git call included (#7965). Under the default `policy`
    /// ([`DirtyWorktreePolicy::Skip`]) a dirty candidate is NEVER removed — it is
    /// reported in [`OrphanSweepOutcome::skipped_dirty`] with the reason and
    /// file/commit counts. `DirtyWorktreePolicy::ForceDiscard` removes it anyway
    /// after a `warn!` naming exactly what is being discarded; that variant is
    /// reachable only from an explicit operator opt-in (`discard_dirty` on the
    /// HTTP route / `tm session prune-worktrees --discard-dirty`), never from the
    /// default `/tm-session-pause` path. The gate runs under `dry_run` too, so a
    /// preview matches a real run.
    ///
    /// #4118 DIRTY-GATE TOCTOU: the Phase 1.5 verdict above is computed for
    /// EVERY candidate before ANY removal happens, so across a ~95-candidate
    /// sweep the gap between "certified clean" and "deleted" is the sweep's
    /// whole duration — minutes, not the sub-millisecond window the paragraph
    /// above describes for the ACTIVE-SESSION check. The dirty gate is
    /// therefore re-run immediately before each individual
    /// `remove_session_worktree`, so the authoritative verdict is adjacent to
    /// the deletion. The Phase 1.5 pass is kept because it is what `dry_run`
    /// reports and what keeps a preview honest. Two extra git invocations per
    /// candidate is nothing against a `remove_dir_all` of gigabytes.
    ///
    /// Test: `prune_orphaned_worktrees_removes_orphan`,
    /// `prune_orphaned_worktrees_spares_active`,
    /// `prune_orphaned_worktrees_store_snapshot_blocks_deletion` (item 1),
    /// `prune_orphaned_worktrees_skips_owner_unknown`,
    /// `prune_orphaned_worktrees_reclaims_terminal_owner`,
    /// `prune_orphaned_worktrees_spares_live_owner`,
    /// `prune_orphaned_worktrees_spares_recent_unregistered_owner` (#3649),
    /// `prune_orphaned_worktrees_skips_modified_tracked_file`,
    /// `prune_orphaned_worktrees_skips_untracked_file`,
    /// `prune_orphaned_worktrees_skips_unpushed_commit`,
    /// `prune_orphaned_worktrees_reclaims_clean_pushed_worktree`,
    /// `prune_orphaned_worktrees_skips_when_dirty_check_errors`,
    /// `prune_orphaned_worktrees_force_discards_dirty` (#4091).
    pub async fn prune_orphaned_worktrees(
        &self,
        repos_root: &Path,
        in_use_workspace_paths: &[PathBuf],
        dry_run: bool,
        policy: DirtyWorktreePolicy,
        // #7357: the caller's adopted anchors, injected all the way down to
        // `find_orphaned_worktrees` so this method's tests stay hermetic.
        adopted: &[PathBuf],
    ) -> Result<OrphanSweepOutcome, anyhow::Error> {
        // #7965: the manual route, the MCP tool and the unattended orphan-GC loop
        // all share the per-call git ceiling the hygiene sweep uses.
        self.prune_orphaned_worktrees_within(
            repos_root,
            in_use_workspace_paths,
            dry_run,
            policy,
            adopted,
            GIT_TIMEOUT,
        )
        .await
    }

    /// [`prune_orphaned_worktrees`](Self::prune_orphaned_worktrees), with every
    /// git call in the sweep bounded by `git_ceiling` (#7965).
    ///
    /// Why: a sweep whose git calls run inline on the async task, with no
    /// ceiling, stalls every other task on that worker for as long as git takes —
    /// which is how an orphan-GC tick kept `tm status` unanswered for 10 s
    /// windows. The ceiling is a parameter so a test can prove the bound with a
    /// wedge that releases in seconds, not minutes.
    /// What: the same gates, in the same order, with identical dry-run and
    /// real-run classification. Phase 1 (canonicalize the active set, discover,
    /// read sentinels), the Phase 1.5 git gates, and all of Phase 2 each run in
    /// one [`off_runtime`] call. The store reads and
    /// [`SessionManager::resolve_ownerless_with_grace`] stay on the async task. A
    /// git call that outlives `git_ceiling` is killed, and its candidate is kept:
    /// the dirty gate reads a timeout as dirty and the registry cross-check reads
    /// it as disagreement. `git worktree remove --force` runs under its own,
    /// longer ceiling inside `remove_session_worktree`.
    /// Test: `a_wedged_git_status_neither_hangs_the_orphan_sweep_nor_delays_health`.
    pub async fn prune_orphaned_worktrees_within(
        &self,
        repos_root: &Path,
        in_use_workspace_paths: &[PathBuf],
        dry_run: bool,
        policy: DirtyWorktreePolicy,
        adopted: &[PathBuf],
        git_ceiling: Duration,
    ) -> Result<OrphanSweepOutcome, anyhow::Error> {
        let repos_root = repos_root.to_path_buf();
        let in_use = in_use_workspace_paths.to_vec();
        let adopted = adopted.to_vec();

        // Phase 1: discover orphan candidates using the initial snapshot, and read
        // each one's sentinel. #7965: all filesystem or git, so all off the runtime.
        let scanned = off_runtime(git_ceiling, "orphan scan", move || {
            // Build a canonicalized set for O(1) lookup and symlink safety.
            let initial_in_use: HashSet<PathBuf> = in_use
                .iter()
                .map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()))
                .collect();
            find_orphaned_worktrees(&repos_root, &initial_in_use, &adopted)
                .into_iter()
                .map(|candidate| {
                    let owner = read_sentinel_owner(&candidate);
                    (candidate, owner)
                })
                .collect::<Vec<_>>()
        })
        .await?;

        // Phase 1.5 (#3649): classify every candidate by ownership BEFORE any
        // deletion decision — applied identically under dry-run and real runs
        // so a preview reflects reality.
        let mut owner_unknown = Vec::new();
        let mut agent_owned = Vec::new();
        let mut owner_gone = Vec::new();
        // #4323: counted, not logged per path — see `SweepClassification`.
        let mut skipped_live = 0usize;
        let total_candidates = scanned.len();
        for (candidate, owner) in scanned {
            match owner {
                SentinelOwner::Unknown => {
                    // #4323: `debug!`, not `info!` — this arm fires once per
                    // owner-unknown worktree on EVERY sweep, and the backlog is
                    // durable by design. The count survives in the summary below.
                    debug!(
                        path = %candidate.display(),
                        "prune-worktrees: owner-unknown sentinel — never auto-deleting; \
                         run `tm doctor` or inspect manually (#3649)"
                    );
                    owner_unknown.push(candidate);
                }
                // #4311: attributed to a dispatched agent, whose liveness this
                // sweep cannot answer — reclaimed by
                // `daemon::services::agent_worktree_reap` on the agent's exit.
                SentinelOwner::Agent(agent, _) => {
                    debug!(
                        path = %candidate.display(),
                        agent_id = %agent.agent_id,
                        "prune-worktrees: owned by a dispatched agent — reclaimed when that \
                         agent exits, never by this sweep (#4311)"
                    );
                    agent_owned.push(candidate);
                }
                SentinelOwner::Known(owner, created_at) => {
                    if !self.resolve_ownerless_with_grace(owner, created_at).await {
                        debug!(
                            path = %candidate.display(),
                            owner = %owner,
                            "prune-worktrees: owner is still live/resumable, or the sentinel \
                             is too young to rule out a creation race — skipping (#3649)"
                        );
                        skipped_live += 1;
                        continue;
                    }
                    owner_gone.push(candidate);
                }
            }
        }
        // #7965: the git gates on the owner-gone candidates, off the runtime.
        let (reclaimable, skipped_dirty) = off_runtime(git_ceiling, "scan gates", move || {
            scan_gates(owner_gone, policy)
        })
        .await?;

        // #4323: ONE line per sweep carrying every classification count.
        // Suppressed entirely when the sweep found no candidates.
        let classification = SweepClassification {
            candidates: total_candidates,
            owner_unknown: owner_unknown.len(),
            agent_owned: agent_owned.len(),
            skipped_live,
            skipped_dirty: skipped_dirty.len(),
            reclaimable: reclaimable.len(),
        };
        if let Some(summary) = classification.summary() {
            info!("{summary}");
        }

        if dry_run {
            for p in &reclaimable {
                info!(path = %p.display(), "prune-worktrees (dry-run): would remove orphaned worktree");
            }
            return Ok(OrphanSweepOutcome {
                removed: reclaimable,
                owner_unknown,
                skipped_dirty,
                agent_owned,
            });
        }

        // Phase 2 (real-delete path): ONE fresh snapshot immediately before the
        // deletion loop (#1845 item 9).
        //
        // #4288: DELIBERATELY UNFILTERED by record state, exactly like the
        // caller-supplied set this backstops. Do NOT add
        // `if r.state != Active { continue; }` here — a `SessionRecord`'s state
        // is bookkeeping, not a liveness signal (session `2eb72dca-…` was
        // measured RUNNING in tmux pane `%981` while recorded `state: "stopped"`,
        // holding 12 modified tracked files, 31 untracked files, and 1 unpushed
        // commit). This read is the LAST thing standing between a reclaimable
        // candidate and `remove_session_worktree`. Pinned by
        // `reap_spares_a_stopped_records_workspace`.
        //
        // #7965: only the raw records are read here; canonicalizing them is
        // filesystem work, so it runs off the runtime with the rest of the phase.
        let active: Vec<(ManagedSessionId, PathBuf)> = self
            .store
            .read()
            .await
            .cached_all()
            .into_iter()
            .filter_map(|r| Some((r.id, r.workspace_path?)))
            .collect();
        let (removed, skipped_dirty) = off_runtime(git_ceiling, "removal phase", move || {
            let fresh_in_use = fresh_in_use_set(active);
            remove_reclaimable(reclaimable, &fresh_in_use, policy, skipped_dirty)
        })
        .await?;
        Ok(OrphanSweepOutcome {
            removed,
            owner_unknown,
            skipped_dirty,
            agent_owned,
        })
    }
}

/// Run blocking sweep work on the blocking pool, under `git_ceiling` (#7965).
///
/// Why: a git subprocess or a `canonicalize` on the async task parks a runtime
/// worker for as long as the call takes.
/// What: `spawn_blocking` around [`with_git_ceiling`]. A panic in `work` becomes
/// an `Err` naming `what` (#1845 item 7), never an empty result.
/// Test: `a_wedged_git_status_neither_hangs_the_orphan_sweep_nor_delays_health`.
async fn off_runtime<T: Send + 'static>(
    git_ceiling: Duration,
    what: &'static str,
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, anyhow::Error> {
    tokio::task::spawn_blocking(move || with_git_ceiling(git_ceiling, work))
        .await
        .map_err(|e| anyhow::anyhow!("prune-worktrees: {what} panicked: {e}"))
}

/// The Phase 1.5 git gates for candidates whose owner is provably gone.
///
/// What: skips a candidate `git worktree list` does not agree on (#3649),
/// reports one holding unsaved work (#4091), and returns the rest as
/// reclaimable. Blocking: run it through [`off_runtime`].
/// Test: `prune_orphaned_worktrees_skips_modified_tracked_file`,
/// `a_wedged_git_status_neither_hangs_the_orphan_sweep_nor_delays_health`.
fn scan_gates(
    owner_gone: Vec<PathBuf>,
    policy: DirtyWorktreePolicy,
) -> (Vec<PathBuf>, Vec<DirtyWorktree>) {
    let mut reclaimable = Vec::new();
    let mut skipped_dirty = Vec::new();
    for candidate in owner_gone {
        if !git_worktree_list_agrees(&candidate) {
            warn!(
                path = %candidate.display(),
                "prune-worktrees: git worktree list disagrees this path is a worktree, or \
                 did not answer in time — skipping conservatively (#3649, #7965)"
            );
            continue;
        }
        // #4091: last gate — never destroy unsaved work.
        if let Some(dirt) = dirt_blocks_removal(&candidate, policy, "scan") {
            skipped_dirty.push(dirt);
            continue;
        }
        reclaimable.push(candidate);
    }
    (reclaimable, skipped_dirty)
}

/// The Phase 2 active set: every record's workspace path, raw AND canonical.
///
/// Why (Finding 3 #1845): if canonicalize fails on the active side, the raw path
/// stays in the set as a protective fallback, so a canonicalize failure can never
/// make an active worktree look like an orphan.
/// What: inserts both forms per record, feeds the #3715 failure-streak tracker
/// (escalating to `error!` at [`CANONICALIZE_FAILURE_STREAK_THRESHOLD`]), and
/// evicts streaks for paths no longer active. Blocking: run it through
/// [`off_runtime`].
/// Test: `canonicalize_streak_escalates_at_threshold`,
/// `prune_orphaned_worktrees_store_snapshot_blocks_deletion`.
fn fresh_in_use_set(active: Vec<(ManagedSessionId, PathBuf)>) -> HashSet<PathBuf> {
    let mut set = HashSet::new();
    // Raw paths observed THIS sweep, used below to evict stale streak entries
    // (#3715 finding 2) — separate from `set`, which also holds canonical forms.
    let mut checked_paths: HashSet<PathBuf> = HashSet::new();
    for (session_id, p) in active {
        checked_paths.insert(p.clone());
        if let Ok(c) = std::fs::canonicalize(&p) {
            set.insert(c);
            // Success breaks any in-flight failure streak (#3715 item 3).
            canonicalize_failure_streaks()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .record_success(&p);
        } else {
            let streak = canonicalize_failure_streaks()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .record_failure(&p);
            if streak >= CANONICALIZE_FAILURE_STREAK_THRESHOLD {
                error!(
                    session = %session_id,
                    path = %p.display(),
                    streak,
                    "prune-worktrees: active session path has failed to \
                     canonicalize for {streak} consecutive real-sweep \
                     observations — sustained failure, investigate before \
                     a stop/reap silently reconstitutes this workspace \
                     root (#3715)"
                );
            } else {
                warn!(
                    path = %p.display(),
                    "prune-worktrees: active session path failed to canonicalize; \
                     using raw path as protective fallback (#1845 F3)"
                );
            }
        }
        // Always insert the raw path so the raw-form check catches cases where
        // the active side failed to canonicalize.
        set.insert(p);
    }
    // #3715 finding 2: evict any tracked streak whose path is no longer among
    // this sweep's active sessions, so the counter map cannot grow unbounded.
    canonicalize_failure_streaks()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .retain_active(&checked_paths);
    set
}

/// The Phase 2 removal loop over the reclaimable candidates.
///
/// What: per candidate, skips one that fails to canonicalize (#1845 item 8) or
/// that appeared in `fresh_in_use`, re-runs the dirty gate immediately before
/// the removal (#4118), then calls `remove_session_worktree`. Returns the
/// removed paths and `skipped_dirty` extended with the pre-removal skips.
/// Blocking: run it through [`off_runtime`].
/// Test: `prune_orphaned_worktrees_rechecks_dirt_immediately_before_removal`,
/// `prune_orphaned_worktrees_reclaims_clean_pushed_worktree`.
fn remove_reclaimable(
    reclaimable: Vec<PathBuf>,
    fresh_in_use: &HashSet<PathBuf>,
    policy: DirtyWorktreePolicy,
    mut skipped_dirty: Vec<DirtyWorktree>,
) -> (Vec<PathBuf>, Vec<DirtyWorktree>) {
    let mut removed = Vec::new();
    for candidate in reclaimable {
        // Item 8 (#1845): skip on canonicalize failure — a path that can't be
        // resolved is left untouched rather than risk incorrect deletion.
        let Ok(canonical_candidate) = std::fs::canonicalize(&candidate) else {
            warn!(
                path = %candidate.display(),
                "prune-worktrees: skipping candidate — canonicalize failed"
            );
            continue;
        };
        // Check both the canonicalized form (symlink-safe) AND the raw form
        // (Finding 3 #1845).
        if fresh_in_use.contains(&canonical_candidate) || fresh_in_use.contains(&candidate) {
            info!(
                path = %candidate.display(),
                "prune-worktrees: skipping — active session appeared after initial snapshot"
            );
            continue;
        }
        // #4118 TOCTOU: the scan-time verdict is now minutes old. Re-ask
        // immediately before THIS removal.
        if let Some(dirt) = dirt_blocks_removal(&candidate, policy, "pre-removal") {
            skipped_dirty.push(dirt);
            continue;
        }

        info!(path = %candidate.display(), "prune-worktrees: removing orphaned worktree");
        // #7965: the whole phase now shares one blocking task, so a panic in one
        // removal is caught here rather than abandoning the rest of the pass.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            remove_session_worktree(&candidate)
        }))
        .unwrap_or_else(|_| {
            error!(path = %candidate.display(), "prune-worktrees: the removal panicked");
            WorktreeRemoval::Kept("the removal task panicked".to_string())
        });
        // #4732: the remover reports WHY it kept a worktree.
        if let Some(reason) = outcome.reason() {
            warn!(
                path = %candidate.display(),
                "prune-worktrees: worktree kept — {reason}"
            );
        }
        if outcome.removed() {
            removed.push(candidate);
        }
    }
    (removed, skipped_dirty)
}
