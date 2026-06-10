//! Embedder stall tracker — shared atomic state updated whenever an embed call
//! succeeds or times out.
//!
//! Why (issue #1003): the `trusty-embedderd` sidecar can stall for tens of
//! minutes (ANE/CoreML session stall on Apple Silicon) while remaining alive
//! and reachable. Before this module, `/health` reported `"embedder":"ready"`
//! throughout the stall because the sidecar process was still running.
//! `EmbedderStallTracker` captures the FUNCTIONAL state (did the last embed
//! call time out?) so the health handler can surface `"stalled"` with
//! supporting fields (`embedder_last_ok_secs_ago`,
//! `embedder_recent_timeout_count`).
//!
//! What: four lock-free atomics:
//!   - `last_ok_unix_secs`   — Unix seconds of the most recent successful embed.
//!     `0` = never succeeded.
//!   - `recent_timeout_count` — rolling count of recent timeouts, incremented on
//!     each error and reset to 0 on each success.
//!   - `total_ok_count`      — lifetime successful-embed counter.
//!   - `total_timeout_count` — lifetime embed-timeout counter.
//!
//! Test: `stall_tracker_records_success_and_timeout` and
//! `stall_tracker_reset_on_success` in the `tests` submodule.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// Lock-free tracker for embedder response health (issue #1003).
///
/// Why: avoids any mutex in the critical embed path; all fields are
/// independently atomic so reads on the health handler path are always
/// wait-free. The `recent_timeout_count` is conservative: it increments on
/// every timeout-flavoured error and resets on every success. Operators can
/// treat count > 0 as "degraded" and count == 0 as "healthy".
///
/// What: wraps four `AtomicU64`/`AtomicU32` counters. `record_success` and
/// `record_timeout` are the only write paths.
///
/// Test: `stall_tracker_records_success_and_timeout` in `tests` below.
#[derive(Debug, Default)]
pub struct EmbedderStallTracker {
    /// Unix seconds of the last successful `embed_batch` call.
    /// `0` = embed has never returned successfully.
    last_ok_unix_secs: AtomicU64,
    /// Rolling count of consecutive/recent timeouts; reset to 0 on success.
    recent_timeout_count: AtomicU32,
    /// Lifetime count of successful embed calls (for metrics / debugging).
    total_ok_count: AtomicU64,
    /// Lifetime count of embed-timeout errors.
    total_timeout_count: AtomicU64,
}

