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
//! Scope, and the reason it is drawn this tightly: this sweep touches
//! `sessions.json` and nothing else. It never removes a worktree, a workspace
//! directory, a git branch, or any other file — see [`retention_verdict`]'s
//! `workspace_on_disk` parameter for the guard that keeps a record-only
//! eviction from becoming a filesystem deletion by proxy, and
//! [`workspace_present`] for why an undetermined path counts as present.
//!
//! Test: `retention_tests.rs`.

use chrono::{DateTime, Duration, Utc};
use tracing::{info, warn};

use super::manager::{ManagedError, SessionManager};
use super::record::{ManagedSessionId, SessionRecord};

/// How long a record stays in the store after entering a terminal state.
///
/// Why: the owner's ruling on the inflated-`NUM` report — "evict old records,
/// 7 days". Long enough that a tombstone an operator captured a number from is
/// still there days later (#3034's guarantee, which in practice never survived
/// a daemon restart anyway); short enough that the store stops being an
/// append-only log of every session ever run.
/// What: the retention window in days, applied to
/// [`SessionRecord::terminal_at`].
/// Test: `retention_window_is_seven_days`.
pub const TERMINAL_RECORD_RETENTION_DAYS: i64 = 7;

/// What the sweep should do with one record.
///
/// Why: keeping the decision separate from the store and slot mutations makes
/// every branch — including the two that must never delete — testable without
/// a store, a daemon, or a clock.
/// What: `Keep` (leave it alone), `Stamp` (terminal but never dated: start its
/// clock now), `Evict` (terminal, dated, and past the window).
/// Test: `retention_verdict_*` in `retention_tests.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetentionVerdict {
    /// Leave the record in the store.
    Keep,
    /// Terminal with no `terminal_at`: stamp it and start the window now.
    Stamp,
    /// Terminal, dated, and older than the retention window: evict.
    Evict,
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
/// 2. **A record whose workspace still exists on disk is never evicted**,
///    however old. This is not tidiness, it is the load-bearing guard.
///    `prune_orphaned_worktrees` protects a worktree from deletion by finding
///    its path in the set of `workspace_path`s read from the store — a set that
///    is *deliberately unfiltered by state*, because a record carrying a
///    terminal state is routinely a live session with unsaved work (see the
///    measured case in `prune::reap_orphaned_worktrees`). Both of that sweep's
///    two independent reads of the protected set come from the store, so
///    deleting the record removes the path from BOTH at once — collapsing a
///    defense-in-depth pair into nothing and handing the worktree to an
///    unattended, `dry_run: false` timer. `workspace_on_disk` keeps the record,
///    and therefore the protection, alive for exactly as long as there is a
///    directory to protect.
///
/// 3. **`terminal_at == None` means UNKNOWN, not old.** It is stamped, not
///    evicted. Guessing the death time from `last_activity_at`/`created_at`
///    would be wrong in the common direction: the auto-prune (#4384/#4702)
///    decommissions long-idle records, so a record whose last activity is a
///    month old may have become terminal seconds ago, and inferring from
///    activity would evict it on the first sweep with no retention at all.
///    Stamping costs one write and one retention window, once, per legacy
///    record.
///
/// What: `Keep` unless the record is terminal AND its workspace is not on
/// disk; then `Stamp` when undated, `Evict` when `now - terminal_at >=
/// retention`, `Keep` otherwise. `workspace_on_disk` is supplied by the caller
/// (via [`workspace_present`], which resolves an undetermined path to `true`)
/// so this function stays pure and does no I/O.
/// Test: `retention_verdict_keeps_live_states`,
/// `retention_verdict_keeps_record_whose_workspace_still_exists`,
/// `retention_verdict_stamps_undated_terminal_record`,
/// `retention_verdict_keeps_record_inside_window`,
/// `retention_verdict_evicts_record_outside_window`.
pub fn retention_verdict(
    record: &SessionRecord,
    workspace_on_disk: bool,
    now: DateTime<Utc>,
    retention: Duration,
) -> RetentionVerdict {
    if !record.state.is_terminal() || workspace_on_disk {
        return RetentionVerdict::Keep;
    }
    match record.terminal_at {
        None => RetentionVerdict::Stamp,
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

/// Is this record's workspace directory still on disk?
///
/// Why: `Path::exists()` maps EVERY stat error to `false` — a permission error
/// on an ancestor, a stale NFS/SMB handle, `EIO` from a departed volume — so
/// "cannot determine" and "not there" produce the same answer, and that answer
/// evicts. The guard this feeds is the one keeping a record's `workspace_path`
/// in `prune_orphaned_worktrees`'s protected set, so failing open here means a
/// transient stat error can hand a live worktree to an unattended sweep.
/// What: `probe` is `Path::try_exists` in production. An `Err` — undetermined —
/// counts as PRESENT, so an unreadable path is never evicted. A `None` path is
/// absent: there is nothing to protect and nothing to fail on.
/// Test: `workspace_present_treats_an_undetermined_path_as_present`.
fn workspace_present(
    path: Option<&std::path::Path>,
    probe: impl Fn(&std::path::Path) -> std::io::Result<bool>,
) -> bool {
    path.is_some_and(|p| probe(p).unwrap_or(true))
}

/// Two-observation gate before a record may be evicted.
///
/// Why: the sibling destructive sweeps in the same orphan-GC tick
/// (`orphan_gc::OrphanGc`, `core::pid_registry::PidOrphanGc`) each require a
/// candidate to survive TWO consecutive observations before acting, and
/// retention deletes something no less permanent. The condition it reads is not
/// purely a persisted timestamp: "the workspace directory is gone" is a live
/// filesystem observation, and an external volume that unmounts between ticks
/// answers `Ok(false)` — genuinely absent, indistinguishable from deleted —
/// which [`workspace_present`]'s error handling cannot catch. Requiring the
/// same verdict on two ticks ~60s apart costs nothing against a 7-day window
/// and rules that case out.
/// What: [`Self::confirm`] returns the candidates that were ALSO candidates on
/// the previous call, then re-arms with the current set — so a record that
/// stops being a candidate (reactivated, workspace remounted) disarms itself.
/// Owned by the caller across ticks, exactly like its two siblings.
/// Test: `debounce_requires_two_consecutive_observations`,
/// `debounce_disarms_a_candidate_that_lapses`.
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
    /// read for deletion, opened for writing, or removed. Its only filesystem
    /// call is [`workspace_present`]'s `try_exists`, whose two non-`Ok(false)`
    /// answers — present, or undetermined — both mean KEEP, so it can only ever
    /// make the sweep evict fewer records.
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
    /// `sweep_never_touches_the_filesystem`,
    /// `sweep_stamps_legacy_records_instead_of_evicting_them`,
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

        let mut stamped: Vec<SessionRecord> = Vec::new();
        let mut candidates: Vec<ManagedSessionId> = Vec::new();
        for record in all {
            match self.verdict_for(&record, now, retention) {
                RetentionVerdict::Keep => {}
                RetentionVerdict::Stamp => {
                    let mut dated = record;
                    dated.terminal_at = Some(now);
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
                "retention: dated {n} terminal record(s) that predate `terminal_at`; \
                 they become evictable in {} day(s)",
                retention.num_days()
            );
        }

        // Phase 2 — a candidate must have been one on the previous sweep too.
        let confirmed = debounce.confirm(&candidates);

        // Phase 3 — re-validate against the CURRENT store immediately before
        // deleting.
        let evicted = self
            .revalidate_for_eviction(confirmed, now, retention)
            .await;

        if !evicted.is_empty() {
            self.store.write().await.remove_many(&evicted).await?;
            let mut reg = self.slots.write().await;
            for id in &evicted {
                reg.release(id);
            }
            info!(
                evicted = evicted.len(),
                "retention: evicted terminal record(s) older than {} day(s) and freed their slots",
                retention.num_days()
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
    /// `revalidate_drops_a_record_a_concurrent_prune_already_removed`.
    async fn revalidate_for_eviction(
        &self,
        confirmed: Vec<ManagedSessionId>,
        now: DateTime<Utc>,
        retention: Duration,
    ) -> Vec<ManagedSessionId> {
        let mut evicted = Vec::new();
        for id in confirmed {
            match self.get(&id).await {
                Ok(fresh)
                    if self.verdict_for(&fresh, now, retention) == RetentionVerdict::Evict =>
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

    /// [`retention_verdict`] with the production existence probe applied.
    fn verdict_for(
        &self,
        record: &SessionRecord,
        now: DateTime<Utc>,
        retention: Duration,
    ) -> RetentionVerdict {
        let on_disk = workspace_present(record.workspace_path.as_deref(), |p| p.try_exists());
        retention_verdict(record, on_disk, now, retention)
    }
}

#[cfg(test)]
#[path = "retention_tests.rs"]
mod tests;
