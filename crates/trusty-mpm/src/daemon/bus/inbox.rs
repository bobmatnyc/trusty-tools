//! Per-client durable inbox — the bus's delivery boundary (DOC-60 §7, #6462).
//!
//! Why: until #6462 every subscriber of one instance shared a single 64-slot
//! `tokio::sync::broadcast` ring. `broadcast` measures backlog across ALL
//! attached receivers and cannot send to a subset, so #6461 could keep the §9
//! log truthful only by refusing the publish — which made one client that
//! stops reading refuse every publish to that instance, healthy co-subscribers
//! included, for as long as it stayed attached. Owner ruling on #6462: bound
//! that. Giving each subscription its own buffer moves the delivery boundary
//! from the instance to the client, so a stalled client's backlog is its own
//! problem and costs the instance a fixed [`CLIENT_INBOX_CAPACITY`] envelopes
//! of memory rather than its availability.
//! What: [`ClientInbox`], one bounded queue per subscription; [`InboxSet`],
//! the per-instance fan-out that owns them; [`InboxSubscription`], the reader
//! handle that detaches on drop; and [`InboxEviction`], the §9 record that
//! keeps an eviction from becoming a silent loss.
//! Test: `bus::tests` — `a_wedged_client_does_not_refuse_publishes_for_a_healthy_co_subscriber`,
//! `eviction_is_recorded_per_client_in_the_durable_log`,
//! `concurrent_publishers_lose_nothing_without_a_record`,
//! `a_detached_subscription_stops_costing_the_instance`.
//!
//! ## Eviction policy, and the two points DOC-60 §7 does not settle
//!
//! §7 specifies a durable inbox and its replay-on-connect recovery. It does
//! not state a per-client buffer depth, nor which envelope loses when a live
//! client's buffer is full. Both are settled here in the direction that keeps
//! the §9 log answerable, which is §4's actual requirement:
//!
//! 1. **Depth is [`CLIENT_INBOX_CAPACITY`], per subscription.** It carries the
//!    retired shared ring's depth forward unchanged, so an instance's worst
//!    case grows with the number of attached clients rather than staying fixed
//!    — which is the point: a wedged client can no longer spend anyone else's
//!    budget.
//! 2. **The OLDEST unread envelope loses, and the loss is recorded.** #4271's
//!    defect was never the eviction; it was the lie — the log said `delivered`
//!    and nobody was told. Every eviction here writes an [`InboxEviction`] to
//!    the §9 stream naming the displaced `message_id` and the subscription that
//!    lost it, and the reader gets an [`InboxItem::Lagged`] at the point of
//!    loss. Refusing the NEWEST envelope instead would leave a recovering
//!    client reading stale traffic forever with no way to catch up, and would
//!    put the refusal back on the publisher — the wedge #6462 exists to remove.
//!
//! Recovery is unchanged from #6461 and is what makes eviction survivable: the
//! §9 JSONL stream holds every envelope, so a client that sees a `lagged` frame
//! re-reads the log from its last known `message_id`. The frame names the log's
//! path and the subscription id, which is the key its [`InboxEviction`] records
//! are written under.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

use super::envelope::BusEnvelope;

/// How many envelopes ONE subscription buffers before it displaces its oldest.
///
/// Why: peer messages are low-volume by construction — DOC-60 §3 keeps
/// streaming token deltas on the existing per-crate buses — so a shallow
/// buffer is right, and it is what bounds a never-polling client's cost. This
/// is the depth the retired shared ring had, now applied per client so one
/// client's backlog is charged to that client.
///
/// Public because it is part of the delivery contract a subscriber reasons
/// about: fall this far behind and the next envelope displaces your oldest.
/// Test: `eviction_is_recorded_per_client_in_the_durable_log`.
pub const CLIENT_INBOX_CAPACITY: usize = 64;

/// The `record` discriminator on an [`InboxEviction`] line in the §9 stream.
///
/// Why: the durable stream now carries two record shapes. A reader that
/// deserializes every line as a [`BusEnvelope`] must be able to tell them apart
/// without guessing, and an envelope record has no `record` field at all — so
/// presence of this key IS the discriminator.
/// Test: `eviction_records_are_distinguishable_from_envelopes`.
pub const INBOX_EVICTION_RECORD: &str = "inbox_eviction";

