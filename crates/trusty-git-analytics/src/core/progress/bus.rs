//! The bounded, drop-oldest progress bus.
//!
//! Why: `tga collect` / `tga classify` run for minutes against large corpora,
//! and #5197's TUI needs a live view of what they are doing. The producer is
//! the pipeline, whose throughput must not depend on whether anybody is
//! watching — so delivery is non-blocking and lossy by construction.
//! What: [`ProgressBus`], a cheap clonable handle over a shared ring buffer.
//! An inactive bus ([`ProgressBus::disabled`], also `Default`) drops every
//! emit on the floor, which is what every existing CLI path passes so its
//! behavior — including the current `indicatif` bars — is unchanged.
//! Test: `super::tests` covers the no-subscriber path, the drop-oldest
//! overflow policy, the dropped counter, and drain ordering.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use super::event::ProgressEvent;

/// Default ring-buffer capacity for [`ProgressBus::bounded`] callers that have
/// no reason to pick their own.
///
/// Why: 1024 events is several seconds of headroom at the emit rates the
/// pipelines actually produce (per repo, per batch — not per commit), so a TUI
/// redrawing at 10 Hz never loses anything in practice.
/// What: `1024`.
/// Test: `super::tests::default_capacity_is_used_by_new`.
pub const DEFAULT_CAPACITY: usize = 1024;

/// Shared state behind an active bus. Never exposed.
#[derive(Debug)]
struct BusInner {
    queue: Mutex<VecDeque<ProgressEvent>>,
    capacity: usize,
    dropped: AtomicU64,
}

/// A non-blocking, bounded, drop-oldest channel for [`ProgressEvent`]s.
///
/// Why: progress is advisory. A pipeline must never stall, fail, or change its
/// results because a consumer is slow, absent, or has gone away — so the bus
/// never applies backpressure and never returns an error the producer has to
/// handle. The cost of that choice is that events can be lost; the bus counts
/// them ([`ProgressBus::dropped`]) so a consumer can say so rather than
/// silently rendering a gap.
///
/// What: a clonable handle. [`ProgressBus::disabled`] (and `Default`) yields
/// an *inactive* bus whose [`ProgressBus::emit`] is a no-op and whose
/// [`ProgressBus::is_active`] is `false` — this is what every non-TUI call
/// site passes. [`ProgressBus::bounded`] yields an active bus holding a ring
/// buffer of `capacity` events; when it is full, the OLDEST event is evicted
/// to make room for the newest. Drop-oldest (rather than drop-newest) is
/// deliberate: for a live display the most recent state is the useful one, and
/// a stale head is exactly what a viewer does not need.
///
/// Consumers call [`ProgressBus::drain`] on their own cadence — typically once
/// per render tick. Producers and consumers hold the internal lock only for
/// the length of a push or a `VecDeque` swap, so neither ever waits on the
/// other's real work.
///
/// Test: `super::tests::disabled_bus_swallows_every_emit`,
/// `overflow_drops_oldest_and_counts`, `drain_returns_fifo_and_empties`.
#[derive(Debug, Clone, Default)]
pub struct ProgressBus {
    inner: Option<Arc<BusInner>>,
}

impl ProgressBus {
    /// An inactive bus: every emit is discarded, nothing is allocated.
    ///
    /// Why: this is the value every existing CLI path passes, so wiring the
    /// bus into `collect` / `classify` cannot change their behavior.
    /// What: a `ProgressBus` with no shared state. Identical to `Default`.
    /// Test: `super::tests::disabled_bus_swallows_every_emit`.
    pub fn disabled() -> Self {
        Self { inner: None }
    }

    /// An active bus with the [`DEFAULT_CAPACITY`] ring buffer.
    ///
    /// Why/What/Test: see [`ProgressBus::bounded`].
    pub fn new() -> Self {
        Self::bounded(DEFAULT_CAPACITY)
    }

    /// An active bus holding at most `capacity` un-drained events.
    ///
    /// Why: bounding the queue is what makes the producer's cost constant no
    /// matter how far behind the consumer falls.
    /// What: allocates the shared ring. A `capacity` of 0 is raised to 1 so an
    /// active bus always delivers at least the newest event.
    /// Test: `super::tests::overflow_drops_oldest_and_counts`.
    pub fn bounded(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            inner: Some(Arc::new(BusInner {
                queue: Mutex::new(VecDeque::with_capacity(capacity)),
                capacity,
                dropped: AtomicU64::new(0),
            })),
        }
    }

    /// Whether anything is listening.
    ///
    /// Why: an emit site that would have to allocate to build its event can
    /// skip that work entirely when nobody is watching.
    /// What: `true` only for a bus built by [`ProgressBus::bounded`] /
    /// [`ProgressBus::new`].
    /// Test: `super::tests::disabled_bus_swallows_every_emit`.
    #[inline]
    pub fn is_active(&self) -> bool {
        self.inner.is_some()
    }

    /// Publish one event. Never blocks on a consumer, never fails.
    ///
    /// Why: the producer is a data pipeline whose correctness must not depend
    /// on the observer, so there is no error to propagate and no backpressure
    /// to wait on.
    /// What: on an inactive bus, returns immediately. On an active bus, pushes
    /// to the back of the ring; if the ring is at capacity the front (oldest)
    /// event is evicted first and the dropped counter is incremented. A
    /// poisoned lock (a consumer panicked mid-drain) is recovered in place
    /// rather than propagated — losing progress data is never worth failing a
    /// collection run over.
    /// Test: `super::tests::overflow_drops_oldest_and_counts`.
    pub fn emit(&self, event: ProgressEvent) {
        let Some(inner) = self.inner.as_ref() else {
            return;
        };
        let mut queue = inner
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while queue.len() >= inner.capacity {
            queue.pop_front();
            inner.dropped.fetch_add(1, Ordering::Relaxed);
        }
        queue.push_back(event);
    }

    /// Take every queued event, oldest first, leaving the bus empty.
    ///
    /// Why: a render loop wants one cheap call per tick that hands over
    /// everything that accumulated since the last one.
    /// What: swaps the ring out under the lock and returns it as a `Vec` in
    /// FIFO order. Returns an empty `Vec` for an inactive bus.
    /// Test: `super::tests::drain_returns_fifo_and_empties`.
    pub fn drain(&self) -> Vec<ProgressEvent> {
        let Some(inner) = self.inner.as_ref() else {
            return Vec::new();
        };
        let mut queue = inner
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::mem::take(&mut *queue).into()
    }

    /// How many events have been evicted by the overflow policy so far.
    ///
    /// Why: a consumer that renders a gap should be able to say it is a gap.
    /// What: the cumulative drop count; `0` for an inactive bus.
    /// Test: `super::tests::overflow_drops_oldest_and_counts`.
    pub fn dropped(&self) -> u64 {
        self.inner
            .as_ref()
            .map_or(0, |i| i.dropped.load(Ordering::Relaxed))
    }

    /// Number of events currently queued and undrained.
    ///
    /// Why: used by tests and by the TUI's diagnostics line.
    /// What: the ring length; `0` for an inactive bus.
    /// Test: `super::tests::drain_returns_fifo_and_empties`.
    pub fn queued(&self) -> usize {
        self.inner.as_ref().map_or(0, |i| {
            i.queue
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .len()
        })
    }
}
