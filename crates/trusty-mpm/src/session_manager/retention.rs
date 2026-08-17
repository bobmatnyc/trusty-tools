//! Age-based eviction of terminal session records, and the slot numbers they
//! hold.
//!
//! Why: `tm ls` showed `NUM 107` at the bottom of a 31-row listing. The slot
//! registry (#3034) hands every record it observes a number, and the default
//! view then hides the `decommissioned`/`deleted` ones — 76 of those 107 slots
//! were held by rows nobody could see. Nothing had ever left the store: the
//! auto-prune path decommissions `record_only`, which leaves the record in
//! place. Freeing numbers therefore requires evicting the records behind them,
//! not a smarter allocator — with no record ever removed, the free set is empty
//! and any "reuse the lowest free slot" scheme is a provable no-op.
//!
//! What: [`SessionManager::sweep_terminal_records`] hard-deletes records in a
//! terminal state ([`ManagedSessionState::is_terminal`]) once they have sat
//! there longer than [`TERMINAL_RECORD_RETENTION_DAYS`], and releases each
//! evicted record's slot so the number becomes available again.
//! [`retention_verdict`] is the pure decision function it drives.
//!
//! Records written before `terminal_at` existed — the whole pre-existing
//! backlog, which is what put `NUM` in three digits — are backfilled from
//! [`inferred_terminal_at`] rather than from the current time, so one already
//! weeks past the window is eligible on the next sweep instead of a full window
//! after this ships.
//!
//! Scope, and the reason it is drawn this tightly: this sweep touches
//! `sessions.json` and nothing else. It never removes a worktree, a workspace
//! directory, a git branch, or any other file — see [`retention_verdict`]'s
//! `workspace_needs_protection` parameter for the guard that keeps a
//! record-only eviction from becoming a filesystem deletion by proxy, and
//! `workspace_needs_protection` for which workspaces that guard now covers
//! and why an undetermined answer counts as protected.
//!
//! Test: `retention_tests.rs`.
//!
//! [`inferred_terminal_at`]: crate::session_manager::retention::inferred_terminal_at

use chrono::{DateTime, Duration, Utc};
use tracing::{info, warn};

use super::manager::{ManagedError, SessionManager};
use super::record::{ManagedSessionId, SessionRecord};

/// How long a record stays in the store after entering a terminal state.
///
/// Why (#5327): protecting a worktree is `workspace_needs_protection`'s job,
/// and it holds a record for as long as there is one, at any age. All the clock
/// has to buy is the span in which an operator can still act on a record that
/// is already dead — a `NUM` read off a listing, or the #2777 in-pane revive,
/// which reads the record out of the store. Both are same-working-day actions
/// that must survive an overnight gap. 24 hours is also how long this crate
/// already waits before an automatic sweep acts on a session with no operator
/// present ([`super::MAX_EPHEMERAL_AGE_HOURS`]), and retention does strictly
/// less than that sweep: it deletes a record, never a process or a directory.
///
/// The unit stays days because the NAME is published API on a 1.x crate.
/// Renaming it to hours to write `24` instead of `1` is a major-version break
/// for a cosmetic gain.
/// What: the retention window in days, applied to
/// [`SessionRecord::terminal_at`].
/// Test: `retention_window_is_twenty_four_hours`.
pub const TERMINAL_RECORD_RETENTION_DAYS: i64 = 1;

/// What the sweep should do with one record.
///
/// Why: keeping the decision separate from the store and slot mutations makes
/// every branch — including the two that must never delete — testable without
/// a store, a daemon, or a clock.
/// What: `Keep` (leave it alone), `Stamp(when)` (terminal but never dated:
/// backfill `terminal_at` with the carried time), `Evict` (terminal, dated, and
/// past the window).
/// Test: `retention_verdict_*` in `retention_tests.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetentionVerdict {
    /// Leave the record in the store.
    Keep,
    /// Terminal with no `terminal_at`: backfill it with this inferred time.
    ///
    /// Carries the value rather than letting the caller pick, so
    /// [`inferred_terminal_at`]'s choice cannot be quietly overridden at the
    /// one call site that acts on it.
    Stamp(DateTime<Utc>),
    /// Terminal, dated, and older than the retention window: evict.
    Evict,
}

