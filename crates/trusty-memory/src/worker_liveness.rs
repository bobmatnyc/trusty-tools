//! Live worker-occupancy gauge for the palace open/write path (issue #4001).
//!
//! Why: during the #3992 incident six daemon threads were parked in
//! `concurrent_open::backoff_sleep_ms` with a `memory_remember` hung ~1800 s,
//! and BOTH `tm doctor` and `trusty-memory doctor` reported HEALTHY the whole
//! time. Doctor observed only *process* liveness — an HTTP listener that still
//! answers, and lock files that still look clean — neither of which can see a
//! wedged worker pool. This module supplies the missing observation: how long
//! the oldest in-flight palace operation has been running. That is the cheapest
//! signal that actually distinguishes "the process is up" from "work is moving".
//!
//! What: a fixed-size, lock-free slot table of operation start timestamps. An
//! operation claims a slot on entry and releases it on drop; the probe reads
//! the table and reports the age of the oldest occupied slot. No allocation, no
//! mutex, and no syscall on the hot path — one CAS in and one store out — so
//! the gauge can never itself become the load problem it exists to detect.
//! Test: see `worker_liveness_tests.rs`.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// Number of concurrently-trackable operations.
///
/// Why: the table is scanned linearly on both claim and probe, so it must stay
/// small enough that a scan is trivial (64 relaxed atomic loads is ~nothing)
/// while comfortably exceeding realistic in-flight concurrency for a
/// single-user memory daemon. Operations beyond this count are still *counted*
/// via the overflow gauge — they simply do not contribute an age sample, which
/// degrades the signal gracefully instead of blocking or allocating.
/// Test: `overflow_is_counted_when_slots_exhausted`.
const SLOTS: usize = 64;

/// Sentinel meaning "this slot is free". Real timestamps are offsets from the
/// tracker's epoch and are stored as `millis + 1`, so 0 is never a valid entry.
const FREE: u64 = 0;