/// What one read from an inbox yields.
///
/// Why: a subscriber needs to learn about a gap AT the gap, not after the
/// fact — that is the whole content of #4271's SSE half. Making the gap a
/// value in the same stream as the envelopes puts it in order by construction.
/// What: an envelope, or the count this subscription lost since its last read.
/// The envelope is boxed because it is an order of magnitude larger than the
/// counter and the enum would otherwise be sized by its rare variant.
/// Test: `a_stalled_subscriber_reads_its_lag_before_the_surviving_envelopes`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InboxItem {
    /// One addressed envelope, in publish order.
    Envelope(Box<BusEnvelope>),
    /// This inbox displaced `missed` unread envelopes to make room. Each one
    /// has an [`InboxEviction`] record in the §9 stream.
    Lagged(u64),
}

/// One envelope displaced unread, with everything its §9 record needs.
///
/// Why: the eviction happens inside the fan-out, under the instance's lock,
/// but the durable write belongs to
/// [`PeerBus::publish`](super::PeerBus::publish) — which owns the logger and
/// the ordering. Handing the facts out as a value keeps the IO out of the
/// guarded section without losing what was lost.
/// What: the displaced envelope plus the subscription it was displaced from.
/// Test: `eviction_is_recorded_per_client_in_the_durable_log`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvictedEnvelope {
    /// The instance whose subscriber lost it.
    pub instance_id: String,
    /// Which of that instance's subscriptions lost it.
    pub subscription_id: u64,
    /// How many envelopes that subscription has lost in total, this one
    /// included.
    pub missed_total: u64,
    /// The envelope itself — the §9 log already holds it as `delivered`, and
    /// this is what says that record is no longer the whole truth.
    pub envelope: BusEnvelope,
}

impl EvictedEnvelope {
    /// The §9 record for this eviction.
    ///
    /// Test: `eviction_is_recorded_per_client_in_the_durable_log`.
    pub fn record(&self) -> InboxEviction {
        InboxEviction {
            record: INBOX_EVICTION_RECORD.to_string(),
            ts: chrono::Utc::now().to_rfc3339(),
            instance_id: self.instance_id.clone(),
            subscription_id: self.subscription_id,
            message_id: self.envelope.message_id.clone(),
            message_ts: self.envelope.ts.clone(),
            missed_total: self.missed_total,
        }
    }
}

/// The §9 record written when one client's inbox displaces an unread envelope.
///
/// Why: DOC-60 §4 forbids an outcome the sender and the operator cannot
/// distinguish, and #4271 is what that looks like when it is violated — a
/// `delivered` record for an envelope its recipient never saw. An eviction is
/// not a delivery failure (co-subscribers may well have read it), so it is not
/// a second `delivery_state` on the envelope; it is a separate fact about one
/// subscription, and it is recorded as one.
/// What: the displaced `message_id`, the subscription that lost it, and that
/// subscription's running loss count — enough to reconstruct exactly what any
/// one client did and did not see by replaying the stream.
/// Test: `eviction_is_recorded_per_client_in_the_durable_log`,
/// `eviction_records_are_distinguishable_from_envelopes`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxEviction {
    /// Always [`INBOX_EVICTION_RECORD`].
    pub record: String,
    /// RFC3339 time of the eviction.
    pub ts: String,
    /// The instance whose subscriber lost the envelope.
    pub instance_id: String,
    /// The subscription that lost it.
    pub subscription_id: u64,
    /// The `message_id` of the envelope displaced unread.
    pub message_id: String,
    /// That envelope's own timestamp, so a replay can seek to it.
    pub message_ts: String,
    /// How many envelopes this subscription has lost in total.
    pub missed_total: u64,
}

/// The mutable half of one inbox.
#[derive(Debug, Default)]
struct InboxState {
    /// Envelopes this subscription has not read, oldest first.
    queue: VecDeque<BusEnvelope>,
    /// Evictions the reader has not yet been told about.
    pending_lag: u64,
    /// Evictions over this subscription's whole life.
    evicted_total: u64,
    /// Set when the instance is deregistered; the reader drains, then ends.
    closed: bool,
}