/// Best estimate of when a record without [`SessionRecord::terminal_at`]
/// entered its terminal state.
///
/// Why: every record written before `terminal_at` existed lacks it, and that is
/// the entire pre-existing backlog — 76 of 116 records on the reporting
/// machine, holding 76 of the slot numbers that put `NUM` in three digits.
/// Stamping those with `now` would grandfather them for another full retention
/// window, so the fix the owner asked for would not arrive until a day after he
/// installed it (and, pre-#5327, a week). Clearing that backlog is the point,
/// and a record whose last sign of life was two months ago has had its
/// retention window many times over.
///
/// The earlier reasoning against inference was right about the mechanism and
/// wrong about the goal: yes, the auto-prune (#4384/#4702) decommissions
/// long-idle records, so this can date a record earlier than it truly died —
/// but only for records that predate the field, and only in the direction of
/// clearing a backlog that is already weeks old.
///
/// What: the LATEST evidence the record carries that it was alive —
/// `last_activity_at` when set, else `created_at`, never earlier than
/// `created_at`. Taking the latest, not the earliest, keeps the inference as
/// conservative as the available data allows. A timestamp in the FUTURE is not
/// evidence of anything (clock skew, or a corrupt record); it falls back to
/// `now`, so such a record gets a full window rather than being deleted on the
/// strength of a value that cannot be true.
///
/// This is inherently one-time: every terminal transition from here on stamps a
/// real `terminal_at` via [`SessionRecord::set_lifecycle_state`], so no record
/// created after this ships reaches this function.
/// Test: `inferred_terminal_at_uses_the_latest_evidence_of_life`,
/// `inferred_terminal_at_falls_back_to_now_for_a_future_timestamp`.
pub fn inferred_terminal_at(record: &SessionRecord, now: DateTime<Utc>) -> DateTime<Utc> {
    let latest = record
        .last_activity_at
        .unwrap_or(record.created_at)
        .max(record.created_at);
    if latest > now { now } else { latest }
}

/// Decide the fate of one record.
///
/// Why: three separate guards have to hold at once, and each has a way of
/// going quietly wrong.
///
/// 1. **Only terminal records are in scope.** An `active`/`stopped`/`errored`
///    record is live work — a `stopped` session is explicitly resumable, with
///    its workspace intact.
///
/// 2. **A record whose workspace is a worktree on disk is never evicted**,
///    however old. This is not tidiness, it is the load-bearing guard.
///    `prune_orphaned_worktrees` protects a worktree from deletion by finding
///    its path in the set of `workspace_path`s read from the store — a set that
///    is *deliberately unfiltered by state*, because a record carrying a
///    terminal state is routinely a live session with unsaved work (see the
///    measured case in `prune::reap_orphaned_worktrees`). Both of that sweep's
///    two independent reads of the protected set come from the store, so
///    deleting the record removes the path from BOTH at once — collapsing a
///    defense-in-depth pair into nothing and handing the worktree to an
///    unattended, `dry_run: false` timer. `workspace_needs_protection` keeps
///    the record, and therefore the protection, alive for exactly as long as
///    there is a worktree to protect — see that function for why a plain
///    main-checkout path (#5274) is not one and what it costs to be wrong.
///
/// 3. **`terminal_at == None` is backfilled, never evicted directly.** A record
///    predating the field is dated from [`inferred_terminal_at`] and written
///    back; only a LATER sweep can evict it. That ordering is deliberate: the
///    inferred date lands on disk before anything acts on it, so an operator
///    can see what date a record was assigned rather than discovering it by its
///    absence. It also means an old legacy record still passes through the
///    two-observation debounce and phase-3 re-validation like any other.
///
/// What: `Keep` unless the record is terminal AND its workspace needs no
/// protection; then `Stamp(inferred)` when undated, `Evict` when `now -
/// terminal_at >= retention`, `Keep` otherwise. `workspace_needs_protection` is
/// supplied by the caller (via the function of the same name, which resolves an
/// undetermined probe to `true`) so this function stays pure and does no I/O.
/// Test: `retention_verdict_keeps_live_states`,
/// `retention_verdict_keeps_record_whose_worktree_still_exists`,
/// `retention_verdict_stamps_undated_terminal_record`,
/// `retention_verdict_keeps_record_inside_window`,
/// `retention_verdict_evicts_record_outside_window`.
pub fn retention_verdict(
    record: &SessionRecord,
    workspace_needs_protection: bool,
    now: DateTime<Utc>,
    retention: Duration,
) -> RetentionVerdict {
    if !record.state.is_terminal() || workspace_needs_protection {
        return RetentionVerdict::Keep;
    }
    match record.terminal_at {
        None => RetentionVerdict::Stamp(inferred_terminal_at(record, now)),
        Some(at) if now - at >= retention => RetentionVerdict::Evict,
        Some(_) => RetentionVerdict::Keep,
    }
}

