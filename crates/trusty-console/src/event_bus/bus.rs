//! The bounded in-memory ring, dedup set, counters, seq assignment and
//! subscriber fan-out.
//!
//! Why: DOC-73 §4.3 gives console two retention tiers — an in-memory ring for
//! live subscribers and a durable log for replay (issue #6848 slice 3b, the
//! `log` submodule). The ring exists so a subscriber that attaches after
//! events have already flowed still gets recent history rather than only what
//! arrives from that point on, and dedup-by-id exists because a producer's
//! [`trusty_common::control_bus`]-based `PushClient` (§4.2, a parallel slice)
//! replays its local buffer on reconnect — the same event can legitimately
//! arrive twice. Console also owns `seq` as of this slice: DOC-73 §4.3 —
//! "`seq` is now minted once, by console, for every frame it accepted — not
//! per-process" — so [`EventBus::ingest`] overwrites whatever the producer
//! stamped with its own monotonic counter, recovered from the durable log's
//! tail on startup so numbers never repeat across a restart.
//! What: [`EventBus`] wraps a capacity-bounded `VecDeque<HarnessEvent>` plus a
//! `HashSet<EventId>` kept in lock-step with it (an id leaves the set the
//! moment its event is evicted from the ring, so the set never outgrows the
//! ring and dedup is a window over what the ring currently holds, not a
//! global history). [`EventBus::ingest`] is the single write path: dedup,
//! assign `seq`, evict-oldest-if-full, push, hand off to the durable log (a
//! non-blocking `try_send`, `log::DurableLog::enqueue`), then fan out to
//! subscribers over a [`tokio::sync::broadcast`] channel carrying [`BusFrame`]
//! — `event` plus `persisted`. `persisted` is always `false` on this live
//! path: by construction the write is still in flight (or was just dropped
//! under backpressure) at the moment of fan-out, since I/O happens
//! asynchronously on the writer task, off this hot path. A [`BusFrame`] built
//! from [`super::log::ReplayItem::Event`] (a later slice's SSE/backfill route)
//! is `persisted: true` instead — read from the log, it already is. Ingest
//! never blocks on a slow subscriber — `broadcast::Sender::send` is
//! synchronous and a subscriber that falls behind gets `Lagged` on its own
//! next `recv`, exactly like the ring's own eviction would have produced, per
//! DOC-73 §4.3's "ingest never blocks on fan-out" rule.
//! Test: `super::tests` — `a_subscriber_receives_ingested_events`,
//! `duplicate_id_is_deduped`, `eviction_at_capacity_drops_the_oldest`,
//! `ingest_assigns_sequential_console_seqs_starting_at_one`,
//! `live_fanout_frames_are_never_marked_persisted`,
//! `concurrent_producers_write_the_durable_log_in_seq_order`.

use std::collections::{HashSet, VecDeque};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::broadcast;
use trusty_common::control_bus::{EventId, HarnessEvent};

use super::log::DurableLog;

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

/// The counters this and the prior slice make observable (issues #6848).
///
/// Why: a later slice serves these over a metrics route; today they exist so
/// the bus's own behavior — dedup, eviction, durable-write backpressure — is
/// provable from outside the lock rather than only inferable from ring
/// contents.
/// What: a plain snapshot, one `Ordering::Relaxed` load per field — these are
/// independent counters, not a transaction, so relaxed ordering is sufficient
/// and matches how `trusty-console`'s other pollers read their own gauges.
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
    /// Frames whose durable-log write was skipped because the writer's
    /// bounded queue was full (issue #6848 slice 3b). Every one of these is
    /// a permanent gap `super::log::ReplayItem::Gap` will surface on the next
    /// replay across it.
    pub log_dropped: u64,
}

#[derive(Debug, Default)]
struct Counters {
    ingested: AtomicU64,
    deduped: AtomicU64,
    evicted: AtomicU64,
    log_dropped: AtomicU64,
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

/// One event as delivered to a live subscriber or replayed from the durable
/// log (issue #6848 slice 3b).
///
/// Why a wrapper rather than broadcasting a bare `HarnessEvent`: a subscriber
/// needs to tell "just happened, durability still in flight" apart from
/// "read back from the log, already durable" — see the module docs' full
/// contract. Kept local to `trusty-console` rather than added to
/// `trusty_common::control_bus::HarnessEvent` because no other crate needs to
/// agree on it: only this bus's own subscribers (the SSE route a later slice
/// adds) ever see a `BusFrame`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BusFrame {
    pub event: HarnessEvent,
    /// `false` on every live-ingest fan-out (the write is always still
    /// pending or was just dropped at the moment of send); `true` only for a
    /// frame built from a replayed, already-written log record.
    pub persisted: bool,
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
    sender: broadcast::Sender<BusFrame>,
    next_seq: AtomicU64,
    log: Option<DurableLog>,
}

impl EventBus {
    /// Build a bus with the given configuration and no durable log — seq
    /// numbering starts fresh at 1 and nothing is ever persisted. Used by
    /// every test that only exercises the ring, and as the degraded fallback
    /// when [`super::log::DurableLog::open`] fails (DOC-73 §4.1's
    /// non-blocking invariant: a console that cannot open its event log still
    /// serves everything else).
    ///
    /// Test: `super::tests::zero_capacity_is_raised_to_one`.
    pub(crate) fn new(config: EventBusConfig) -> Self {
        Self::with_log(config, None, 1)
    }

