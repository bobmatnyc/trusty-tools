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

/// The configured bounds the wedge line is derived from (#4001, #9783).
///
/// Why: each is a process-wide env read, so a test cannot vary them without
/// racing its siblings; naming them in one struct lets the derivation stay a
/// pure function of its inputs.
/// What: the two wait bounds (#3992, #4001) and the two write bounds (#4002,
/// #6366).
/// Test: `wedge_line_is_the_write_span_plus_the_wait_line`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WedgeBounds {
    /// `open_queue_timeout()`: a palace open waits this long at most.
    pub open_queue: Duration,
    /// `write_lock_timeout()`: one write-mutex wait lasts this long at most.
    pub write_lock: Duration,
    /// `write_op_budget()`: a write's acquisition legs share this budget.
    pub write_op_budget: Duration,
    /// `write_pipeline_timeout()`: a write holds the palace write lock across
    /// embed, upsert and persist for this long at most.
    pub write_pipeline: Duration,
}

impl WedgeBounds {
    /// The env-configured wait bounds, with the two write bounds the caller
    /// enforces.
    pub(crate) fn configured(write_op_budget: Duration, write_pipeline: Duration) -> Self {
        Self {
            open_queue: crate::engine::timeouts::open_queue_timeout(),
            write_lock: crate::engine::timeouts::write_lock_timeout(),
            write_op_budget,
            write_pipeline,
        }
    }
}

/// How long the oldest in-flight operation, or a held palace handle lock, may
/// last before `/health` calls it wedged.
///
/// Why: this must sit ABOVE every legitimately-bounded operation, or healthy
/// load reads as a wedge. #9783: a `memory_remember` holds the palace write
/// lock across embed, upsert and persist for up to `write_pipeline_timeout()`
/// (240 s), so the old `2 × max(open_queue, write_lock)` line (120 s) called a
/// progressing write wedged. Env-overridable so an operator can pick the line.
/// What: `TRUSTY_WEDGE_THRESHOLD_SECS` if set and parseable, else
/// [`derive_wedge_threshold`] over the env-configured bounds (420 s at the
/// defaults).
/// Test: `wedge_threshold_exceeds_the_open_queue_bound`,
/// `a_legal_write_span_does_not_cross_the_default_wedge_line`.
pub fn wedge_threshold() -> Duration {
    wedge_threshold_for(
        crate::engine::timeouts::write_op_budget(),
        crate::engine::timeouts::write_pipeline_timeout(),
    )
}

/// [`wedge_threshold`] for a daemon enforcing the given write bounds (#9783).
///
/// Why: `AppState` carries its own write budget and pipeline ceiling, and the
/// line must follow the bounds the daemon actually enforces.
/// What: the env override when set, else [`derive_wedge_threshold`] over
/// [`WedgeBounds::configured`].
/// Test: `the_wedge_line_follows_the_configured_write_budgets`.
pub(crate) fn wedge_threshold_for(write_op_budget: Duration, write_pipeline: Duration) -> Duration {
    let override_secs = std::env::var("TRUSTY_WEDGE_THRESHOLD_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok());
    derive_wedge_threshold(
        override_secs,
        &WedgeBounds::configured(write_op_budget, write_pipeline),
    )
}

/// The wedge line for `bounds`, or the operator's override (#4001, #9783).
///
/// Why: a write's whole legal span is its acquisition budget plus its
/// pipeline ceiling. The margin on top is the pre-#9783 wait line,
/// `2 × max(open_queue, write_lock)`: it covers the handle write-mutex wait
/// inside the pipeline call, which draws on no budget, and a palace open
/// outside any write, so no wait that sat inside the old line can cross the new
/// one. A line below the span calls a progressing write wedged; one with no
/// finite bound would hide the #3992 shape (a remember stuck ~1800 s).
/// What: the override in seconds when present, else
/// `write_op_budget + write_pipeline + 2 × max(open_queue, write_lock)`,
/// saturating so an absurd bound cannot panic the daemon at construction.
/// Zero inputs never disable detection: the env path floors the pipeline
/// ceiling ([`crate::engine::timeouts::floor_write_pipeline`]), and an
/// all-zero input yields a zero line, under which `/health` reports any held
/// lock wedged rather than none.
/// Test: `wedge_line_is_the_write_span_plus_the_wait_line`,
/// `a_zero_or_absurd_bound_never_disables_wedge_detection`.
pub(crate) fn derive_wedge_threshold(override_secs: Option<u64>, bounds: &WedgeBounds) -> Duration {
    if let Some(secs) = override_secs {
        return Duration::from_secs(secs);
    }
    // #9783: the write span (budget + pipeline) plus the pre-#9783 wait line.
    let wait_line = bounds.open_queue.max(bounds.write_lock).saturating_mul(2);
    bounds
        .write_op_budget
        .saturating_add(bounds.write_pipeline)
        .saturating_add(wait_line)
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