/// What one retention sweep did.
///
/// Why: the daemon logs it, and tests assert on it, without either having to
/// re-derive the decision.
/// What: how many undated terminal records were stamped, and the ids evicted.
/// Test: `sweep_*` in `retention_tests.rs`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RetentionOutcome {
    /// Terminal records that had no `terminal_at` and were dated this sweep.
    pub stamped: usize,
    /// Records hard-deleted from the store, and whose slots were released.
    pub evicted: Vec<ManagedSessionId>,
}

impl RetentionOutcome {
    /// Whether this sweep changed anything — the common case is `false`.
    pub fn is_empty(&self) -> bool {
        self.stamped == 0 && self.evicted.is_empty()
    }
}

/// Would evicting this record expose a worktree to `prune_orphaned_worktrees`?
///
/// Why: a record's `workspace_path` is what puts a directory in that sweep's
/// protected set, so evicting the record is what can get the directory deleted.
/// Until #5327 the question asked here was the broader "is the workspace still
/// on disk", which was the right question when every session provisioned a
/// worktree. Under ADR-0037 as amended (#5274) the default session runs on the
/// project's own main checkout and records it as its `workspace_path` — a
/// directory that never goes away — so the broad form pinned every such record
/// in the store permanently, at any window. That is the measured cause of
/// #5327's unbounded `NUM`, and shortening the window alone would not have
/// moved it.
///
/// The narrowing is tied to what the sweep can actually reach. A candidate must
/// carry a `.trusty-mpm-worktree` ownership sentinel naming a resolvable owner
/// (#3649); an absent sentinel is reported and never removed. The sentinel has
/// exactly two write sites — `provisioner::workspace` and
/// `managed_routes::inproject` — and each writes it immediately after its own
/// `git worktree add`. A path with no sentinel was therefore not provisioned as
/// a session worktree, and the sweep will not delete it however the record
/// changes.
///
/// What: `probe` is `Path::try_exists` in production and answers both reads.
/// Protected when ANY of:
///
/// - the workspace path cannot be probed (`Err`) — `Path::exists()` collapses a
///   permission error, a stale NFS/SMB handle, and `EIO` from a departed volume
///   into "not there", and "not there" is what evicts;
/// - the path's immediate parent is a worktree base
///   ([`super::decommission::is_session_worktree`], a deliberate superset of
///   creation since #5204);
/// - a sentinel file exists at the path, or its existence cannot be determined.
///
/// The sentinel and shape clauses are independent on purpose. The sweep reads
/// the sentinel at deletion time, this reads it now, and a read that fails
/// transiently here would drop the protection for good while the sweep's own
/// later read succeeds; the shape clause covers that for the standard
/// `.worktrees` layout, and treating an undetermined sentinel probe as present
/// covers it for the rest. Unprotected only when there is no path, the path is
/// gone, or it is a plain directory with no sentinel — the main-checkout case.
/// #5897: `pub(super)` rather than module-private because
/// [`super::prune::PruneOutcome`]'s compaction branch releases the slot behind
/// the record it removes, and must gate that release on the SAME condition
/// this sweep does. A second copy of the predicate in `prune.rs` would drift,
/// and the two paths disagreeing about who may free a slot is what #5897 is.
/// Test: `workspace_needs_protection_treats_an_undetermined_path_as_protected`,
/// `workspace_needs_protection_covers_a_session_worktree`,
/// `workspace_needs_protection_ignores_a_plain_main_checkout`,
/// `prune_compaction_keeps_the_slot_when_the_worktree_is_still_on_disk`.
pub(super) fn workspace_needs_protection(
    path: Option<&std::path::Path>,
    names: &trusty_common::workspace_layout::WorktreeDirNames,
    probe: impl Fn(&std::path::Path) -> std::io::Result<bool>,
) -> bool {
    let Some(p) = path else {
        return false;
    };
    match probe(p) {
        // Undetermined: never evict on the strength of an answer we do not have.
        Err(_) => return true,
        Ok(false) => return false,
        Ok(true) => {}
    }
    super::decommission::is_session_worktree_with(p, names)
        || probe(&p.join(super::decommission::WORKTREE_SENTINEL_FILE)).unwrap_or(true)
}

