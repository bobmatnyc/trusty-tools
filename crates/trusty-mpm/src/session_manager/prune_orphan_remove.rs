//! The orphan sweep's action-time half: the pre-deletion active set and the
//! per-candidate re-check that runs immediately before each removal (#8782).
//!
//! Why: split out of `prune.rs` by #8782, whose action-time identity and
//! discard-allowlist checks would otherwise have pushed that file past the
//! 500-SLOC production cap. Every entry point that removes an orphan — the
//! PM-pause prune, the 60 s orphan-GC loop, and `prune-worktrees --force` —
//! reaches [`remove_candidate`] through `prune_orphaned_worktrees_in`, so the
//! re-check lives in exactly one place.
//! What: the #3715 canonicalize-failure streak counter, [`fresh_in_use`] (the
//! Phase 2 active-set snapshot), [`allowed_dirt_verdict`] (the #4091 dirty gate
//! bounded by the operator's discard allowlist), and [`remove_candidate`].
//! Test: `a_scanned_path_replaced_by_a_symlink_is_not_removed`,
//! `a_tree_dirtied_after_a_clean_preview_is_not_discarded`,
//! `canonicalize_streak_escalates_at_threshold`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use tracing::{error, info, warn};

use super::super::decommission::WorktreeRemoval;
use super::super::record::SessionRecord;
use super::super::worktree_safety::{
    DirtVerdict, DirtyWorktree, DirtyWorktreePolicy, dirt_verdict,
};
use super::super::worktree_scope::WorktreeScope;

/// Consecutive REAL-SWEEP canonicalize failures on the same path before the
/// #1845 F3 fallback escalates from `warn!` to `error!` (#3715 item 3).
///
/// Why: the F3 fallback WARN fired every minute for ~8h on the same path
/// before the underlying vanished-workspace-root issue (#3715) was noticed —
/// a per-tick WARN buried in a large log carries no signal that distinguishes
/// "just started" from "sustained for hours". 10 is deliberately a count of
/// consecutive OBSERVATIONS, not a wall-clock duration: `prune_orphaned_worktrees`
/// (real, non-dry-run, deletion-capable) has THREE call sites —
/// `orphan_gc_loop`'s periodic ~60s tick (`daemon/mod.rs`, spawned at line
/// 132, via `reap_orphaned_worktrees`), the `prune_worktrees` MCP tool
/// (`daemon/mcp_context.rs:182`, always real), and the
/// `POST /sessions/managed/prune-worktrees` HTTP route
/// (`daemon/managed_routes/prune.rs:111`, real whenever `dry_run` is
/// `false`) — so an operator-triggered manual sweep or MCP call between
/// periodic ticks advances the SAME streak. In the common case (only the
/// periodic loop running) 10 observations is roughly 10 minutes; under
/// interleaved manual sweeps it escalates sooner. Either way the count is
/// still meaningful as "sustained past a single transient blip" — the
/// interleaving only makes detection faster, never slower or wrong.
/// What: the threshold [`CanonicalizeFailureStreaks::record_failure`] compares
/// its return value against, to decide `warn!` vs `error!`.
/// Test: `canonicalize_streak_escalates_at_threshold`.
pub(super) const CANONICALIZE_FAILURE_STREAK_THRESHOLD: u32 = 10;