/// One subscription's bounded, per-client buffer.
///
/// Why: see the module doc — this is what moves the delivery boundary off the
/// instance. Each inbox is written by publishers under [`InboxSet`]'s lock and
/// read by exactly one reader, the [`InboxSubscription`] that owns it.
/// What: a bounded [`VecDeque`] plus the lag counters, behind one mutex, with a
/// [`Notify`] the single reader parks on.
/// Test: `eviction_is_recorded_per_client_in_the_durable_log`,
/// `a_stalled_subscriber_reads_its_lag_before_the_surviving_envelopes`.
#[derive(Debug)]
pub struct ClientInbox {
    /// Which subscription this is — unique within its instance.
    subscription_id: u64,
    /// The instance this inbox belongs to, carried so an eviction record can
    /// name it without a registry lookup.
    instance_id: String,
    /// Queue and counters.
    state: Mutex<InboxState>,
    /// Wakes the single reader. `notify_one` stores a permit when no reader is
    /// parked, so a delivery between the reader's queue check and its `await`
    /// cannot be lost.
    signal: Notify,
}

impl ClientInbox {
    /// Recover a poisoned lock rather than fail a publish.
    ///
    /// Why: the guarded section pushes and pops a `VecDeque` and increments two
    /// counters. It calls no user code and holds no invariant across a panic,
    /// so a poisoned lock carries no corrupted state — refusing delivery on it
    /// would turn an unrelated panic elsewhere into a permanent dead inbox.
    fn state(&self) -> std::sync::MutexGuard<'_, InboxState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Which subscription this inbox serves.
    ///
    /// Test: `a_stalled_subscriber_reads_its_lag_before_the_surviving_envelopes`.
    pub fn subscription_id(&self) -> u64 {
        self.subscription_id
    }

    /// How many envelopes this subscription has lost to eviction.
    ///
    /// Test: `eviction_is_recorded_per_client_in_the_durable_log`.
    pub fn evicted_total(&self) -> u64 {
        self.state().evicted_total
    }

    /// Take one envelope, displacing this inbox's oldest when it is full.
    ///
    /// Why the oldest and not the newest: see the module doc's point 2.
    /// What: pushes; when the queue already holds [`CLIENT_INBOX_CAPACITY`],
    /// pops the front first and returns it so the caller can record the loss.
    /// A closed inbox takes nothing — its reader is on its way out and an
    /// envelope pushed into it would be recorded delivered and then never read.
    /// Test: `eviction_is_recorded_per_client_in_the_durable_log`.
    fn deliver(&self, envelope: BusEnvelope) -> Option<EvictedEnvelope> {
        let mut state = self.state();
        if state.closed {
            return None;
        }
        let evicted = if state.queue.len() >= CLIENT_INBOX_CAPACITY {
            let displaced = state.queue.pop_front();
            state.evicted_total += 1;
            state.pending_lag += 1;
            displaced.map(|envelope| EvictedEnvelope {
                instance_id: self.instance_id.clone(),
                subscription_id: self.subscription_id,
                missed_total: state.evicted_total,
                envelope,
            })
        } else {
            None
        };
        state.queue.push_back(envelope);
        drop(state);
        self.signal.notify_one();
        evicted
    }

    /// Stop the reader once it has drained what it already holds.
    ///
    /// Why: deregistration used to end a subscriber's stream by dropping the
    /// broadcast `Sender`. With per-client queues the end has to be stated,
    /// and it has to come AFTER the queue rather than instead of it — envelopes
    /// the log recorded delivered are still readable.
    /// Test: `deregister_ends_a_subscription_after_it_drains`.
    fn close(&self) {
        self.state().closed = true;
        self.signal.notify_one();
    }