/// Two-observation gate before a record may be evicted.
///
/// Why: the sibling destructive sweeps in the same orphan-GC tick
/// (`orphan_gc::OrphanGc`, `core::pid_registry::PidOrphanGc`) each require a
/// candidate to survive TWO consecutive observations before acting, and
/// retention deletes something no less permanent. The condition it reads is not
/// purely a persisted timestamp: "no worktree behind this workspace" is a live
/// filesystem observation, and an external volume that unmounts between ticks
/// answers `Ok(false)` — genuinely absent, indistinguishable from deleted —
/// which `workspace_needs_protection`'s error handling cannot catch.
/// Requiring the same verdict on two ticks ~60s apart costs one to two minutes
/// against a 24-hour window and rules that case out. #5327 kept it unchanged
/// and unrelaxed: shortening the window makes this sweep act sooner, which
/// raises what a single stale observation costs rather than lowering it.
/// What: [`Self::confirm`] returns the candidates that were ALSO candidates on
/// the previous call, then re-arms with the current set — so a record that
/// stops being a candidate (reactivated, workspace remounted) disarms itself.
/// Owned by the caller across ticks, exactly like its two siblings.
/// Test: `debounce_requires_two_consecutive_observations`,
/// `debounce_disarms_a_candidate_that_lapses`,
/// `sweep_spares_a_record_whose_worktree_appears_between_sweeps`.
#[derive(Debug, Default)]
pub struct RetentionDebounce {
    armed: std::collections::HashSet<ManagedSessionId>,
}

impl RetentionDebounce {
    /// A gate with nothing armed — nothing can be evicted on the first tick.
    pub fn new() -> Self {
        Self::default()
    }

    /// Return the candidates seen on the previous call too, and re-arm.
    pub fn confirm(&mut self, candidates: &[ManagedSessionId]) -> Vec<ManagedSessionId> {
        let confirmed: Vec<ManagedSessionId> = candidates
            .iter()
            .filter(|id| self.armed.contains(id))
            .copied()
            .collect();
        self.armed = candidates.iter().copied().collect();
        confirmed
    }
}