/// In-memory, per-path consecutive-failure counter for the #1845 F3
/// canonicalize fallback (#3715 item 3).
///
/// Why: a lone per-tick WARN gives no sense of DURATION — the F3 fallback for
/// the path behind #3715 fired unnoticed for ~8h because every occurrence
/// looked identical in the log. Tracking a streak lets the sweep escalate to
/// `error!` once failure has been sustained past
/// [`CANONICALIZE_FAILURE_STREAK_THRESHOLD`] consecutive REAL-sweep
/// observations (see that constant's doc for why this is observation-count,
/// not wall-clock), making it greppable and a future alerting target,
/// without adding persistence or an external alerting pipeline —
/// deliberately kept as simple in-process state (reset on daemon restart,
/// which is acceptable: a restart re-establishes a clean baseline for the
/// same underlying condition to re-accumulate if it is still present).
/// Entries are evicted once their path leaves the sweep's active-session set
/// (`retain_active`, called every real sweep — #3715 finding-2 follow-up)
/// so a decommissioned/deleted/moved session's streak does not linger
/// forever.
/// What: `record_failure` increments (or starts at 1) the counter for `path`
/// and returns the new streak length; `record_success` clears any existing
/// entry for `path` (a single successful canonicalize breaks the streak);
/// `retain_active` drops every tracked path NOT in the caller-supplied active
/// set.
/// Test: `canonicalize_streak_escalates_at_threshold`,
/// `canonicalize_streak_resets_on_success`,
/// `canonicalize_streak_evicts_paths_no_longer_active`.
#[derive(Debug, Default)]
pub(super) struct CanonicalizeFailureStreaks {
    counts: std::collections::HashMap<PathBuf, u32>,
}

impl CanonicalizeFailureStreaks {
    /// Record one more consecutive failure for `path`, returning the new streak length.
    pub(super) fn record_failure(&mut self, path: &Path) -> u32 {
        let count = self.counts.entry(path.to_path_buf()).or_insert(0);
        *count += 1;
        *count
    }

    /// Record a success for `path`, resetting (removing) any existing streak.
    pub(super) fn record_success(&mut self, path: &Path) {
        self.counts.remove(path);
    }

    /// Evict every tracked path NOT present in `active` (#3715 finding 2).
    ///
    /// Why: without this, a path whose session is decommissioned/deleted, or
    /// whose `workspace_path` simply changes, leaves a permanent orphaned
    /// entry in `counts` — unbounded growth over the daemon's lifetime.
    /// What: called once per real sweep with the set of `workspace_path`s
    /// actually observed THIS sweep; removes any tracked key absent from it.
    pub(super) fn retain_active(&mut self, active: &HashSet<PathBuf>) {
        self.counts.retain(|path, _| active.contains(path));
    }
}

/// Process-global streak state backing [`CanonicalizeFailureStreaks`] (#3715
/// item 3).
///
/// Why: exactly one [`CanonicalizeFailureStreaks`] instance should back all
/// three real-sweep call sites (see [`CANONICALIZE_FAILURE_STREAK_THRESHOLD`]'s
/// doc for why there are three, not one) so a streak observed via the MCP
/// tool or the HTTP route counts toward the same escalation as the periodic
/// loop — process-global state achieves that without adding a field (and
/// constructor-init site) to the shared `SessionManager` struct in
/// `manager.rs`. Deliberately unpersisted — see the type's own doc.
/// What: lazily-initialized `Mutex`-guarded counter map, accessed only via
/// [`canonicalize_failure_streaks`].
/// Test: covered indirectly by `canonicalize_streak_escalates_at_threshold`,
/// `canonicalize_streak_resets_on_success`, and
/// `canonicalize_streak_evicts_paths_no_longer_active`, which exercise
/// [`CanonicalizeFailureStreaks`] directly (no global state involved) to stay
/// deterministic and independent of test execution order.
static CANONICALIZE_FAILURE_STREAKS: std::sync::OnceLock<
    std::sync::Mutex<CanonicalizeFailureStreaks>,
> = std::sync::OnceLock::new();

