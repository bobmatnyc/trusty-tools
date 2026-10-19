//! `events.subscribe`: replay from `since_seq`, then follow live events, with
//! every missed event reported as a `Gap` (#9075, DOC-73 §4.3-§4.4).
//!
//! Why: a reader that opens mid-session must see what it missed and then what
//! happens next, in `seq` order, with no repeat and no silent hole. A reader
//! that falls behind must be told, because a stream that skips events without
//! saying so breaks the tree view below the missing node.
//! What: [`router`] registers the method on a `RpcRouter`. Each subscription
//! takes a slot (at most `max_subscribers`) and an [`crate::bus::Attach`]
//! cut: the ring's events after `since_seq` and a live receiver, read under
//! one lock. A spawned pump then sends, in order: the durable log's events in
//! `(since_seq, first ring seq)`, the ring backlog, and live events. One rule
//! reports every hole: an event whose `seq` is more than one past the last
//! `seq` the pump passed is preceded by `Gap { skipped }`, and the log-to-ring
//! boundary is checked the same way. Holes come from retention, a log write
//! dropped under backpressure, ring eviction, or a lagged live receiver.
//! `skipped` counts events of any filter, since the reader cannot know whether
//! the missing ones matched. Filtered-out events advance the cursor without a
//! gap. A failed log replay ends the stream with an error frame. A `since_seq`
//! past the bus's last `seq` is refused: the bus restarted on a fresh log, or
//! the reader is confused, and serving it would skip events silently. #9075:
//! so is a resume from `since_seq > 0` without `since_id`, or whose
//! `since_id` is not the event the ring or the log holds at that seq. A crash
//! can lose a `seq` the bus already handed out, and the next start hands it
//! out again; the id is what tells the two events apart.
//! Test: `super::serve_tests`, `tests/daemon_wire.rs`,
//! `tests/daemon_resume.rs::a_resume_after_a_kill_with_an_undrained_log_is_refused_not_skipped`.

use std::sync::Arc;

use tokio::sync::{OwnedSemaphorePermit, Semaphore, broadcast, mpsc};
use trusty_common::control_bus::{
    EventId, HarnessEvent, METHOD_EVENTS_SUBSCRIBE, SubscribeFilter, SubscribeItem, SubscribeParams,
};
use trusty_common::uds::server::{RpcError, RpcRouter, RpcStreamItems};

use crate::bus::{Attach, BusFrame, EventBus};
use crate::log::ReplayItem;

/// Items buffered between a subscription's pump and its socket writer.
///
/// A reader slower than this lets the live receiver lag, which becomes a
/// `Gap`; it never slows ingest.
const ITEM_BUFFER: usize = 64;

/// A router that serves `events.subscribe` on `bus`.
///
/// Test: `super::serve_tests::a_subscriber_beyond_the_limit_is_refused`.
pub(crate) fn router(bus: Arc<EventBus>, max_subscribers: usize) -> RpcRouter {
    let slots = Arc::new(Semaphore::new(max_subscribers));
    RpcRouter::new().typed_stream(METHOD_EVENTS_SUBSCRIBE, move |params: SubscribeParams| {
        let opened = open(Arc::clone(&bus), &slots, params);
        async move { opened }
    })
}

/// Open one subscription, or refuse it.
///
/// # Errors
///
/// An [`RpcError`] when every subscriber slot is in use, when `since_seq`
/// is past the bus's last assigned `seq`, or when a non-zero `since_seq` comes
/// without a `since_id` or with one the ring holds for another event. Either becomes the stream's terminal
/// error frame, with no item before it.
///
/// Test: `super::serve_tests::a_subscriber_beyond_the_limit_is_refused`,
/// `super::serve_tests::a_since_seq_ahead_of_the_bus_is_refused`,
/// `super::serve_tests::a_resume_without_since_id_is_refused`,
/// `super::serve_tests::a_resume_whose_since_id_names_another_event_is_refused`,
/// `super::serve_tests::a_resume_from_a_seq_no_tier_holds_is_refused`.
fn open(
    bus: Arc<EventBus>,
    slots: &Arc<Semaphore>,
    params: SubscribeParams,
) -> Result<RpcStreamItems, RpcError> {
    let slot = Arc::clone(slots).try_acquire_owned().map_err(|_| {
        RpcError::internal("events.subscribe: every subscriber slot is in use; retry later")
    })?;
    let attach = bus.attach(params.since_seq);
    if let Some(since) = params.since_seq
        && since > attach.head
    {
        return Err(RpcError::invalid_params(format!(
            "since_seq {since} is past the bus's last seq {}; resubscribe with since_seq \
             at or below it",
            attach.head
        )));
    }
    // #9075: a resume names the event it holds. The ring answers now; an
    // event that left the ring is checked against the log in the pump.
    let check = match params.since_seq {
        Some(since) if since > 0 => {
            let Some(id) = params.since_id else {
                return Err(RpcError::invalid_params(format!(
                    "since_seq {since} needs since_id, the id of the event held at that seq; \
                     resubscribe with since_seq 0 to replay what is retained"
                )));
            };
            match attach.anchor {
                Some(found) if found == id => None,
                Some(_) => return Err(id_mismatch(since)),
                None => Some((since, id)),
            }
        }
        _ => None,
    };
    let (tx, items) = mpsc::channel(ITEM_BUFFER);
    let cursor = Cursor {
        tx,
        filter: params.filters,
        last: params.since_seq.unwrap_or(attach.head),
    };
    tokio::spawn(pump(bus, attach, params.since_seq, check, cursor, slot));
    Ok(items)
}