impl SessionManager {
    /// Evict terminal records past the retention window and free their slots.
    ///
    /// Why: see this module's doc. This is the only thing in the codebase that
    /// hard-deletes a record without an operator asking for that specific
    /// record by id, so its scope is deliberately narrow: `sessions.json`, and
    /// the in-memory slot registry. Nothing on disk outside the store file is
    /// read for deletion, opened for writing, or removed. It reads the config
    /// once per sweep to resolve the worktree base names, and otherwise its only
    /// filesystem calls are `workspace_needs_protection`'s two `try_exists`
    /// probes — every answer they can give other than "the workspace is gone" or
    /// "it is a plain directory with no ownership sentinel" means KEEP.
    /// What: three phases, mirroring `prune_orphaned_worktrees`'s #1845 item-9
    /// shape.
    ///
    /// 1. Snapshot the store and run [`retention_verdict`] over every record.
    ///    Undated terminal records are stamped and written back in one batch; a
    ///    record stamped by this sweep is never evicted by it, so every record
    ///    gets the full window.
    /// 2. Put the eviction candidates through `debounce`, which only passes
    ///    those that were candidates on the previous sweep as well.
    /// 3. RE-READ each surviving candidate from the store and re-run the
    ///    verdict against its current state and a fresh existence probe,
    ///    immediately before deleting. Phase 1's snapshot is stale by then: this
    ///    sweep's own `upsert_many` has landed, and another process may have
    ///    reactivated a record (`mark_reactivated`) or removed it. Only records
    ///    that still say `Evict` are removed, and only then are their slots
    ///    released.
    ///
    /// `now`, `retention` and `debounce` are parameters rather than read from
    /// the clock and a global so tests are deterministic.
    /// Test: `sweep_evicts_only_records_past_the_window`,
    /// `sweep_releases_the_evicted_slot`,
    /// `sweep_never_touches_a_worktree_on_the_filesystem`,
    /// `sweep_backfills_an_old_legacy_record_and_evicts_it_without_waiting`,
    /// `sweep_gives_a_recent_legacy_record_its_full_window`,
    /// `sweep_stamps_now_when_a_legacy_record_has_no_usable_signal`,
    /// `sweep_backfill_still_spares_a_legacy_record_whose_worktree_exists`,
    /// `sweep_spares_a_record_reactivated_between_sweeps` (which the debounce
    /// and phase 1 protect — phase 3's own arms are covered by
    /// [`Self::revalidate_for_eviction`]'s `revalidate_*` tests) in
    /// `super::retention_tests`.
    pub async fn sweep_terminal_records(
        &self,
        now: DateTime<Utc>,
        retention: Duration,
        debounce: &mut RetentionDebounce,
    ) -> Result<RetentionOutcome, ManagedError> {
        // Phase 1 — snapshot. Reload under a brief write lock, then snapshot via
        // the I/O-free `cached_all()` under a read lock (`reap_aged_ephemeral`'s
        // pattern).
        self.store.write().await.reload_if_changed().await?;
        let all = self.store.read().await.cached_all();

        // Resolve the worktree base names ONCE for the whole sweep. Doing it per
        // record would re-read config per record per 60s tick — see
        // `decommission::is_session_worktree_with`.
        let names = super::decommission::worktree_dir_names();

        let mut stamped: Vec<SessionRecord> = Vec::new();
        let mut candidates: Vec<ManagedSessionId> = Vec::new();
        for record in all {
            match self.verdict_for(&record, &names, now, retention) {
                RetentionVerdict::Keep => {}
                RetentionVerdict::Stamp(inferred) => {
                    let mut dated = record;
                    dated.terminal_at = Some(inferred);
                    stamped.push(dated);
                }
                RetentionVerdict::Evict => candidates.push(record.id),
            }
        }

        let n = stamped.len();
        if n > 0 {
            self.store.write().await.upsert_many(stamped).await?;
            info!(
                stamped = n,
                "retention: backfilled `terminal_at` on {n} record(s) that predate the field, \
                 from their last evidence of life; any already older than {} hour(s) become \
                 evictable on the next sweep",
                retention.num_hours()
            );
        }

        // Phase 2 — a candidate must have been one on the previous sweep too.
        let confirmed = debounce.confirm(&candidates);

        // Phase 3 — re-validate against the CURRENT store immediately before
        // deleting.
        let evicted = self
            .revalidate_for_eviction(confirmed, &names, now, retention)
            .await;

        if !evicted.is_empty() {
            self.store.write().await.remove_many(&evicted).await?;
            let mut reg = self.slots.write().await;
            for id in &evicted {
                reg.release(id);
            }
            info!(
                evicted = evicted.len(),
                "retention: evicted terminal record(s) older than {} hour(s) and freed their slots",
                retention.num_hours()
            );
        }

        Ok(RetentionOutcome {
            stamped: n,
            evicted,
        })
    }

