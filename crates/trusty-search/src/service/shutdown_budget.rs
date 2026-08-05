//! The process's real shutdown window, and the per-index flush deadlines it
//! may mint (#4393).
//!
//! Why: `shutdown_flush::shutdown_flush_deadline_for` sizes each index's flush
//! budget from that index's on-disk snapshot — 30 s floor, 20 min ceiling — and
//! never once asks how long the process is going to live. It cannot: nothing in
//! the flush path knew. So the daemon planned a 30-second-per-index sweep inside
//! a 5-second life, and every terminator in the system SIGKILLed it partway
//! through: launchd's `ExitTimeOut` default (measured 5 s), `trusty-search
//! stop`'s wait (5 s), the orphan reaper's wait (3 s). The flush that "has a
//! generous budget" had, in practice, never run to completion when it had real
//! work to do.
//!
//! Raising the windows is half the fix ([`trusty_common::shutdown::
//! TERMINATION_GRACE_SECS`], rendered into every plist as `ExitTimeOut`). This
//! module is the other half, and the durable one: a per-index deadline is now a
//! [`FlushDeadline`], and the ONLY way to obtain one is to ask a
//! [`ShutdownBudget`] — which subtracts elapsed time and hands back `None` once
//! the process is out of window. The flush loop is therefore structurally
//! incapable of granting an index more time than the process has left, and a
//! sweep that runs out of window stops cleanly at an index boundary instead of
//! being cut off mid-write.
//!
//! What an interrupted sweep costs, precisely: nothing beyond the last
//! checkpoint. `UsearchStore::save` writes to a staging file and renames, so a
//! flush that never starts and a flush that is abandoned leave the same on-disk
//! state — whatever the incremental persister last published (every
//! `HNSW_SNAPSHOT_BATCH_INTERVAL` batches). Stopping at an index boundary is
//! what converts "SIGKILLed at an arbitrary point" into "these N indexes were
//! flushed, these M kept their last checkpoint", which is a bounded, logged
//! outcome rather than a silent one.
//!
//! What: [`ShutdownBudget`] (the window, counted from SIGTERM) and
//! [`FlushDeadline`] (an unforgeable per-index grant).
//!
//! Test: `shutdown_budget_tests.rs`.

use std::path::Path;
use std::time::{Duration, Instant};

/// Wall-clock time held back from the flush for post-flush cleanup.
///
/// Why: `run_daemon` still has work to do after the sweep returns — remove the
/// port file, remove `http_addr`, deregister shared discovery, release the
/// lockfile, exit. Spending the entire window on flushing would hand SIGKILL the
/// cleanup instead of the flush, trading one truncated step for another.
/// What: 5 s, subtracted from the grace window when a budget is created.
/// Test: `budget_reserves_time_for_post_flush_cleanup`.
pub(crate) const CLEANUP_RESERVE: Duration = Duration::from_secs(5);

/// How much of its life this process has left to spend on the shutdown flush.
///
/// Why: see the module doc. The type exists so that "how long may this index
/// take?" can only be answered by something that knows how long the process has
/// left — a plain `Duration` parameter is exactly what let a 20-minute deadline
/// be handed to a 5-second process.
/// What: an absolute deadline, created from the termination window at the
/// moment SIGTERM was observed, minus [`CLEANUP_RESERVE`].
/// Test: `shutdown_budget_tests.rs`.
#[derive(Debug, Clone, Copy)]
pub struct ShutdownBudget {
    deadline: Instant,
}

/// A per-index flush grant that cannot outlive the process (#4393).
///
/// Why: the unforgeable half of the fix. The field is private and this module
/// exposes no constructor from a bare `Duration`, so `shutdown_flush` — a
/// different module — literally cannot write `FlushDeadline(Duration::from_secs(
/// 1200))`. Every deadline that reaches [`crate::service::shutdown_flush`] has
/// passed through [`ShutdownBudget::flush_deadline_for`] and been clamped there.
/// What: wraps the granted duration; [`Self::as_duration`] reads it back.
/// Test: `flush_deadline_is_clamped_to_the_remaining_window`.
#[derive(Debug, Clone, Copy)]
pub struct FlushDeadline(Duration);

impl FlushDeadline {
    /// The granted duration.
    pub fn as_duration(self) -> Duration {
        self.0
    }
}

impl ShutdownBudget {
    /// Open a budget for a shutdown that began at `sigterm_at`.
    ///
    /// Why: the window starts when SIGTERM lands, not when the flush starts —
    /// the axum drain and watcher teardown happen in between and spend real
    /// time. Taking the instant as a parameter is what lets `run_daemon` record
    /// it in the signal handler and charge that time honestly.
    /// What: `sigterm_at + termination_grace() - CLEANUP_RESERVE`. A grace
    /// shorter than the reserve saturates to zero rather than wrapping, so a
    /// tiny declared window yields a budget that is immediately exhausted (no
    /// flush) rather than an accidentally enormous one.
    /// Test: `budget_reserves_time_for_post_flush_cleanup`,
    /// `budget_shorter_than_the_cleanup_reserve_is_exhausted`.
    pub fn started_at(sigterm_at: Instant) -> Self {
        Self::from_window_at(sigterm_at, trusty_common::shutdown::termination_grace())
    }

    /// [`Self::started_at`] with an explicit window instead of the configured
    /// one — the seam the tests drive.
    pub fn from_window_at(sigterm_at: Instant, window: Duration) -> Self {
        Self {
            deadline: sigterm_at + window.saturating_sub(CLEANUP_RESERVE),
        }
    }

    /// [`Self::from_window_at`] anchored at now.
    pub fn from_window(window: Duration) -> Self {
        Self::from_window_at(Instant::now(), window)
    }

    /// Time left before the process must stop flushing and start cleaning up.
    pub fn remaining(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }

    /// Whether the window is spent.
    pub fn is_exhausted(&self) -> bool {
        self.remaining().is_zero()
    }

    /// Mint the flush grant for one index, or `None` if the window is spent.
    ///
    /// Why: THE #4393 fix, in one function. `shutdown_flush_deadline_for` still
    /// decides how long this index *deserves* from its snapshot size; this
    /// clamps that to how long the process *has*. Before, only the first half
    /// existed, so index #1 could be granted 20 minutes of a 5-second life and
    /// indexes #2..N were never reached at all.
    /// What: `min(size-scaled deadline, remaining window)`, or `None` when the
    /// budget is exhausted — which the caller reports as a skip, leaving that
    /// index on its last incremental checkpoint.
    /// Test: `flush_deadline_is_clamped_to_the_remaining_window`,
    /// `exhausted_budget_mints_no_deadline`,
    /// `flush_deadline_keeps_the_size_scaled_value_when_the_window_is_ample`.
    pub fn flush_deadline_for(&self, hnsw_path: &Path) -> Option<FlushDeadline> {
        let remaining = self.remaining();
        if remaining.is_zero() {
            return None;
        }
        let wanted = crate::service::shutdown_flush::shutdown_flush_deadline_for(hnsw_path);
        Some(FlushDeadline(wanted.min(remaining)))
    }
}

#[cfg(test)]
#[path = "shutdown_budget_tests.rs"]
mod tests;
