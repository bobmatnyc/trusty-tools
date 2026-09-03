//! Bounded in-memory history for host-metric samples (#6641).
//!
//! Why: the console's real-time graphs need the last 10 minutes of host
//!      samples, not just the newest one. A ring lives here rather than in
//!      trusty-console because the buffer has two writers by design: the
//!      console's own sampler today, and the services that will PUSH their
//!      samples once the #6284 transport inversion lands. Per the workspace
//!      "common entry point" rule both writers must enter through the same
//!      function, so that function is defined once, here.
//! What: [`MetricRing`] is a fixed-capacity FIFO — [`MetricRing::push`] is the
//!      ONE write path; when the ring is full it evicts the oldest item before
//!      appending, so capacity is never exceeded and insertion order is
//!      preserved. [`HOST_HISTORY_CAPACITY`] and [`HOST_SAMPLE_INTERVAL_SECS`]
//!      pin the owner's 10-minute window (600 points at 1 s). Nothing here
//!      persists: the ring is in-memory only and a restart starts an empty
//!      window by design (owner ruling, epic #6516 Phase 3).
//!
//! #6642: the cadence moved from 5 s to 1 s and the capacity from 120 to 600.
//! The window is the same 10 minutes; each home-page card now draws one bar per
//! second, which a 5 s sample cannot render.
//! Test: the inline `tests` module — `push_evicts_oldest_at_capacity`,
//!      `push_preserves_insertion_order`, `empty_ring_reads_empty`,
//!      `host_window_is_the_owner_ruling`, `capacity_of_zero_is_clamped_to_one`.

use std::collections::VecDeque;

use super::HostMetrics;

/// Points retained per metric ring — 600 (#6641; raised from 120 by #6642).
///
/// Why: the owner's ruling is a 10-minute window. #6641 sampled it every 5 s,
///      which is 120 points; #6642 moved the cadence to 1 s, so the same ten
///      minutes is 600 points. Naming it once keeps the console, the history
///      endpoint, and the UI from each hard-coding their own number.
/// What: `600`.
/// Test: `host_window_is_the_owner_ruling`.
pub const HOST_HISTORY_CAPACITY: usize = 600;

/// Seconds between host samples — 1 (#6641; lowered from 5 by #6642).
///
/// Why: the other half of the owner's window ruling; the console's sampler
///      default and the `sample_interval_secs` the history payload advertises
///      both read it from here, so the graph's x-axis cannot disagree with the
///      producer. #6642 set it to 1 because each home-page card draws one bar
///      per second.
/// What: `1`.
/// Test: `host_window_is_the_owner_ruling`.
pub const HOST_SAMPLE_INTERVAL_SECS: u64 = 1;

/// A fixed-capacity FIFO of metric samples, oldest first (#6641).
///
/// Why: an unbounded `Vec` of samples grows without limit in a daemon that runs
///      for weeks. A ring caps memory at `capacity` items and drops the oldest
///      sample when a new one arrives, which is exactly the shape a
///      sliding-window graph reads.
/// What: wraps a `VecDeque<T>` whose length never exceeds `capacity`.
///      [`MetricRing::push`] is the single mutation point — every producer
///      (today's sampler, tomorrow's pushed sample from #6284) calls it, so the
///      eviction rule cannot diverge between them. Iteration and
///      [`MetricRing::snapshot`] yield oldest → newest.
/// Test: `push_evicts_oldest_at_capacity`, `push_preserves_insertion_order`,
///      `empty_ring_reads_empty`.
#[derive(Debug, Clone)]
pub struct MetricRing<T> {
    capacity: usize,
    items: VecDeque<T>,
}