    /// Take the next item if one is ready, without waiting.
    ///
    /// Why lag first: the eviction removed envelopes from the FRONT, which is
    /// where the reader is, so the gap belongs before everything still queued.
    /// That is the same ordering `broadcast` gave and what the SSE contract
    /// (#4271) already documents.
    /// Test: `a_stalled_subscriber_reads_its_lag_before_the_surviving_envelopes`.
    fn try_recv(&self) -> Option<InboxItem> {
        let mut state = self.state();
        if state.pending_lag > 0 {
            return Some(InboxItem::Lagged(std::mem::take(&mut state.pending_lag)));
        }
        state
            .queue
            .pop_front()
            .map(|e| InboxItem::Envelope(Box::new(e)))
    }

    /// Whether the queue is drained and no further envelope can arrive.
    fn is_finished(&self) -> bool {
        let state = self.state();
        state.closed && state.pending_lag == 0 && state.queue.is_empty()
    }
}

/// Every inbox attached to one instance, and the fan-out over them.
///
/// Why: the instance-level lock survives the move to per-client buffers, but
/// its job changes. It no longer gates delivery on the slowest reader — it
/// makes ONE publish's fan-out indivisible, so two concurrent publishers cannot
/// interleave and hand two clients the same two envelopes in opposite orders.
/// That is the ordering guarantee the retired broadcast ring gave for free and
/// that `two_subscribers_one_lagging_account_exactly` pins.
/// What: the attached inboxes plus the subscription-id source. Attach, detach,
/// and fan-out all take the same lock; a reader takes only its own inbox's.
/// Test: `two_subscribers_one_lagging_account_exactly`,
/// `concurrent_publishers_lose_nothing_without_a_record`.
#[derive(Debug, Default)]
pub struct InboxSet {
    /// Attached inboxes, in attachment order.
    inboxes: Mutex<Vec<Arc<ClientInbox>>>,
    /// Source of subscription ids, unique within this instance.
    next_subscription: AtomicU64,
}

/// What one fan-out actually did.
///
/// Why: the publisher must write a durable record stating what happened, and
/// the two outcomes carry different records and different answers to the
/// sender. Since #6462 a slow subscriber is no longer one of them: it cannot
/// refuse a publish, so the `Saturated` arm — and the `503` it produced — is
/// gone.
/// What: delivered to every attached inbox (naming whatever each displaced), or
/// nothing attached at all.
/// Test: `publish_without_subscriber_errors`,
/// `eviction_is_recorded_per_client_in_the_durable_log`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryOutcome {
    /// Every attached inbox holds the envelope. `evicted` names what each one
    /// displaced unread to make room — empty in the ordinary case.
    Delivered {
        /// The envelopes displaced by this delivery, one entry per loss.
        evicted: Vec<EvictedEnvelope>,
    },
    /// The instance is registered but nothing is subscribed.
    NoSubscriber,
}

impl InboxSet {
    /// Recover a poisoned lock rather than fail a publish — see
    /// [`ClientInbox::state`] for why that is safe here.
    fn inboxes(&self) -> std::sync::MutexGuard<'_, Vec<Arc<ClientInbox>>> {
        self.inboxes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Attach a new subscription and hand back its reader.
    ///
    /// Test: `a_detached_subscription_stops_costing_the_instance`.
    pub(super) fn attach(self: &Arc<Self>, instance_id: &str) -> InboxSubscription {
        let inbox = Arc::new(ClientInbox {
            subscription_id: self.next_subscription.fetch_add(1, Ordering::SeqCst),
            instance_id: instance_id.to_string(),
            state: Mutex::new(InboxState::default()),
            signal: Notify::new(),
        });
        self.inboxes().push(Arc::clone(&inbox));
        InboxSubscription {
            set: Arc::clone(self),
            inbox,
        }
    }

    /// Remove one subscription's inbox, freeing whatever it never read.
    ///
    /// Test: `a_detached_subscription_stops_costing_the_instance`.
    fn detach(&self, subscription_id: u64) {
        self.inboxes()
            .retain(|inbox| inbox.subscription_id != subscription_id);
    }