impl EmbedderStallTracker {
    /// Construct a new tracker with zeroed counters.
    ///
    /// Why: default construction is fine; no side effects at creation time.
    /// What: all atomics start at 0.
    /// Test: `stall_tracker_records_success_and_timeout`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one successful embed response.
    ///
    /// Why: resets `recent_timeout_count` so the stall signal clears as soon as
    /// the embedder recovers (the ANE stall self-heals once fresh requests arrive).
    /// What: stores the current Unix second into `last_ok_unix_secs`, atomically
    /// resets `recent_timeout_count` to 0, and increments `total_ok_count`.
    /// Test: `stall_tracker_reset_on_success`.
    pub fn record_success(&self) {
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        self.last_ok_unix_secs.store(now_secs, Ordering::Relaxed);
        self.recent_timeout_count.store(0, Ordering::Relaxed);
        self.total_ok_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Record one embed-timeout (or other transient failure) event.
    ///
    /// Why: increments the rolling counter so `/health` can surface
    /// `embedder_recent_timeout_count > 0` as a degraded signal.
    /// What: increments both `recent_timeout_count` and `total_timeout_count`.
    /// `last_ok_unix_secs` is intentionally NOT updated.
    /// Test: `stall_tracker_records_success_and_timeout`.
    pub fn record_timeout(&self) {
        self.recent_timeout_count.fetch_add(1, Ordering::Relaxed);
        self.total_timeout_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Seconds elapsed since the last successful embed call, or `None` when the
    /// embedder has never returned a successful result yet.
    ///
    /// Why: surfaced on `/health` as `embedder_last_ok_secs_ago` so monitoring
    /// can alert when the gap exceeds a threshold (e.g. > 5 min = stalled).
    /// What: reads `last_ok_unix_secs`, compares to now. Returns `None` when
    /// the stored value is 0 (never succeeded).
    /// Test: `stall_tracker_records_success_and_timeout`.
    pub fn last_ok_secs_ago(&self) -> Option<u64> {
        let stored = self.last_ok_unix_secs.load(Ordering::Relaxed);
        if stored == 0 {
            return None;
        }
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(stored);
        Some(now_secs.saturating_sub(stored))
    }

    /// Number of consecutive/recent embed timeouts since the last success.
    ///
    /// Why: the primary signal for `/health` to flip `embedder` to `"stalled"`.
    /// A non-zero count means the embedder returned zero usable results on the
    /// last N embed attempts — even if the sidecar process is alive.
    /// What: loads `recent_timeout_count` with `Relaxed` ordering.
    /// Test: `stall_tracker_records_success_and_timeout`.
    pub fn recent_timeout_count(&self) -> u32 {
        self.recent_timeout_count.load(Ordering::Relaxed)
    }
}

// Cheap clone so `SearchAppState` can be cloned (it holds `Arc<…>`).
// `EmbedderStallTracker` itself is always wrapped in `Arc`.
impl Clone for EmbedderStallTracker {
    fn clone(&self) -> Self {
        // Snapshot atomics for the copy — only used in tests.
        Self {
            last_ok_unix_secs: AtomicU64::new(self.last_ok_unix_secs.load(Ordering::Relaxed)),
            recent_timeout_count: AtomicU32::new(self.recent_timeout_count.load(Ordering::Relaxed)),
            total_ok_count: AtomicU64::new(self.total_ok_count.load(Ordering::Relaxed)),
            total_timeout_count: AtomicU64::new(self.total_timeout_count.load(Ordering::Relaxed)),
        }
    }
}

/// Convenience type alias used throughout the server module.
pub type StallTrackerArc = Arc<EmbedderStallTracker>;

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies that `record_timeout` increments counters and `record_success`
    /// resets `recent_timeout_count` while setting `last_ok_unix_secs`.
    ///
    /// Why: core correctness — if the reset does not happen, `/health` keeps
    /// showing `"stalled"` after the embedder recovers.
    /// What: sequence of record_timeout / record_success calls; assert counts.
    /// Test: this test.
    #[test]
    fn stall_tracker_records_success_and_timeout() {
        let t = EmbedderStallTracker::new();

        // Initially everything is zero.
        assert_eq!(t.recent_timeout_count(), 0);
        assert!(t.last_ok_secs_ago().is_none(), "never succeeded yet");

        // Two timeouts.
        t.record_timeout();
        t.record_timeout();
        assert_eq!(t.recent_timeout_count(), 2);
        assert!(t.last_ok_secs_ago().is_none(), "still no success");

        // One success resets the recent count.
        t.record_success();
        assert_eq!(t.recent_timeout_count(), 0, "reset on success");
        let ago = t.last_ok_secs_ago().expect("success should set timestamp");
        // In a unit test this runs fast — should be < 5s.
        assert!(ago < 5, "last_ok_secs_ago should be near-zero; got {ago}");
    }

    /// Verifies that `record_success` after multiple timeouts resets to 0.
    ///
    /// Why: guards the specific ANE stall self-heal scenario — after the stall
    /// clears and the first embed succeeds, `/health` should immediately return
    /// `"ready"` again.
    /// What: record 5 timeouts, then 1 success, assert count == 0.
    /// Test: this test.
    #[test]
    fn stall_tracker_reset_on_success() {
        let t = EmbedderStallTracker::new();
        for _ in 0..5 {
            t.record_timeout();
        }
        assert_eq!(t.recent_timeout_count(), 5);
        t.record_success();
        assert_eq!(t.recent_timeout_count(), 0, "must reset on first success");
    }

    /// Verifies that calling `record_timeout` never touches `last_ok_unix_secs`.
    ///
    /// Why: `last_ok_secs_ago` must remain `None` until a real success arrives;
    /// a stalled sidecar should not falsely claim "last ok 0 seconds ago".
    /// What: record only timeouts; assert `last_ok_secs_ago` returns `None`.
    /// Test: this test.
    #[test]
    fn stall_tracker_timeout_does_not_set_ok_timestamp() {
        let t = EmbedderStallTracker::new();
        t.record_timeout();
        t.record_timeout();
        assert!(
            t.last_ok_secs_ago().is_none(),
            "timeout must not set the ok timestamp"
        );
    }
}