impl<T> MetricRing<T> {
    /// Build an empty ring holding at most `capacity` items.
    ///
    /// Why: capacity is a construction-time decision (the owner's 120 for the
    ///      host window, a smaller number in tests), never a per-push argument.
    /// What: allocates a `VecDeque` with room for `capacity`. A `capacity` of
    ///      `0` is clamped to `1` — a ring that can hold nothing would silently
    ///      discard every sample, which the caller would never see, so the type
    ///      refuses to be useless instead.
    /// Test: `capacity_of_zero_is_clamped_to_one`.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            capacity,
            items: VecDeque::with_capacity(capacity),
        }
    }

    /// Append `item`, evicting the oldest when the ring is already full.
    ///
    /// Why: THE write path. Both the console's sampler and a future pushed
    ///      sample (#6284) enter here, so a pushed sample and a sampled one are
    ///      indistinguishable to every reader.
    /// What: when `len == capacity`, pops the front (oldest) before pushing the
    ///      new item at the back. Length therefore never exceeds `capacity`, and
    ///      insertion order is preserved.
    /// Test: `push_evicts_oldest_at_capacity`, `push_preserves_insertion_order`.
    pub fn push(&mut self, item: T) {
        if self.items.len() == self.capacity {
            self.items.pop_front();
        }
        self.items.push_back(item);
    }

    /// Items currently held, oldest first.
    ///
    /// Test: `push_preserves_insertion_order`.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &T> {
        self.items.iter()
    }

    /// The newest item, or `None` on an empty ring.
    ///
    /// Why (#6642): a caller that wants only the latest value — the console's
    /// service list renders one number per service before its stream connects —
    /// would otherwise call [`MetricRing::snapshot`] and take the last element,
    /// cloning the whole 600-point window to read one point.
    /// What: the back of the deque, borrowed.
    /// Test: `last_reads_the_newest_item`.
    #[must_use]
    pub fn last(&self) -> Option<&T> {
        self.items.back()
    }

    /// Number of items currently held (never above [`MetricRing::capacity`]).
    ///
    /// Test: `push_evicts_oldest_at_capacity`.
    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// `true` before the first push.
    ///
    /// Test: `empty_ring_reads_empty`.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The configured maximum item count.
    ///
    /// Test: `capacity_of_zero_is_clamped_to_one`.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity
    }
}

impl<T: Clone> MetricRing<T> {
    /// Copy the ring out as a `Vec`, oldest first.
    ///
    /// Why: the history endpoint and the SSE `history` event both serialise the
    ///      window as a JSON array, and neither may hold the console's lock
    ///      while doing it.
    /// What: clones every item in iteration order.
    /// Test: `push_preserves_insertion_order`, `empty_ring_reads_empty`.
    #[must_use]
    pub fn snapshot(&self) -> Vec<T> {
        self.items.iter().cloned().collect()
    }
}