    /// Deliver one envelope to every attached inbox.
    ///
    /// Why the whole fan-out is under one lock: see the type doc — it is what
    /// makes concurrent publishers agree on an order. The guarded section is a
    /// handful of `VecDeque` pushes with no IO and no await, which is what
    /// [`CLIENT_INBOX_CAPACITY`]'s low-volume rationale already assumes.
    /// What: [`DeliveryOutcome::NoSubscriber`] when nothing is attached, else
    /// [`DeliveryOutcome::Delivered`] carrying every eviction the fan-out
    /// caused. The envelope is cloned per inbox because each client owns its
    /// copy from here on.
    /// Test: `two_subscribers_one_lagging_account_exactly`,
    /// `concurrent_publishers_lose_nothing_without_a_record`.
    pub(super) fn fan_out(&self, envelope: BusEnvelope) -> DeliveryOutcome {
        let inboxes = self.inboxes();
        if inboxes.is_empty() {
            return DeliveryOutcome::NoSubscriber;
        }
        let evicted = inboxes
            .iter()
            .filter_map(|inbox| inbox.deliver(envelope.clone()))
            .collect();
        DeliveryOutcome::Delivered { evicted }
    }

    /// End every attached subscription once it has drained.
    ///
    /// Test: `deregister_ends_a_subscription_after_it_drains`.
    pub(super) fn close_all(&self) {
        for inbox in self.inboxes().iter() {
            inbox.close();
        }
    }

    /// How many subscriptions are attached.
    ///
    /// Test: `a_detached_subscription_stops_costing_the_instance`.
    pub fn len(&self) -> usize {
        self.inboxes().len()
    }

    /// Whether nothing is subscribed.
    ///
    /// Test: `a_detached_subscription_stops_costing_the_instance`.
    pub fn is_empty(&self) -> bool {
        self.inboxes().is_empty()
    }
}

/// One client's read handle on its inbox.
///
/// Why it owns the detach: a subscriber that goes away — an SSE body dropped
/// when the client disconnects — must stop being fanned out to and must stop
/// holding envelopes nobody will read. Tying that to `Drop` means no code path
/// can forget it, which is what keeps a disconnected client's cost at zero
/// rather than at [`CLIENT_INBOX_CAPACITY`] envelopes forever.
/// What: the inbox plus its set, with `recv`/`try_recv` over the former.
/// Exactly one of these exists per inbox, which is why the inbox's `Notify`
/// only ever needs to wake one reader.
/// Test: `a_detached_subscription_stops_costing_the_instance`,
/// `deregister_ends_a_subscription_after_it_drains`.
#[derive(Debug)]
pub struct InboxSubscription {
    /// The instance's fan-out set, so `Drop` can detach.
    set: Arc<InboxSet>,
    /// This subscription's own buffer.
    inbox: Arc<ClientInbox>,
}

impl InboxSubscription {
    /// This subscription's id — the key its [`InboxEviction`] records carry.
    ///
    /// Test: `a_stalled_subscriber_reads_its_lag_before_the_surviving_envelopes`.
    pub fn subscription_id(&self) -> u64 {
        self.inbox.subscription_id()
    }

    /// How many envelopes this subscription has lost to eviction.
    ///
    /// Test: `eviction_is_recorded_per_client_in_the_durable_log`.
    pub fn evicted_total(&self) -> u64 {
        self.inbox.evicted_total()
    }

    /// The next item if one is ready, without waiting.
    ///
    /// Test: `a_stalled_subscriber_reads_its_lag_before_the_surviving_envelopes`.
    pub fn try_recv(&self) -> Option<InboxItem> {
        self.inbox.try_recv()
    }

    /// Wait for the next item; `None` once the instance is gone and the inbox
    /// is drained.
    ///
    /// Why the loop: the wake is advisory. A `notify_one` permit can outlive
    /// the item that caused it (the reader drained it on the previous pass), so
    /// the queue is re-checked on every wake rather than trusted.
    /// Test: `deregister_ends_a_subscription_after_it_drains`,
    /// `two_subscribers_one_lagging_account_exactly`.
    pub async fn recv(&self) -> Option<InboxItem> {
        loop {
            if let Some(item) = self.inbox.try_recv() {
                return Some(item);
            }
            if self.inbox.is_finished() {
                return None;
            }
            self.inbox.signal.notified().await;
        }
    }
}

impl Drop for InboxSubscription {
    fn drop(&mut self) {
        self.set.detach(self.inbox.subscription_id);
    }
}
