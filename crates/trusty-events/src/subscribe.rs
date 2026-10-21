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
//! #10071: a subscription that sets `heartbeat_ms` is sent a `Heartbeat
//! { head }` once it is accepted, before the `since_id` check and the replay,
//! and then once per interval for as long as it is open. The log is read one
//! day file per step, each step raced against the next beat and against the
//! reader leaving, so a slow read neither silences the stream nor outlives
//! its reader (#10071 review). A heartbeat is a `try_send`: one that finds the
//! reader's buffer full is dropped, since that reader has frames to read. It
//! is not filtered, never moves the cursor and never reports a gap. A
//! subscription without `heartbeat_ms` gets no heartbeat, so a reader that
//! does not know the item never sees one. `heartbeat_ms` outside
//! `HEARTBEAT_MS_MIN..=HEARTBEAT_MS_MAX` is refused.
//! Test: `super::serve_tests`, `tests/daemon_wire.rs`,
//! `tests/daemon_resume.rs::a_resume_after_a_kill_with_an_undrained_log_is_refused_not_skipped`.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, broadcast, mpsc};
use tokio::time::{Interval, MissedTickBehavior};
use trusty_common::control_bus::{
    EventId, HEARTBEAT_MS_MAX, HEARTBEAT_MS_MIN, HarnessEvent, METHOD_EVENTS_SUBSCRIBE,
    SubscribeFilter, SubscribeItem, SubscribeParams,
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
/// An [`RpcError`] when `heartbeat_ms` is out of bounds, when every
/// subscriber slot is in use, when `since_seq` is past the bus's last
/// assigned `seq`, or when a non-zero `since_seq` comes without a `since_id`
/// or with one the ring holds for another event. Either becomes the stream's
/// terminal error frame, with no item before it.
///
/// Test: `super::serve_tests::a_heartbeat_ms_outside_its_bounds_is_refused`,
/// `super::serve_tests::a_subscriber_beyond_the_limit_is_refused`,
/// `super::serve_tests::a_since_seq_ahead_of_the_bus_is_refused`,
/// `super::serve_tests::a_resume_without_since_id_is_refused`,
/// `super::serve_tests::a_resume_whose_since_id_names_another_event_is_refused`,
/// `super::serve_tests::a_resume_from_a_seq_no_tier_holds_is_refused`.
fn open(
    bus: Arc<EventBus>,
    slots: &Arc<Semaphore>,
    params: SubscribeParams,
) -> Result<RpcStreamItems, RpcError> {
    let heartbeat = heartbeat_interval(params.heartbeat_ms)?;
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
    tokio::spawn(pump(
        bus,
        attach,
        params.since_seq,
        check,
        cursor,
        slot,
        heartbeat,
    ));
    Ok(items)
}

/// The heartbeat interval a subscription asked for, if any (#10071).
///
/// # Errors
///
/// `invalid_params` when `heartbeat_ms` is outside
/// `HEARTBEAT_MS_MIN..=HEARTBEAT_MS_MAX`: `0` would busy-loop.
fn heartbeat_interval(heartbeat_ms: Option<u64>) -> Result<Option<Duration>, RpcError> {
    match heartbeat_ms {
        None => Ok(None),
        Some(ms) if (HEARTBEAT_MS_MIN..=HEARTBEAT_MS_MAX).contains(&ms) => {
            Ok(Some(Duration::from_millis(ms)))
        }
        Some(ms) => Err(RpcError::invalid_params(format!(
            "heartbeat_ms {ms} is outside {HEARTBEAT_MS_MIN}..={HEARTBEAT_MS_MAX}; omit it for \
             no heartbeat"
        ))),
    }
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

/// Check a resume's `since_id` against the durable log, one day file per
/// step. `false` when the stream has ended: refused, or the reader left.
///
/// Test: `super::serve_tests::replay_reads_the_log_up_to_the_ring_then_the_ring`,
/// `super::serve_tests::a_slow_since_id_check_still_beats`.
async fn check_against_log(
    bus: &EventBus,
    since: u64,
    id: EventId,
    cursor: &Cursor,
    beats: &mut Beats,
) -> bool {
    let Some(opened) = while_beating(bus.replay_files(since - 1), bus, cursor, beats).await else {
        return false;
    };
    let refusal = match opened {
        None => id_unverifiable(since),
        Some(Err(e)) => RpcError::internal(format!("read the event log: {e}")),
        Some(Ok(mut files)) => loop {
            let Some(step) = while_beating(files.next_file(), bus, cursor, beats).await else {
                return false;
            };
            let items = match step {
                None => break id_unverifiable(since),
                Some(Err(e)) => break RpcError::internal(format!("read the event log: {e}")),
                Some(Ok(items)) => items,
            };
            // Events come in seq order after `since - 1`: the first decides.
            let first = items.into_iter().find_map(|item| match item {
                ReplayItem::Event(event) => Some(event),
                ReplayItem::Gap { .. } => None,
            });
            match first {
                None => {}
                Some(event) if event.seq != since => break id_unverifiable(since),
                Some(event) if event.id == id => return true,
                Some(_) => break id_mismatch(since),
            }
        },
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

    /// Send a heartbeat carrying `head` without waiting (#10071). A full
    /// buffer drops it: that reader has frames to read. `false` once the
    /// reader has gone.
    fn beat(&self, head: u64) -> bool {
        let Ok(value) = serde_json::to_value(SubscribeItem::Heartbeat { head }) else {
            // A two-field item always encodes; skipping a beat is harmless.
            return true;
        };
        !matches!(self.tx.try_send(Ok(value)), Err(TrySendError::Closed(_)))
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
    heartbeat: Option<Duration>,
) {
    let Attach {
        backlog,
        head,
        mut receiver,
        ..
    } = attach;
    let mut beats = heartbeat.map(|every| {
        let mut beats = tokio::time::interval_at(tokio::time::Instant::now() + every, every);
        beats.set_missed_tick_behavior(MissedTickBehavior::Delay);
        beats
    });
    // #10071: "alive" at once, before any log read can delay it.
    if beats.is_some() && !cursor.beat(bus.head()) {
        return;
    }
    if let Some((seq, id)) = check
        && !check_against_log(&bus, seq, id, &cursor, &mut beats).await
    {
        return;
    }
    if let Some(since) = since
        && !replay(&bus, since, head, backlog, &mut cursor, &mut beats).await
    {
        return;
    }
    follow(&bus, &mut receiver, &mut cursor, &mut beats).await;
}

/// A subscription's heartbeat schedule; `None` when it did not ask.
type Beats = Option<Interval>;

/// Await `work` while beating on schedule (#10071 review). `None`, with
/// `work` dropped, once the reader has gone.
async fn while_beating<T>(
    work: impl std::future::Future<Output = T>,
    bus: &EventBus,
    cursor: &Cursor,
    beats: &mut Beats,
) -> Option<T> {
    tokio::pin!(work);
    loop {
        tokio::select! {
            out = &mut work => return Some(out),
            () = cursor.tx.closed() => return None,
            () = next_beat(beats) => {
                if !cursor.beat(bus.head()) {
                    return None;
                }
            }
        }
    }
}

/// Send the log's events up to the ring, one day file per step, then the
/// ring backlog. `false` when the stream has ended.
///
/// Test: `super::serve_tests::replay_reads_the_log_up_to_the_ring_then_the_ring`,
/// `super::serve_tests::a_slow_replay_read_still_beats`,
/// `super::serve_tests::a_pump_whose_reader_left_stops_reading_the_log`.
async fn replay(
    bus: &EventBus,
    since: u64,
    head: u64,
    backlog: Vec<HarnessEvent>,
    cursor: &mut Cursor,
    beats: &mut Beats,
) -> bool {
    // The ring holds `[boundary, head]`; the log serves `(since, boundary)`.
    let boundary = backlog.first().map_or(head + 1, |event| event.seq);
    if since + 1 < boundary {
        let Some(opened) = while_beating(bus.replay_files(since), bus, cursor, beats).await else {
            return false;
        };
        match opened {
            None => {}
            Some(Err(e)) => {
                cursor
                    .fail(RpcError::internal(format!("replay the event log: {e}")))
                    .await;
                return false;
            }
            Some(Ok(mut files)) => 'files: loop {
                let Some(step) = while_beating(files.next_file(), bus, cursor, beats).await else {
                    return false;
                };
                let items = match step {
                    None => break,
                    Some(Ok(items)) => items,
                    Some(Err(e)) => {
                        cursor
                            .fail(RpcError::internal(format!("replay the event log: {e}")))
                            .await;
                        return false;
                    }
                };
                for item in items {
                    // A `ReplayItem::Gap` needs no arm: the next event's seq,
                    // or the boundary check below, reports the same hole.
                    if let ReplayItem::Event(event) = item {
                        if event.seq >= boundary {
                            break 'files;
                        }
                        if !cursor.deliver(*event, true).await {
                            return false;
                        }
                    }
                }
            },
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

/// Send live events, and a heartbeat per interval when one was asked for,
/// until the reader leaves or the bus closes its channel.
async fn follow(
    bus: &EventBus,
    receiver: &mut broadcast::Receiver<BusFrame>,
    cursor: &mut Cursor,
    beats: &mut Beats,
) {
    loop {
        let next = tokio::select! {
            frame = receiver.recv() => frame,
            () = cursor.tx.closed() => return,
            () = next_beat(beats) => {
                if !cursor.beat(bus.head()) {
                    return;
                }
                continue;
            }
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

/// The next heartbeat tick; never, for a subscription that did not ask.
async fn next_beat(beats: &mut Option<Interval>) {
    match beats {
        Some(beats) => {
            beats.tick().await;
        }
        None => std::future::pending().await,
    }
}
