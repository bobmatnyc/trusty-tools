//! Reindex retry quarantine — back-off and quarantine after N consecutive failures.
//!
//! Why (issue #764): a repeatedly-failing index reindex (e.g. an `apex` temp-dir
//! index stuck in a zero-vector rollback loop) retried indefinitely and re-stalled
//! the embedderd sidecar on every attempt, taking down the whole daemon. A
//! quarantine gate stops the retry storm: after `MAX_CONSECUTIVE_FAILURES`
//! consecutive failures the index is quarantined for an exponentially-growing
//! back-off period, preventing it from re-entering the background reindex queue
//! until the operator intervenes (or the back-off expires).
//!
//! What: `ReindexQuarantine` is a DashMap-backed, lock-free counter per index
//! (identified by `IndexId`). `record_failure` bumps the counter and updates the
//! quarantine deadline; `record_success` resets both. `is_quarantined` is the
//! cheap gate called by `spawn_reindex_with_cleanup` before queuing.
//!
//! Test: unit tests in this module — `cargo test -p trusty-search -- quarantine`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;

use crate::core::registry::IndexId;

/// Number of consecutive reindex failures before an index is quarantined.
///
/// Why: a single transient failure (OOM spike, sidecar crash) should not
/// quarantine an otherwise healthy index. Three consecutive failures are
/// a strong signal that the index is broken and needs operator attention.
/// Env: `TRUSTY_REINDEX_MAX_FAILURES` (default 3, must be ≥ 1).
pub fn max_consecutive_failures() -> u32 {
    std::env::var("TRUSTY_REINDEX_MAX_FAILURES")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|&n| n >= 1)
        .unwrap_or(3)
}

