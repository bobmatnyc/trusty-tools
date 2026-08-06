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
//! eviction from becoming a filesystem deletion by proxy.
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
/// (`record.workspace_path.as_deref().is_some_and(Path::exists)`) so this
/// function stays pure and does no I/O.
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

impl SessionManager {
    /// Evict terminal records past the retention window and free their slots.
    ///
    /// Why: see this module's doc. This is the only thing in the codebase that
    /// hard-deletes a record without an operator asking for that specific
    /// record by id, so its scope is deliberately narrow: `sessions.json`, and
    /// the in-memory slot registry. Nothing on disk outside the store file is
    /// read for deletion, opened for writing, or removed — the one filesystem
    /// call it makes is an existence check that can only ever make it evict
    /// FEWER records.
    /// What: snapshots the store, runs [`retention_verdict`] over every record,
    /// writes the stamped records back in one batch, removes the evicted ones
    /// in one batch, then releases each evicted id's slot so the number is
    /// reusable. A record stamped by this sweep is never evicted by the same
    /// sweep, so every record gets the full window. `now` and `retention` are
    /// parameters rather than read from the clock so tests are deterministic.
    /// Test: `sweep_evicts_only_records_past_the_window`,
    /// `sweep_releases_the_evicted_slot`,
    /// `sweep_never_touches_the_filesystem`,
    /// `sweep_stamps_legacy_records_instead_of_evicting_them` in
    /// `super::retention_tests`.
    pub async fn sweep_terminal_records(
        &self,
        now: DateTime<Utc>,
        retention: Duration,
    ) -> Result<RetentionOutcome, ManagedError> {
        // Reload under a brief write lock, then snapshot via the I/O-free
        // `cached_all()` under a read lock — the `reap_aged_ephemeral` pattern.
        self.store.write().await.reload_if_changed().await?;
        let all = self.store.read().await.cached_all();

        let mut stamped: Vec<SessionRecord> = Vec::new();
        let mut evicted: Vec<ManagedSessionId> = Vec::new();
        for record in all {
            let on_disk = record
                .workspace_path
                .as_deref()
                .is_some_and(std::path::Path::exists);
            match retention_verdict(&record, on_disk, now, retention) {
                RetentionVerdict::Keep => {}
                RetentionVerdict::Stamp => {
                    let mut dated = record;
                    dated.terminal_at = Some(now);
                    stamped.push(dated);
                }
                RetentionVerdict::Evict => evicted.push(record.id),
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

        let removed = self.store.write().await.remove_many(&evicted).await?;
        if removed != evicted.len() {
            // The ids came from a snapshot; a concurrent prune may have removed
            // one first. Harmless, but say so rather than reporting a count the
            // store did not perform.
            warn!(
                asked = evicted.len(),
                removed, "retention: some records were already gone from the store"
            );
        }
        if !evicted.is_empty() {
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
}

#[cfg(test)]
#[path = "retention_tests.rs"]
mod tests;