impl MetricRing<HostMetrics> {
    /// A host-metrics ring sized to the owner's 10-minute window (#6641).
    ///
    /// Why: the console should not restate `600`; the window is one ruling and
    ///      belongs to one constant.
    /// What: `MetricRing::new(HOST_HISTORY_CAPACITY)`.
    /// Test: `host_window_is_the_owner_ruling`.
    #[must_use]
    pub fn host_window() -> Self {
        Self::new(HOST_HISTORY_CAPACITY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the eviction rule is the whole point of a bounded ring — a ring that
    ///      grew past its capacity would leak in a long-lived daemon.
    /// What: pushes `capacity + 1` items into a 600-slot ring and asserts the
    ///      length stopped at 600, the oldest item is gone, and the newest is
    ///      present.
    /// Test: this test.
    #[test]
    fn push_evicts_oldest_at_capacity() {
        let mut ring: MetricRing<u32> = MetricRing::new(HOST_HISTORY_CAPACITY);
        for i in 0..HOST_HISTORY_CAPACITY as u32 {
            ring.push(i);
        }
        assert_eq!(ring.len(), HOST_HISTORY_CAPACITY);
        assert_eq!(ring.snapshot().first().copied(), Some(0));

        // #6642: the 601st push must evict sample 0 and pin the length.
        ring.push(HOST_HISTORY_CAPACITY as u32);
        assert_eq!(
            ring.len(),
            HOST_HISTORY_CAPACITY,
            "capacity must never be exceeded"
        );
        let items = ring.snapshot();
        assert_eq!(items.first().copied(), Some(1), "oldest sample was evicted");
        assert_eq!(
            items.last().copied(),
            Some(HOST_HISTORY_CAPACITY as u32),
            "newest sample is at the back"
        );
    }

    /// Why: a graph plotted from a ring that reordered its points would be
    ///      wrong in a way no type check catches.
    /// What: pushes a known sequence through a wrap-around and asserts the
    ///      snapshot and the iterator both read oldest → newest.
    /// Test: this test.
    #[test]
    fn push_preserves_insertion_order() {
        let mut ring: MetricRing<u32> = MetricRing::new(3);
        for i in 1..=5 {
            ring.push(i);
        }
        assert_eq!(ring.snapshot(), vec![3, 4, 5]);
        assert_eq!(ring.iter().copied().collect::<Vec<_>>(), vec![3, 4, 5]);
    }

    /// Why (#6642): the console's service list renders the newest point per
    ///      service. Reading it through `snapshot().last()` would clone the
    ///      whole 600-point window per service per request.
    /// What: asserts `last` follows the newest push, survives an eviction, and
    ///      is `None` before the first push.
    /// Test: this test.
    #[test]
    fn last_reads_the_newest_item() {
        let mut ring: MetricRing<u32> = MetricRing::new(2);
        assert_eq!(ring.last(), None, "an empty ring has no newest item");
        ring.push(1);
        assert_eq!(ring.last().copied(), Some(1));
        ring.push(2);
        ring.push(3);
        assert_eq!(ring.snapshot(), vec![2, 3], "1 was evicted");
        assert_eq!(ring.last().copied(), Some(3));
    }

    /// Why: the history endpoint must answer `200` with an empty window before
    /// the first sample, so an empty ring has to read as empty rather than as an
    /// error or a panic.
    /// What: asserts `is_empty`, `len == 0`, and an empty snapshot on a fresh
    /// ring.
    /// Test: this test.
    #[test]
    fn empty_ring_reads_empty() {
        let ring: MetricRing<u32> = MetricRing::new(HOST_HISTORY_CAPACITY);
        assert!(ring.is_empty());
        assert_eq!(ring.len(), 0);
        assert!(ring.snapshot().is_empty());
        assert_eq!(ring.iter().count(), 0);
        assert_eq!(ring.capacity(), HOST_HISTORY_CAPACITY);
    }

    /// Why: the 10-minute window is an owner ruling; if the constants drift the
    ///      graph silently covers a different span than the UI labels claim.
    /// What: pins 600 points × 1 s = 600 s and asserts the host-window
    ///      constructor uses the constant. #6642 moved the pair from 120 × 5 to
    ///      600 × 1; the PRODUCT is what the UI's x-axis depends on, so it is
    ///      asserted separately from the two operands.
    /// Test: this test.
    #[test]
    fn host_window_is_the_owner_ruling() {
        assert_eq!(HOST_HISTORY_CAPACITY, 600);
        assert_eq!(HOST_SAMPLE_INTERVAL_SECS, 1);
        assert_eq!(
            HOST_HISTORY_CAPACITY as u64 * HOST_SAMPLE_INTERVAL_SECS,
            600,
            "600 points at 1s is the owner's 10-minute window"
        );
        let ring = MetricRing::<HostMetrics>::host_window();
        assert_eq!(ring.capacity(), HOST_HISTORY_CAPACITY);
        assert!(ring.is_empty());
    }

    /// Why: a zero-capacity ring would drop every sample and report an empty
    ///      graph forever, with nothing to distinguish it from "no data yet".
    /// What: builds a ring with capacity 0 and asserts it holds one item.
    /// Test: this test.
    #[test]
    fn capacity_of_zero_is_clamped_to_one() {
        let mut ring: MetricRing<u32> = MetricRing::new(0);
        assert_eq!(ring.capacity(), 1);
        ring.push(7);
        ring.push(8);
        assert_eq!(ring.snapshot(), vec![8]);
    }
}