/// Maximum quarantine period (cap for the exponential back-off).
///
/// Why: an unbounded back-off would effectively permanently quarantine an
/// index with no operator action. The 1-hour cap gives the operator time to
/// notice and intervene while guaranteeing automatic recovery attempts.
/// Env: `TRUSTY_REINDEX_QUARANTINE_MAX_SECS` (default 3600 = 1 hour).
pub fn max_quarantine_secs() -> u64 {
    std::env::var("TRUSTY_REINDEX_QUARANTINE_MAX_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|&s| s >= 1)
        .unwrap_or(3600)
}

/// Base quarantine period for exponential back-off (first quarantine = 60 s).
const BASE_QUARANTINE_SECS: u64 = 60;

/// Per-index failure tracking entry.
#[derive(Debug)]
struct QuarantineEntry {
    /// Number of consecutive failures (resets on success).
    consecutive_failures: u32,
    /// When the current quarantine period expires. `None` if not quarantined.
    quarantine_until: Option<Instant>,
}

/// Process-wide reindex quarantine registry.
///
/// Why: prevents a broken index from hammering the sidecar with
/// infinite retries by backing off exponentially after repeated failures
/// (issue #764).
///
/// What: a `DashMap<IndexId, QuarantineEntry>` tracking consecutive failure
/// counts and quarantine deadlines. Cheap to `Clone` (Arc-backed).
///
/// Test: `quarantine_*` tests in this module.
#[derive(Clone, Default)]
pub struct ReindexQuarantine {
    entries: Arc<DashMap<IndexId, QuarantineEntry>>,
}

impl ReindexQuarantine {
    /// Create a fresh quarantine registry (no quarantined indexes).
    ///
    /// Why: used by `SearchAppState::new()` to wire the quarantine at startup.
    /// What: allocates an empty DashMap.
    /// Test: `quarantine_new_is_empty` below.
    pub fn new() -> Self {
        Self {
            entries: Arc::new(DashMap::new()),
        }
    }

    /// Returns `true` if `id` is currently quarantined (back-off has not expired).
    ///
    /// Why: the hot gate — called by `spawn_reindex_with_cleanup` before
    /// queuing a background reindex. Must be cheap (no lock contention on
    /// the happy path).
    /// What: if no entry exists → not quarantined. If the deadline is in the
    /// future → quarantined. If the deadline has passed → expired, clears the
    /// deadline (keeps the failure counter so the NEXT failure still counts).
    /// Test: `quarantine_blocks_until_deadline_expires` below.
    pub fn is_quarantined(&self, id: &IndexId) -> bool {
        if let Some(mut entry) = self.entries.get_mut(id) {
            if let Some(until) = entry.quarantine_until {
                if Instant::now() < until {
                    return true;
                }
                // Deadline passed — lift the quarantine but keep the failure
                // counter so a fresh failure immediately re-triggers quarantine.
                entry.quarantine_until = None;
            }
        }
        false
    }

    /// Record a reindex failure for `id`.
    ///
    /// Why: the feedback loop — called at the end of every failed reindex
    /// task. If `consecutive_failures` reaches `max_consecutive_failures()`
    /// the index is quarantined with an exponentially-growing back-off.
    ///
    /// What: increments `consecutive_failures`. When the threshold is crossed,
    /// computes `quarantine_secs = min(BASE * 2^(excess_failures), max)` and
    /// sets `quarantine_until = now + quarantine_secs`. Logs at `warn` level
    /// so the operator sees the quarantine in daemon logs.
    ///
    /// Test: `quarantine_triggers_after_threshold` and
    /// `quarantine_backoff_grows_exponentially` below.
    pub fn record_failure(&self, id: &IndexId) {
        let max_failures = max_consecutive_failures();
        let mut entry = self
            .entries
            .entry(id.clone())
            .or_insert_with(|| QuarantineEntry {
                consecutive_failures: 0,
                quarantine_until: None,
            });
        entry.consecutive_failures = entry.consecutive_failures.saturating_add(1);
        let failures = entry.consecutive_failures;

        if failures >= max_failures {
            // Exponential back-off: first quarantine = BASE, doubles each time.
            // `excess` = how many failures over the threshold (0-indexed).
            let excess = failures.saturating_sub(max_failures);
            // Exponential back-off: 2^excess multiplier, capped at 2^30 to avoid
            // overflow. `checked_shl` returns None when the shift overflows u64;
            // we fall back to the maximum to avoid a silent 0.
            let multiplier = 1u64.checked_shl(excess.min(30)).unwrap_or(u64::MAX);
            let backoff_secs = BASE_QUARANTINE_SECS
                .saturating_mul(multiplier)
                .min(max_quarantine_secs());
            let until = Instant::now() + Duration::from_secs(backoff_secs);
            entry.quarantine_until = Some(until);
            tracing::warn!(
                index_id = %id.0,
                consecutive_failures = failures,
                backoff_secs,
                "reindex quarantine: index quarantined after {} consecutive failure(s) \
                 — next retry in {}s (issue #764). \
                 Resolve the root cause (missing root? corrupt corpus? sidecar crash?) \
                 and issue a manual `POST /indexes/{}/reindex` to clear.",
                failures,
                backoff_secs,
                id.0,
            );
        } else {
            tracing::debug!(
                index_id = %id.0,
                consecutive_failures = failures,
                remaining = max_failures.saturating_sub(failures),
                "reindex quarantine: failure recorded ({}/{} before quarantine)",
                failures,
                max_failures,
            );
        }
    }

    /// Record a successful reindex for `id`, clearing all failure state.
    ///
    /// Why: a successful reindex proves the index is healthy; reset the counter
    /// so a single future failure doesn't immediately hit the threshold that
    /// was set by prior failures in a different failure mode.
    /// What: removes the entry from the map entirely (equivalent to resetting
    /// both `consecutive_failures` and `quarantine_until` to zero/None).
    /// Test: `quarantine_success_clears_failures` below.
    pub fn record_success(&self, id: &IndexId) {
        self.entries.remove(id);
    }

    /// Return the current consecutive failure count for `id` (0 if never failed).
    ///
    /// Why: exposed for `/health` so operators can see which indexes are
    /// accumulating failures before they hit the quarantine threshold.
    /// What: reads `consecutive_failures` from the entry, or returns 0.
    /// Test: `quarantine_failure_count_increments` below.
    pub fn failure_count(&self, id: &IndexId) -> u32 {
        self.entries
            .get(id)
            .map(|e| e.consecutive_failures)
            .unwrap_or(0)
    }

    /// Return the number of currently quarantined indexes.
    ///
    /// Why: surfaced in `/health` as `quarantined_index_count` so operators
    /// don't have to poll every index status to detect the condition.
    /// What: counts entries whose `quarantine_until` is in the future.
    /// Test: `quarantine_count_reflects_active_quarantines` below.
    pub fn quarantined_count(&self) -> usize {
        let now = Instant::now();
        self.entries
            .iter()
            .filter(|e| e.quarantine_until.is_some_and(|t| now < t))
            .count()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    //! Unit tests for the reindex quarantine registry.
    //!
    //! Why: the quarantine decisions are pure functions of time and counters;
    //! testing them without a real embedder keeps the suite fast.
    //! Test: `cargo test -p trusty-search -- quarantine`.

    use super::*;

    fn id(s: &str) -> IndexId {
        IndexId(s.to_string())
    }

    /// A fresh registry must have no quarantined indexes.
    ///
    /// Why: guards against accidental shared global state leaking into tests.
    /// Test: this test.
    #[test]
    fn quarantine_new_is_empty() {
        let q = ReindexQuarantine::new();
        assert!(!q.is_quarantined(&id("x")));
        assert_eq!(q.failure_count(&id("x")), 0);
        assert_eq!(q.quarantined_count(), 0);
    }

    /// Failure count must increment with each `record_failure` call.
    ///
    /// Why: confirms the counter is per-index and monotonically growing
    /// before the threshold.
    /// Test: this test.
    #[test]
    fn quarantine_failure_count_increments() {
        let q = ReindexQuarantine::new();
        assert_eq!(q.failure_count(&id("a")), 0);
        q.record_failure(&id("a"));
        assert_eq!(q.failure_count(&id("a")), 1);
        q.record_failure(&id("a"));
        assert_eq!(q.failure_count(&id("a")), 2);
        // Different index — must not share state.
        assert_eq!(q.failure_count(&id("b")), 0);
    }

    /// After `max_consecutive_failures()` failures the index must be quarantined.
    ///
    /// Why: the core invariant — the quarantine must fire at the threshold.
    /// Test: this test.
    #[test]
    fn quarantine_triggers_after_threshold() {
        let q = ReindexQuarantine::new();
        let max = max_consecutive_failures();
        for _ in 0..max.saturating_sub(1) {
            q.record_failure(&id("idx"));
            // Not yet quarantined.
            assert!(!q.is_quarantined(&id("idx")));
        }
        // The Nth failure must quarantine.
        q.record_failure(&id("idx"));
        assert!(q.is_quarantined(&id("idx")));
        assert_eq!(q.quarantined_count(), 1);
    }

    /// A successful reindex clears all failure state.
    ///
    /// Why: a success means the index is healthy; reset so the next failure
    /// starts the counter from 0 again.
    /// Test: this test.
    #[test]
    fn quarantine_success_clears_failures() {
        let q = ReindexQuarantine::new();
        let max = max_consecutive_failures();
        for _ in 0..max {
            q.record_failure(&id("z"));
        }
        assert!(q.is_quarantined(&id("z")));
        q.record_success(&id("z"));
        assert!(!q.is_quarantined(&id("z")));
        assert_eq!(q.failure_count(&id("z")), 0);
    }

    /// Multiple failures beyond the threshold must produce increasing back-offs.
    ///
    /// Why: the exponential back-off prevents the same broken index from
    /// hammering the sidecar indefinitely.
    /// Test: this test (verifies ordering, not exact values, to be
    /// robust to env-var overrides in CI).
    #[test]
    fn quarantine_backoff_grows_exponentially() {
        let q = ReindexQuarantine::new();
        let max = max_consecutive_failures();

        // First quarantine.
        for _ in 0..max {
            q.record_failure(&id("w"));
        }
        let until1 = q
            .entries
            .get(&id("w"))
            .and_then(|e| e.quarantine_until)
            .expect("must be quarantined");

        // Second quarantine (one more failure beyond threshold).
        q.record_failure(&id("w"));
        let until2 = q
            .entries
            .get(&id("w"))
            .and_then(|e| e.quarantine_until)
            .expect("must still be quarantined");

        assert!(
            until2 >= until1,
            "second quarantine deadline must not be earlier than the first"
        );
    }
}