    /// Build a bus wired to a durable log, resuming seq numbering at
    /// `recovered_next_seq` (from `log::DurableLog::open`'s
    /// [`super::log::RecoveredState`]).
    pub(crate) fn with_log(
        config: EventBusConfig,
        log: Option<DurableLog>,
        recovered_next_seq: u64,
    ) -> Self {
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
            next_seq: AtomicU64::new(recovered_next_seq.max(1)),
            log,
        }
    }

    /// Accept one frame: dedup by id, assign console's own `seq`, evict the
    /// oldest if full, push, hand off to the durable log, fan out — all
    /// while still holding the ring lock.
    ///
    /// Why the lock is held across the whole decision, INCLUDING the
    /// durable-log hand-off and the fan-out send (#6848 — moved back under
    /// the lock after a code-critic review found the gap): `DurableLog::
    /// enqueue` is a non-blocking `try_send` and `broadcast::Sender::send` is
    /// synchronous — neither `.await`s or invokes a subscriber's code
    /// reentrantly, so holding the `std::sync::Mutex` across them costs
    /// nothing async-wise. Without it, a real ordering bug follows:
    /// `EventBus` is shared via `Arc` across one tokio task per producer
    /// connection, and this crate's default multi-threaded runtime can run
    /// those tasks on different OS threads. The mutex already orders two
    /// threads' `seq` assignments (5, then 6); if `log.enqueue`/
    /// `sender.send` ran after the lock was dropped, the OS scheduler would
    /// be free to run thread B's `try_send(seq=6)` before thread A's
    /// `try_send(seq=5)`, letting the durable log (and live subscribers)
    /// observe seq 6 before seq 5 — breaking `replay_since`'s file-order-
    /// equals-seq-order assumption (`super::log::replay`'s module docs).
    /// What: see module docs for the algorithm. A duplicate never consumes a
    /// `seq` value or reaches the log — only genuinely new events do, which
    /// is what keeps `seq` a compact, gapless (absent backpressure) sequence.
    /// The durable-log hand-off is `DurableLog::enqueue`, a non-blocking
    /// `try_send`; a full queue increments `log_dropped` and logs a warning
    /// but never blocks or fails ingest itself (DOC-73 §4.1's non-blocking
    /// invariant). A bus with zero subscribers returns a `SendError` from
    /// `broadcast::Sender::send`, which is discarded — no subscriber is not a
    /// failure, it is the common case before any SSE route (#6851) attaches.
    /// Test: `super::tests::a_subscriber_receives_ingested_events`,
    /// `super::tests::duplicate_id_is_deduped`,
    /// `super::tests::eviction_at_capacity_drops_the_oldest`,
    /// `super::tests::ingest_assigns_sequential_console_seqs_starting_at_one`,
    /// `super::tests::live_fanout_frames_are_never_marked_persisted`,
    /// `super::tests::concurrent_producers_write_the_durable_log_in_seq_order`.
    pub(crate) fn ingest(&self, mut event: HarnessEvent) -> IngestOutcome {
        let id = event.id;
        let mut ring = self.lock_ring();
        if !ring.ids.insert(id) {
            drop(ring);
            self.counters.deduped.fetch_add(1, Ordering::Relaxed);
            return IngestOutcome::Deduped;
        }

        // DOC-73 §4.3: console mints seq once, on acceptance, overwriting
        // whatever the producer stamped — a per-process value cannot order
        // across processes.
        event.seq = self.next_seq.fetch_add(1, Ordering::Relaxed);

        if ring.events.len() >= ring.capacity
            && let Some(evicted) = ring.events.pop_front()
        {
            ring.ids.remove(&evicted.id);
            self.counters.evicted.fetch_add(1, Ordering::Relaxed);
        }
        ring.events.push_back(event.clone());
        self.counters.ingested.fetch_add(1, Ordering::Relaxed);

        // #6848: still under `ring`'s lock — see this fn's doc comment for
        // why that is required, not just harmless, for on-disk/fan-out seq
        // order to match assignment order under concurrent producers.
        if let Some(log) = &self.log
            && !log.enqueue(event.clone())
        {
            self.counters.log_dropped.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(
                seq = event.seq,
                "event-bus: durable-log writer is backpressured; this event \
                 will not be persisted"
            );
        }

        // Best-effort: no subscriber yet is the common case (#6851 is a later
        // slice), not a failure to report. Always `persisted: false` — see
        // the module docs' contract.
        let _ = self.sender.send(BusFrame {
            event,
            persisted: false,
        });
        drop(ring);
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
    pub(crate) fn subscribe(&self) -> broadcast::Receiver<BusFrame> {
        self.sender.subscribe()
    }

    /// Replay everything the durable log has persisted after `since_seq`, in
    /// order. `None` when this bus has no durable log wired (degraded mode —
    /// see [`EventBus::new`]).
    ///
    /// # Errors
    ///
    /// `Some(Err(_))` when the log is wired but a day file could not be
    /// listed or read.
    // Reserved for the SSE/backfill route a later slice adds (#6851, DOC-73
    // §4.4); exercised today only by this module's own tests.
    #[allow(dead_code)]
    pub(crate) async fn replay_since(
        &self,
        since_seq: u64,
    ) -> Option<Result<Vec<super::log::ReplayItem>, super::log::LogError>> {
        let log = self.log.as_ref()?;
        Some(log.replay_since(since_seq).await)
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
            log_dropped: self.counters.log_dropped.load(Ordering::Relaxed),
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