/// The refusal for a resume whose `since_id` is not the event the bus holds
/// at `since`: the bus restarted and assigned that seq again, or the reader is
/// confused. Serving it could skip events the reader never saw.
fn id_mismatch(since: u64) -> RpcError {
    RpcError::invalid_params(format!(
        "since_id is not the event at since_seq {since}: the bus assigned that seq to \
         another event after a restart; resubscribe with since_seq 0 to replay what is \
         retained"
    ))
}

/// The refusal for a resume from a seq neither the ring nor the log holds.
fn id_unverifiable(since: u64) -> RpcError {
    RpcError::invalid_params(format!(
        "since_id cannot be checked: no event at since_seq {since} is retained; resubscribe \
         with since_seq 0 to replay what is retained"
    ))
}

/// Check a resume's `since_id` against the durable log. `false` when the
/// stream has ended with a refusal.
async fn check_against_log(bus: &EventBus, since: u64, id: EventId, cursor: &Cursor) -> bool {
    let refusal = match bus.logged_id(since).await {
        Some(Ok(Some(found))) if found == id => return true,
        Some(Ok(Some(_))) => id_mismatch(since),
        Some(Ok(None)) | None => id_unverifiable(since),
        Some(Err(e)) => RpcError::internal(format!("read the event log: {e}")),
    };
    cursor.fail(refusal).await;
    false
}

/// Where a subscription stands: the last `seq` it passed and where it sends.
struct Cursor {
    tx: mpsc::Sender<Result<serde_json::Value, RpcError>>,
    filter: SubscribeFilter,
    last: u64,
}

impl Cursor {
    /// Pass `event` on if it is new, reporting any hole before it first.
    /// `false` once the reader has gone.
    async fn deliver(&mut self, event: HarnessEvent, persisted: bool) -> bool {
        if event.seq <= self.last {
            return true;
        }
        if !self.skip_to(event.seq - 1).await {
            return false;
        }
        self.last = event.seq;
        if !self.filter.matches(&event) {
            return true;
        }
        self.send(&SubscribeItem::Event {
            event: Box::new(event),
            persisted,
        })
        .await
    }

    /// Report a `Gap` for every `seq` in `(last, seq]`, if there are any.
    async fn skip_to(&mut self, seq: u64) -> bool {
        if seq <= self.last {
            return true;
        }
        let skipped = seq - self.last;
        self.last = seq;
        self.send(&SubscribeItem::Gap { skipped }).await
    }

    async fn send(&self, item: &SubscribeItem) -> bool {
        match serde_json::to_value(item) {
            Ok(value) => self.tx.send(Ok(value)).await.is_ok(),
            Err(e) => {
                self.fail(RpcError::internal(format!("encode a subscribe item: {e}")))
                    .await;
                false
            }
        }
    }

    /// End the stream with `error` as its terminal frame.
    async fn fail(&self, error: RpcError) {
        let _ = self.tx.send(Err(error)).await;
    }
}

/// Run one subscription until the reader leaves or the bus shuts down. The
/// slot is held for exactly that long.
async fn pump(
    bus: Arc<EventBus>,
    attach: Attach,
    since: Option<u64>,
    check: Option<(u64, EventId)>,
    mut cursor: Cursor,
    _slot: OwnedSemaphorePermit,
) {
    let Attach {
        backlog,
        head,
        mut receiver,
        ..
    } = attach;
    if let Some((seq, id)) = check
        && !check_against_log(&bus, seq, id, &cursor).await
    {
        return;
    }
    if let Some(since) = since
        && !replay(&bus, since, head, backlog, &mut cursor).await
    {
        return;
    }
    follow(&mut receiver, &mut cursor).await;
}

/// Send the log's events up to the ring, then the ring backlog. `false` when
/// the stream has ended.
async fn replay(
    bus: &EventBus,
    since: u64,
    head: u64,
    backlog: Vec<HarnessEvent>,
    cursor: &mut Cursor,
) -> bool {
    // The ring holds `[boundary, head]`; the log serves `(since, boundary)`.
    let boundary = backlog.first().map_or(head + 1, |event| event.seq);
    if since + 1 < boundary {
        match bus.replay_since(since).await {
            None => {}
            Some(Err(e)) => {
                cursor
                    .fail(RpcError::internal(format!("replay the event log: {e}")))
                    .await;
                return false;
            }
            Some(Ok(items)) => {
                for item in items {
                    // A `ReplayItem::Gap` needs no arm: the next event's seq,
                    // or the boundary check below, reports the same hole.
                    if let ReplayItem::Event(event) = item {
                        if event.seq >= boundary {
                            break;
                        }
                        if !cursor.deliver(*event, true).await {
                            return false;
                        }
                    }
                }
            }
        }
        if !cursor.skip_to(boundary - 1).await {
            return false;
        }
    }
    for event in backlog {
        if !cursor.deliver(event, false).await {
            return false;
        }
    }
    true
}

/// Send live events until the reader leaves or the bus closes its channel.
async fn follow(receiver: &mut broadcast::Receiver<BusFrame>, cursor: &mut Cursor) {
    loop {
        let next = tokio::select! {
            frame = receiver.recv() => frame,
            () = cursor.tx.closed() => return,
        };
        match next {
            Ok(frame) => {
                if !cursor.deliver(frame.event, frame.persisted).await {
                    return;
                }
            }
            // The receiver resumes at the oldest event it still holds, whose
            // seq is past `last`; `deliver` reports the hole with its exact size.
            Err(broadcast::error::RecvError::Lagged(_)) => {}
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}
