//! The bounded in-memory ring, dedup set, counters and subscriber fan-out.
//!
//! Why: DOC-73 §4.3 gives console two retention tiers — an in-memory ring for
//! live subscribers and a durable log for replay. This module is the first
//! tier only; the log is out of scope for this PR (see `super`'s module
//! docs). The ring exists so a subscriber that attaches after events have
//! already flowed still gets recent history rather than only what arrives
//! from that point on, and dedup-by-id exists because a producer's
//! [`trusty_common::control_bus`]-based `PushClient` (§4.2, a parallel slice)
//! replays its local buffer on reconnect — the same event can legitimately
//! arrive twice.
//! What: [`EventBus`] wraps a capacity-bounded `VecDeque<HarnessEvent>` plus a
//! `HashSet<EventId>` kept in lock-step with it (an id leaves the set the
//! moment its event is evicted from the ring, so the set never outgrows the
//! ring and dedup is a window over what the ring currently holds, not a
//! global history). [`EventBus::ingest`] is the single write path: dedup,
//! then evict-oldest-if-full, then push and fan out to subscribers over a
//! [`tokio::sync::broadcast`] channel sized to the ring capacity. Ingest never
//! blocks on a slow subscriber — `broadcast::Sender::send` is synchronous and
//! a subscriber that falls behind gets `Lagged` on its own next `recv`,
//! exactly like the ring's own eviction would have produced, per DOC-73 §4.3's
//! "ingest never blocks on fan-out" rule.
//! Test: `super::tests` — `ingest_reaches_a_subscriber`,
//! `duplicate_id_is_deduped`, `eviction_at_capacity_drops_the_oldest`,
//! `metrics_count_ingested_deduped_and_evicted`.

use std::collections::{HashSet, VecDeque};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::broadcast;
use trusty_common::control_bus::{EventId, HarnessEvent};

/// Default ring capacity (DOC-73 §4.3: "an in-memory ring, capacity
/// configurable, defaulting to 8192" — "sized for three (soon four)
/// harnesses' actions rather than one harness's lifecycle").
pub(crate) const DEFAULT_CAPACITY: usize = 8192;

/// Construction-time configuration for [`EventBus`].
///
/// Why a separate type rather than a bare `usize` parameter: a later slice is
/// expected to source `capacity` from console's own config surface, and a
/// named struct is where that field grows without changing every call site.
/// What: `capacity` is the ring's maximum event count; zero is raised to one
/// so the broadcast channel underneath is always constructible
/// (`tokio::sync::broadcast::channel` panics at capacity zero).
/// Test: `super::tests::zero_capacity_is_raised_to_one`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct EventBusConfig {
    pub capacity: usize,
}

impl Default for EventBusConfig {
    fn default() -> Self {
        Self {
            capacity: DEFAULT_CAPACITY,
        }
    }
}

/// The three counters this slice makes observable (issue #6848).
///
/// Why: a later slice serves these over a metrics route; today they exist so
/// the bus's own behavior — dedup, eviction — is provable from outside the
/// lock rather than only inferable from ring contents.
/// What: a plain snapshot, one `Ordering::Relaxed` load per field — these are
/// independent counters, not a transaction, so relaxed ordering is sufficient
/// and matches how `trusty-console`'s other pollers read their own gauges.
/// Test: `super::tests::metrics_count_ingested_deduped_and_evicted`.
// Reserved for the metrics route a later slice adds (#6850); exercised today
// only by this module's own tests.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct EventBusMetrics {
    /// Frames accepted into the ring (excludes dedup hits).
    pub ingested: u64,
    /// Frames dropped because their id was already in the ring.
    pub deduped: u64,
    /// Frames evicted from the ring to make room for a newer one.
    pub evicted: u64,
}

#[derive(Debug, Default)]
struct Counters {
    ingested: AtomicU64,
    deduped: AtomicU64,
    evicted: AtomicU64,
}

/// What [`EventBus::ingest`] did with one frame.
///
/// Why: a caller (today, the ingest listener's tests; later, the ingest
/// listener itself for logging) needs to distinguish "accepted" from
/// "already seen" without re-deriving it from the metrics delta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IngestOutcome {
    /// Accepted into the ring and fanned out to subscribers.
    Ingested,
    /// Dropped: an event with this id is already in the ring.
    Deduped,
}

/// The ring plus its dedup set, always mutated together under one lock.
struct Ring {
    capacity: usize,
    events: VecDeque<HarnessEvent>,
    ids: HashSet<EventId>,
}

/// The console-hosted event bus core (DOC-73 §4.1).
///
/// Why/What: see module docs.
/// Test: `super::tests`.
pub(crate) struct EventBus {
    ring: Mutex<Ring>,
    counters: Counters,
    sender: broadcast::Sender<HarnessEvent>,
}