/// How long the oldest in-flight operation may run before the pool is called
/// wedged.
///
/// Why: this must sit ABOVE every legitimately-bounded wait, or healthy load
/// would read as a wedge. The longest such bound on the palace open path is
/// `memory_core::timeouts::open_queue_timeout()` (default 60 s, issue #3992),
/// after which `open_palace` gives up and returns an error. Doubling it means
/// any operation still outstanding has already blown through the bound that
/// was supposed to release it — which is precisely the #3992 signature, where
/// a `memory_remember` ran ~1800 s. Env-overridable so an operator running a
/// deliberately long `open_queue_timeout` can move the wedge line with it.
/// What: `TRUSTY_WEDGE_THRESHOLD_SECS` if set and parseable, else
/// `2 × open_queue_timeout()`.
/// Test: `wedge_threshold_exceeds_the_open_queue_bound`.
pub fn wedge_threshold() -> Duration {
    if let Some(secs) = std::env::var("TRUSTY_WEDGE_THRESHOLD_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
    {
        return Duration::from_secs(secs);
    }
    trusty_common::memory_core::timeouts::open_queue_timeout() * 2
}

/// Tracks how long the oldest in-flight palace operation has been running.
///
/// Why (issue #4001): see the module docs — this is the signal that would have
/// revealed the #3992 wedge. Alternatives considered and rejected: sampling
/// thread state (needs platform-specific debugging APIs and is expensive),
/// mutex wait-time histograms (needs instrumenting `parking_lot` internals),
/// and a plain in-flight *count* (a count alone cannot distinguish healthy
/// concurrency from a wedge — only the *age* of outstanding work can).
/// What: a slot table of start timestamps plus an overflow counter. Cloneable
/// and `Send`/`Sync` via the caller's `Arc`.
/// Test: `worker_liveness_tests.rs`.
#[derive(Debug)]
pub struct WorkerLiveness {
    /// Start timestamps as `epoch.elapsed().as_millis() + 1`; [`FREE`] when
    /// the slot is unoccupied.
    slots: [AtomicU64; SLOTS],
    /// Operations in flight that could not claim a slot. Contributes to the
    /// in-flight count but not to the oldest-age sample.
    overflow: AtomicUsize,
    /// Reference point for every stored timestamp. Using a monotonic `Instant`
    /// rather than wall-clock time keeps the gauge immune to clock steps.
    epoch: Instant,
}

impl Default for WorkerLiveness {
    fn default() -> Self {
        Self::new()
    }
}

impl WorkerLiveness {
    /// Create an empty tracker.
    ///
    /// Why: `AtomicU64` is not `Copy`, so the slot array cannot be built with
    /// `[AtomicU64::new(0); SLOTS]`; `from_fn` is the idiomatic construction.
    /// What: all slots [`FREE`], overflow zero, epoch = now.
    /// Test: `idle_tracker_reports_no_work`.
    pub fn new() -> Self {
        Self {
            slots: std::array::from_fn(|_| AtomicU64::new(FREE)),
            overflow: AtomicUsize::new(0),
            epoch: Instant::now(),
        }
    }

    /// Register the start of an operation, returning a guard that unregisters
    /// it on drop.
    ///
    /// Why: the release MUST be drop-driven rather than an explicit call. The
    /// operations being tracked are exactly the ones that fail, time out, and
    /// `?`-propagate mid-function; an explicit `finish()` would be skipped on
    /// every error path and leak slots, which would then be misread as a
    /// permanent wedge — turning this fix into a new false positive.
    /// What: claims the first [`FREE`] slot via a relaxed CAS, storing the
    /// current offset from `epoch`. Falls back to the overflow counter when
    /// every slot is taken.
    /// Test: `guard_releases_slot_on_drop`, `guard_releases_slot_on_panic`.
    pub fn track(&self) -> WorkGuard<'_> {
        // `+ 1` keeps 0 reserved as the FREE sentinel, so an operation
        // starting in the tracker's first millisecond is still distinguishable
        // from an empty slot.
        let now = self.epoch.elapsed().as_millis() as u64 + 1;
        for (idx, slot) in self.slots.iter().enumerate() {
            if slot
                .compare_exchange(FREE, now, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                return WorkGuard {
                    tracker: self,
                    slot: Some(idx),
                };
            }
        }
        self.overflow.fetch_add(1, Ordering::AcqRel);
        WorkGuard {
            tracker: self,
            slot: None,
        }
    }

    /// Number of operations currently in flight.
    ///
    /// What: occupied slots plus the overflow count.
    /// Test: `in_flight_counts_active_operations`.
    pub fn in_flight(&self) -> usize {
        let occupied = self
            .slots
            .iter()
            .filter(|s| s.load(Ordering::Acquire) != FREE)
            .count();
        occupied + self.overflow.load(Ordering::Acquire)
    }

    /// Age of the oldest in-flight operation, or `None` when idle.
    ///
    /// Why: this is the actual health signal. A daemon with zero in-flight work
    /// is trivially not wedged; a daemon whose oldest operation has been
    /// running for minutes is wedged regardless of what the HTTP listener says.
    /// What: scans for the smallest occupied timestamp and returns the elapsed
    /// duration since it. Returns `None` when every slot is free — note that
    /// overflow-only operations produce `None` here by design, since no age
    /// sample exists for them.
    /// Test: `oldest_age_tracks_the_earliest_operation`.
    pub fn oldest_age(&self) -> Option<Duration> {
        let oldest = self
            .slots
            .iter()
            .map(|s| s.load(Ordering::Acquire))
            .filter(|&v| v != FREE)
            .min()?;
        let now = self.epoch.elapsed().as_millis() as u64 + 1;
        Some(Duration::from_millis(now.saturating_sub(oldest)))
    }

    /// True when the oldest in-flight operation has exceeded `threshold`.
    ///
    /// Why: turns the raw age into the verdict doctor reports. Keeping the
    /// threshold a parameter (rather than a constant baked in here) lets the
    /// caller pick a bound derived from the real operation timeout, and lets
    /// tests drive the wedge condition deterministically without sleeping.
    /// What: `oldest_age() > threshold`; `false` when idle.
    /// Test: `wedged_when_oldest_exceeds_threshold`.
    pub fn is_wedged(&self, threshold: Duration) -> bool {
        self.oldest_age().is_some_and(|age| age > threshold)
    }
}

/// RAII registration for one in-flight operation.
///
/// Why: see [`WorkerLiveness::track`] — drop-driven release is what makes the
/// gauge correct across the `?` and panic paths that a wedge actually travels.
/// What: releases its slot (or decrements overflow) on drop.
/// Test: `guard_releases_slot_on_drop`, `guard_releases_slot_on_panic`.
#[derive(Debug)]
pub struct WorkGuard<'a> {
    tracker: &'a WorkerLiveness,
    /// `Some(idx)` when a slot was claimed, `None` when the operation landed
    /// in the overflow bucket.
    slot: Option<usize>,
}

impl Drop for WorkGuard<'_> {
    fn drop(&mut self) {
        match self.slot {
            Some(idx) => {
                self.tracker.slots[idx].store(FREE, Ordering::Release);
            }
            None => {
                self.tracker.overflow.fetch_sub(1, Ordering::AcqRel);
            }
        }
    }
}

#[cfg(test)]
#[path = "worker_liveness_tests.rs"]
mod tests;
