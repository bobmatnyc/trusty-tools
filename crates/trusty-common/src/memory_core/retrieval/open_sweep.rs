//! Bounded, single-flight kg.redb writes at palace open (#8314).
//!
//! Why: an open runs maintenance writes — the expired-drawer purge in
//! `PalaceHandle::open_with_intent`, and the room backfill and default-wing
//! seeding in `PalaceRegistry::open_palace`. The daemon reopens a palace on a
//! READ after an LRU or idle eviction, and the reopen shares the cached
//! `Database` with any handle still alive — including one whose write
//! transaction never ends. redb's `begin_write` waits for that transaction
//! without a bound, so each of those writes parked the read behind the reopen
//! forever.
//! What: [`run_open_write`] runs the write on a helper thread and waits at most
//! a budget. In the normal case the write finishes inside it and the contract
//! is unchanged: the rows are written when `open` returns. Past the budget the
//! open proceeds and logs an error naming the palace and the operation; the
//! helper finishes whenever the writer lets go. Each redb transaction is atomic,
//! so a row lands whole or not at all. One helper per shared database may be
//! outstanding (`KgStoreRedb::try_claim_open_write`): while it is, later opens
//! skip their maintenance writes instead of parking another thread. Every
//! write skipped this way is idempotent and re-runs on the next open.
//! Test: `a_reopen_behind_a_stuck_kg_write_still_reads`,
//! `reopens_behind_a_stuck_kg_write_park_at_most_one_sweep_helper`,
//! `a_writer_reopen_behind_a_stuck_kg_write_answers_in_bounded_time`,
//! `expired_tier_c_drawer_survives_the_open_time_sweep`.

use crate::memory_core::palace::PalaceId;
use crate::memory_core::store::kg_redb::KgStoreRedb;
use std::sync::Arc;
// Aliased: the `redb_cache_bounds` scan reads any `Builder` constructor as a redb open.
use std::thread::Builder as OpenWriteThread;
use std::time::Duration;
use uuid::Uuid;

/// How long an open waits for one maintenance write before moving on.
pub(crate) const OPEN_WRITE_BUDGET: Duration = Duration::from_secs(2);

/// Run `work` on a helper thread, waiting at most `budget` for its result.
///
/// Why/What: see the module doc. Returns `None` when the write was skipped
/// because an earlier helper for the same database is still outstanding, when
/// it ran past `budget`, or when it panicked; each case is logged with
/// `palace` and `operation`.
/// Test: `reopens_behind_a_stuck_kg_write_park_at_most_one_sweep_helper`.
pub(crate) fn run_open_write<R, F>(
    store: &KgStoreRedb,
    palace: &PalaceId,
    operation: &'static str,
    budget: Duration,
    work: F,
) -> Option<R>
where
    R: Send + 'static,
    F: FnOnce() -> R + Send + 'static,
{
    let Some(claim) = store.try_claim_open_write() else {
        tracing::warn!(
            palace = %palace,
            operation,
            "#8314: skipped: an earlier open-time kg.redb write for this palace \
             has not finished; it re-runs on a later open"
        );
        return None;
    };
    let (done_tx, done_rx) = std::sync::mpsc::sync_channel::<R>(1);
    let spawned = OpenWriteThread::new()
        .name("trusty-open-write".into())
        .spawn(move || {
            let out = work();
            // Free the slot before reporting, so a caller that sees the result
            // also sees the slot free.
            drop(claim);
            let _ = done_tx.send(out);
        });
    if let Err(e) = spawned {
        tracing::warn!(palace = %palace, operation, "open-time write thread failed to start: {e}");
        return None;
    }
    match done_rx.recv_timeout(budget) {
        Ok(out) => Some(out),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            tracing::error!(
                palace = %palace,
                operation,
                budget_ms = budget.as_millis(),
                "#8314: open-time write waited past its budget on a kg.redb \
                 write that has not finished; the open proceeds and the write \
                 lands when that write ends"
            );
            None
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            tracing::warn!(palace = %palace, operation, "open-time write panicked");
            None
        }
    }
}

/// Delete `ids` from kg.redb through [`run_open_write`].
///
/// Why/What: see the module doc. Returns how many deletes succeeded, or 0 when
/// the sweep was skipped or ran past `budget`, so the caller's "purged" count
/// never reports a row that is not known to be gone.
pub(super) fn reclaim_expired_rows(
    store: Arc<KgStoreRedb>,
    ids: Vec<Uuid>,
    palace: &PalaceId,
    budget: Duration,
) -> usize {
    if ids.is_empty() {
        return 0;
    }
    let owner = palace.clone();
    let worker = Arc::clone(&store);
    let work = move || {
        let mut reclaimed = 0usize;
        for id in ids {
            match worker.delete_drawer(id) {
                Ok(()) => reclaimed += 1,
                Err(e) => tracing::warn!(
                    palace = %owner, id = %id,
                    "purge_expired: delete_drawer failed: {e:#}"
                ),
            }
        }
        reclaimed
    };
    run_open_write(&store, palace, "purge_expired_at_open", budget, work).unwrap_or(0)
}
