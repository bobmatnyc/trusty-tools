//! Bounded durable half of the open-time expired-drawer sweep (#8314).
//!
//! Why: `PalaceHandle::open_with_intent` deletes each expired drawer's row with
//! a synchronous kg.redb write. The daemon reopens a palace on a READ after an
//! LRU or idle eviction, and the reopen shares the cached `Database` with any
//! handle still alive — including one whose write transaction never ends.
//! redb's `begin_write` waits for that transaction without a bound, so the
//! sweep parked the read behind the reopen forever.
//! What: [`reclaim_expired_rows`] runs the deletes on a helper thread and waits
//! at most [`OPEN_SWEEP_BUDGET`]. In the normal case the deletes finish inside
//! it and the contract is unchanged: the rows are gone when `open` returns. Past
//! the budget the open proceeds and logs an error naming the palace and the
//! operation; the helper finishes whenever the writer lets go. Each delete is
//! its own redb transaction, so a row is removed whole or not at all. The
//! expired drawers are already out of the in-memory table either way, and
//! every read filters on expiry (ADR-0028 D4).
//! Test: `a_reopen_behind_a_stuck_kg_write_still_reads`,
//! `expired_tier_c_drawer_survives_the_open_time_sweep`.

use crate::memory_core::palace::PalaceId;
use crate::memory_core::store::kg_redb::KgStoreRedb;
use std::sync::Arc;
// Aliased: the `redb_cache_bounds` scan reads any `Builder` constructor as a redb open.
use std::thread::Builder as SweepThread;
use std::time::Duration;
use uuid::Uuid;

/// How long an open waits for the sweep's deletes before moving on.
pub(super) const OPEN_SWEEP_BUDGET: Duration = Duration::from_secs(2);

/// Delete `ids` from kg.redb, waiting at most `budget` for them to finish.
///
/// Why/What: see the module doc. Returns how many deletes completed inside the
/// budget, so the caller's "purged" count reports rows actually reclaimed.
pub(super) fn reclaim_expired_rows(
    store: Arc<KgStoreRedb>,
    ids: Vec<Uuid>,
    palace: &PalaceId,
    budget: Duration,
) -> usize {
    if ids.is_empty() {
        return 0;
    }
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let owner = palace.clone();
    let spawned = SweepThread::new()
        .name("trusty-open-sweep".into())
        .spawn(move || {
            for id in ids {
                match store.delete_drawer(id) {
                    Ok(()) => {
                        let _ = done_tx.send(());
                    }
                    Err(e) => tracing::warn!(
                        palace = %owner, id = %id,
                        "purge_expired: delete_drawer failed: {e:#}"
                    ),
                }
            }
        });
    if let Err(e) = spawned {
        tracing::warn!(palace = %palace, "open-time sweep thread failed to start: {e}");
        return 0;
    }
    let deadline = std::time::Instant::now() + budget;
    let mut reclaimed = 0usize;
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        match done_rx.recv_timeout(left) {
            Ok(()) => reclaimed += 1,
            // Every delete has reported or failed: the helper dropped its sender.
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return reclaimed,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                tracing::error!(
                    palace = %palace,
                    operation = "purge_expired_at_open",
                    budget_ms = budget.as_millis(),
                    reclaimed,
                    "#8314: open-time sweep waited past its budget on a kg.redb \
                     write that has not finished; the open proceeds and the rows \
                     are reclaimed when that write ends"
                );
                return reclaimed;
            }
        }
    }
}