    /// Drop any confirmed candidate that no longer deserves eviction, reading
    /// the CURRENT store rather than the sweep's snapshot.
    ///
    /// Why: `confirmed` was derived from a snapshot taken earlier in the sweep,
    /// and by the time it reaches here that snapshot is stale — this sweep's own
    /// `upsert_many` has landed, and another process may have reactivated
    /// (`mark_reactivated`) or removed a record in between. Deleting straight
    /// from the snapshot is the mistake `prune_orphaned_worktrees` already fixed
    /// once (#1845 item 9); this is the same re-read, immediately before the
    /// destructive step.
    ///
    /// It is a separate method so it is REACHABLE from a test. Inside the full
    /// sweep it is not: phase 1 re-derives candidates from scratch every tick,
    /// so a record that changed between ticks is dropped there and never
    /// reaches here — meaning an end-to-end test of "reactivate between sweeps"
    /// exercises the debounce, not this guard, however it is named. Taking the
    /// candidate list as a plain argument is not a test hook: a list computed
    /// from an older view of the store IS the stale snapshot this defends
    /// against, and passing one directly is the only way to observe the two
    /// non-evicting arms fire.
    /// What: for each id, re-reads the record (`get` reloads from disk, so it
    /// sees another process's write) and re-runs [`Self::verdict_for`] with a
    /// fresh existence probe. Keeps only ids that still say `Evict`; a changed
    /// record is left in place and logged, and one already gone is silently
    /// skipped — a concurrent prune beating us to it is not an error.
    /// Test: `revalidate_keeps_a_still_evictable_record`,
    /// `revalidate_drops_a_record_reactivated_after_the_snapshot`,
    /// `revalidate_drops_a_record_whose_worktree_appeared_after_the_snapshot`,
    /// `revalidate_drops_a_record_a_concurrent_prune_already_removed`.
    async fn revalidate_for_eviction(
        &self,
        confirmed: Vec<ManagedSessionId>,
        names: &trusty_common::workspace_layout::WorktreeDirNames,
        now: DateTime<Utc>,
        retention: Duration,
    ) -> Vec<ManagedSessionId> {
        let mut evicted = Vec::new();
        for id in confirmed {
            match self.get(&id).await {
                Ok(fresh)
                    if self.verdict_for(&fresh, names, now, retention)
                        == RetentionVerdict::Evict =>
                {
                    evicted.push(id);
                }
                Ok(_) => warn!(
                    id = %id,
                    "retention: candidate changed between snapshot and delete; leaving it in place"
                ),
                // Already gone (a concurrent prune won the race) — nothing to do.
                Err(_) => {}
            }
        }
        evicted
    }

    /// [`Self::sweep_terminal_records`] against the wall clock and the shipped
    /// [`TERMINAL_RECORD_RETENTION_DAYS`] window.
    ///
    /// Why: the daemon's GC tick wants neither of those two parameters — they
    /// exist so tests can drive the sweep deterministically. Keeping the
    /// defaults here rather than at the call site also keeps `daemon/mod.rs`
    /// under the 500-SLOC cap.
    /// What: `sweep_terminal_records(Utc::now(), Duration::days(WINDOW), debounce)`.
    /// Test: covered through `sweep_terminal_records`'s own tests.
    pub async fn sweep_default_retention(
        &self,
        debounce: &mut RetentionDebounce,
    ) -> Result<RetentionOutcome, ManagedError> {
        let window = Duration::days(TERMINAL_RECORD_RETENTION_DAYS);
        self.sweep_terminal_records(Utc::now(), window, debounce)
            .await
    }

    /// [`retention_verdict`] with the production existence probes applied.
    ///
    /// `names` is resolved ONCE per sweep by the caller rather than per record —
    /// see [`super::decommission::is_session_worktree_with`] for why a per-record
    /// resolve would re-read config and re-log on every tick.
    fn verdict_for(
        &self,
        record: &SessionRecord,
        names: &trusty_common::workspace_layout::WorktreeDirNames,
        now: DateTime<Utc>,
        retention: Duration,
    ) -> RetentionVerdict {
        let protected =
            workspace_needs_protection(record.workspace_path.as_deref(), names, |p| p.try_exists());
        retention_verdict(record, protected, now, retention)
    }
}

#[cfg(test)]
#[path = "retention_tests.rs"]
mod tests;