impl EventBus {
    /// Build a bus with the given configuration.
    ///
    /// Test: `super::tests::zero_capacity_is_raised_to_one`.
    pub(crate) fn new(config: EventBusConfig) -> Self {
        let capacity = config.capacity.max(1);
        let (sender, _receiver) = broadcast::channel(capacity);
        Self {
            ring: Mutex::new(Ring {
                capacity,
                events: VecDeque::with_capacity(capacity),
                ids: HashSet::with_capacity(capacity),
            }),
            counters: Counters::default(),
            sender,
        }
    }

    /// Accept one frame: dedup by id, evict the oldest if full, push, fan out.
    ///
    /// Why the lock is held across the whole decision and never across the
    /// fan-out send: `broadcast::Sender::send` is synchronous and does not
    /// await, so holding the lock across it costs nothing async-wise, but the
    /// send is still issued after the lock is dropped — a subscriber
    /// receiving an event must be able to immediately call back into
    /// [`EventBus::snapshot`] without deadlocking on this same mutex.
    /// What: see module docs for the algorithm. A bus with zero subscribers
    /// returns a `SendError` from `broadcast::Sender::send`, which is
    /// discarded — no subscriber is not a failure, it is the common case
    /// before any SSE route (#6851) attaches.
    /// Test: `super::tests::ingest_reaches_a_subscriber`,
    /// `super::tests::duplicate_id_is_deduped`,
    /// `super::tests::eviction_at_capacity_drops_the_oldest`.
    pub(crate) fn ingest(&self, event: HarnessEvent) -> IngestOutcome {
        let id = event.id;
        let mut ring = self.lock_ring();
        if !ring.ids.insert(id) {
            drop(ring);
            self.counters.deduped.fetch_add(1, Ordering::Relaxed);
            return IngestOutcome::Deduped;
        }
        if ring.events.len() >= ring.capacity
            && let Some(evicted) = ring.events.pop_front()
        {
            ring.ids.remove(&evicted.id);
            self.counters.evicted.fetch_add(1, Ordering::Relaxed);
        }
        ring.events.push_back(event.clone());
        drop(ring);
        self.counters.ingested.fetch_add(1, Ordering::Relaxed);
        // Best-effort: no subscriber yet is the common case (#6851 is a later
        // slice), not a failure to report.
        let _ = self.sender.send(event);
        IngestOutcome::Ingested
    }

    /// Subscribe to every future ingested event (DOC-73 §4.2's fan-out seam
    /// for the SSE route #6851 adds).
    ///
    /// A subscriber that stops reading falls behind the bounded channel and
    /// receives `Lagged` on its next `recv` rather than stalling ingest —
    /// DOC-73 §4.3's "ingest never blocks on fan-out" rule.
    /// Test: `super::tests::a_subscriber_receives_ingested_events`.
    // Reserved for the SSE route a later slice adds (#6851); exercised today
    // only by this module's own tests.
    #[allow(dead_code)]
    pub(crate) fn subscribe(&self) -> broadcast::Receiver<HarnessEvent> {
        self.sender.subscribe()
    }

    /// A snapshot of the counters as of this call.
    ///
    /// Test: `super::tests::duplicate_id_is_deduped`,
    /// `super::tests::eviction_at_capacity_drops_the_oldest`.
    // Reserved for the metrics route a later slice adds (#6850); exercised
    // today only by this module's own tests.
    #[allow(dead_code)]
    pub(crate) fn metrics(&self) -> EventBusMetrics {
        EventBusMetrics {
            ingested: self.counters.ingested.load(Ordering::Relaxed),
            deduped: self.counters.deduped.load(Ordering::Relaxed),
            evicted: self.counters.evicted.load(Ordering::Relaxed),
        }
    }

    /// How many events the ring currently holds. Test-only introspection —
    /// no production caller needs the count without also needing the events
    /// themselves, which [`EventBus::subscribe`] serves instead.
    ///
    /// Test: `super::tests::eviction_at_capacity_drops_the_oldest`.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.lock_ring().events.len()
    }

    /// Whether an event with this id is currently in the ring. Test-only
    /// introspection, for the same reason as [`EventBus::len`].
    ///
    /// Test: `super::tests::eviction_at_capacity_drops_the_oldest`,
    /// `super::tests::duplicate_id_is_deduped`.
    #[cfg(test)]
    pub(crate) fn contains(&self, id: EventId) -> bool {
        self.lock_ring().ids.contains(&id)
    }

    /// Recover a poisoned lock rather than panic a second time on top of
    /// whatever already panicked while holding it — an ingest-path mutex is
    /// not worth taking the whole bus down over one panicking accessor, and
    /// the ring's own invariants (capacity, dedup) do not depend on any
    /// single field having been left mid-update.
    fn lock_ring(&self) -> std::sync::MutexGuard<'_, Ring> {
        self.ring
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