/// The process-global [`CanonicalizeFailureStreaks`] instance, locked. A
/// poisoned lock is recovered: the map holds only counters.
fn canonicalize_failure_streaks() -> std::sync::MutexGuard<'static, CanonicalizeFailureStreaks> {
    CANONICALIZE_FAILURE_STREAKS
        .get_or_init(|| std::sync::Mutex::new(CanonicalizeFailureStreaks::default()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The Phase 2 active set: every record's `workspace_path`, canonical AND raw
/// (#1845 items 9 and F3).
///
/// Why: a canonicalize failure on the active side must never let an active
/// worktree be misidentified as an orphan, so the raw path is kept as a
/// protective fallback beside the canonical one.
/// What: inserts both forms per record, advances or clears the #3715 streak
/// per path, and evicts streaks for paths no longer observed.
/// Test: `reap_spares_a_stopped_records_workspace`,
/// `prune_orphaned_worktrees_store_snapshot_blocks_deletion`.
pub(super) fn fresh_in_use(records: Vec<SessionRecord>) -> HashSet<PathBuf> {
    let mut set = HashSet::new();
    // Raw `workspace_path`s actually observed THIS sweep, used below to
    // evict stale streak entries (#3715 finding 2) — kept separate from
    // `set` because `set` also accumulates canonicalized forms, which are
    // not the keys `CanonicalizeFailureStreaks` tracks.
    let mut checked_paths: HashSet<PathBuf> = HashSet::new();
    // #4288: DELIBERATELY UNFILTERED by record state, exactly like the
    // caller-supplied set this backstops. Do NOT add
    // `if r.state != Active { continue; }` here — a `SessionRecord`'s
    // state is bookkeeping, not a liveness signal (session
    // `2eb72dca-…` was measured RUNNING in tmux pane `%981` while
    // recorded `state: "stopped"`, holding 12 modified tracked files,
    // 31 untracked files, and 1 unpushed commit).
    //
    // This read is the LAST thing standing between a reclaimable
    // candidate and `remove_session_worktree`. It is what makes
    // narrowing any single caller's active set survivable, so it is
    // also the one whose loss is least visible: filter here and the
    // callers' own unfiltered reads still hide the damage until one of
    // them is tidied up too. Pinned by
    // `reap_spares_a_stopped_records_workspace` (real sweep) — that
    // test goes red once this read AND a caller's set are both narrowed.
    for r in records {
        let session_id = r.id;
        let Some(p) = r.workspace_path else {
            continue;
        };
        checked_paths.insert(p.clone());
        if let Ok(c) = std::fs::canonicalize(&p) {
            set.insert(c);
            // Success breaks any in-flight failure streak (#3715 item 3).
            canonicalize_failure_streaks().record_success(&p);
        } else {
            let streak = canonicalize_failure_streaks().record_failure(&p);
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
        // Always insert the raw path so the raw-form check catches cases
        // where the active side failed to canonicalize.
        set.insert(p);
    }
    // #3715 finding 2: evict any tracked streak whose path is no longer
    // among this sweep's active sessions, so the map cannot grow unbounded.
    canonicalize_failure_streaks().retain_active(&checked_paths);
    set
}

/// The #4091 dirty verdict, bounded by the operator's discard allowlist (#8782).
///
/// Why: `--force --discard-dirty` removes a tree after its preview. A tree the
/// preview reported clean but which is dirty by the time of the removal holds
/// work the operator never saw named, so `--discard-dirty` must not reach it.
/// What: [`dirt_verdict`], then a `Discards` verdict for a path the scope's
/// discard allowlist does not list becomes `Blocks`, with a reason naming the
/// allowlist. With no allowlist every `Discards` stands — the preview itself.
/// Test: `a_tree_dirtied_after_a_clean_preview_is_not_discarded`.
pub(super) fn allowed_dirt_verdict(
    candidate: &Path,
    policy: DirtyWorktreePolicy,
    phase: &'static str,
    scope: &WorktreeScope,
) -> DirtVerdict {
    match dirt_verdict(candidate, policy, phase) {
        DirtVerdict::Discards(dirt) if !scope.may_discard(candidate) => {
            warn!(
                path = %candidate.display(), phase,
                "prune-worktrees: unsaved work the preview did not name — keeping it (#8782)"
            );
            DirtVerdict::Blocks(DirtyWorktree {
                reason: format!(
                    "{} — the preview reported no unsaved work here, so --discard-dirty does \
                     not cover it; kept (#8782)",
                    dirt.reason
                ),
                ..dirt
            })
        }
        verdict => verdict,
    }
}

/// What the action-time re-check did with one candidate (#8782).
#[derive(Debug)]
pub(super) enum CandidateRemoval {
    /// Removed; `Some` names the unsaved work the removal discarded.
    Removed(Option<DirtyWorktree>),
    /// Kept; `Some` is reported in `skipped_dirty`.
    Kept(Option<DirtyWorktree>),
}

/// Re-check one reclaimable candidate at action time, then remove it (#8782).
///
/// Why: the scan's verdicts are minutes old by the time the sweep reaches a
/// candidate, and a path is not an identity — the directory at it can be
/// replaced, for instance by a symlink to another tree. Removing through a
/// replaced path acts on whatever the path now names.
/// What: in order, and keeping the candidate on the first refusal: the path
/// must still resolve to itself (scanned candidates are canonical, so a
/// difference means the path was replaced since the scan); no record in
/// `fresh_in_use` may claim it (#1845); and [`allowed_dirt_verdict`] must not
/// block it (#4118). Then `remove_session_worktree`.
/// Test: `a_scanned_path_replaced_by_a_symlink_is_not_removed`,
/// `a_tree_dirtied_after_a_clean_preview_is_not_discarded`,
/// `prune_orphaned_worktrees_store_snapshot_blocks_deletion`.
pub(super) async fn remove_candidate(
    candidate: &Path,
    fresh_in_use: &HashSet<PathBuf>,
    policy: DirtyWorktreePolicy,
    scope: &WorktreeScope,
) -> CandidateRemoval {
    // #8782: identity at action time. A canonicalize error (the path is gone)
    // is also a skip — #1845 item 8.
    match std::fs::canonicalize(candidate) {
        Ok(c) if c == candidate => {}
        resolved => {
            warn!(
                path = %candidate.display(), resolved = ?resolved,
                "prune-worktrees: skipping — the path no longer resolves to the \
                 scanned worktree (#8782)"
            );
            return CandidateRemoval::Kept(None);
        }
    }
    // The path is its own canonical form, so one lookup covers both the
    // canonical and the raw active entries (#1845 F3).
    if fresh_in_use.contains(candidate) {
        info!(
            path = %candidate.display(),
            "prune-worktrees: skipping — active session appeared after initial snapshot"
        );
        return CandidateRemoval::Kept(None);
    }
    // #4118 TOCTOU: the scan-time verdict is now minutes old. Re-ask
    // immediately before THIS removal.
    let discard = match allowed_dirt_verdict(candidate, policy, "pre-removal", scope) {
        DirtVerdict::Blocks(dirt) => return CandidateRemoval::Kept(Some(dirt)),
        DirtVerdict::Discards(dirt) => Some(dirt),
        DirtVerdict::Clean => None,
    };
    info!(path = %candidate.display(), "prune-worktrees: removing orphaned worktree");
    let owned = candidate.to_path_buf();
    // #7885: name the route in the audit line. #8534: `--discard-dirty`
    // discards gitignored output too.
    let outcome = tokio::task::spawn_blocking(move || {
        super::super::decommission::remove_session_worktree(
            &owned,
            "prune-worktrees orphan sweep: no live session claims this worktree",
            policy,
        )
    })
    .await
    .unwrap_or_else(|e| {
        error!("prune-worktrees: spawn_blocking panicked during removal: {e}");
        WorktreeRemoval::Kept(format!("the removal task panicked: {e}"))
    });
    // #4732: the remover reports WHY it kept a worktree.
    if let Some(reason) = outcome.reason() {
        warn!(path = %candidate.display(), "prune-worktrees: worktree kept — {reason}");
    }
    if outcome.removed() {
        CandidateRemoval::Removed(discard)
    } else {
        CandidateRemoval::Kept(None)
    }
}
