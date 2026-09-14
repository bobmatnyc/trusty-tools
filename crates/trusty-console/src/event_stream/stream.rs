//! The byte stream behind `GET /api/console/events/stream/sse` (issue #6851,
//! DOC-73 §4.4).
//!
//! Why this is a module and not an inline closure in the route: the resume
//! contract — every event after `Last-Event-ID`, exactly once, with any
//! unservable range named rather than skipped — is the part that can silently
//! break, so it is built where a test drives it without an HTTP client. This
//! follows `machine_history::stream` (#6641), which solved the same
//! snapshot-then-subscribe ordering problem for host samples.
//!
//! What: [`event_stream`] takes one atomic reading of the bus
//! (`EventBus::subscribe_since`), emits a `ready` frame, then the ring backfill
//! after the resume point, then one frame per live event, with a `gap` frame
//! ahead of the backfill when the ring can no longer reach the resume point, a
//! `lagged` frame when this reader falls behind the broadcast channel, and a
//! `: heartbeat` comment on an idle timer.
//!
//! Test: `super::tests::a_reconnect_resumes_without_gap_or_duplicate`,
//! `super::tests::a_resume_older_than_the_ring_opens_with_a_gap`,
//! `super::tests::a_slow_reader_is_told_it_lagged_and_never_stalls_the_bus`.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use tokio::sync::broadcast::error::RecvError;
use tracing::debug;

use super::frames::{event_frame, gap_frame, lagged_frame, ready_frame};
use crate::event_bus::{BusFrame, EventBus};

/// How often a silent stream emits an SSE comment.
///
/// Why 20 s and why borrowed rather than redeclared: `trusty-common`'s hoisted
/// UDS→SSE bridge (#7220) owns the interval every console stream already uses,
/// and an intermediary's idle timeout does not care which route it is closing.
/// Test: `super::tests::a_healthy_empty_bus_opens_ready_then_heartbeats` drives
/// a millisecond interval instead, so the contract is proven without waiting.
pub(crate) const HEARTBEAT_INTERVAL: Duration = trusty_common::uds::sse::SSE_HEARTBEAT_INTERVAL;

/// The SSE comment sent to keep an idle connection open.
const HEARTBEAT: &[u8] = b": heartbeat\n\n";

/// Build the SSE byte stream for one subscriber.
///
/// Why the snapshot and the subscription are taken together (see
/// `EventBus::subscribe_since`): a client resuming mid-stream must get every
/// event exactly once — no gap where an event landed between the ring read and
/// the subscribe, and no duplicate where it landed in both.
///
/// Why the live tail is NOT filtered by `since_seq` while the backfill is: a
/// console restart whose durable log was unavailable rewinds `seq` to 1, so a
/// client resuming from a pre-restart id would match nothing and the stream
/// would go silent forever. Delivering the live tail unfiltered makes that case
/// visibly odd (ids go backwards) instead of invisibly dead.
///
/// What: `ready`, then a `gap` frame when the requested resume point predates
/// everything still available, then one `harness_event` frame per backfilled
/// ring event, then the live tail — an event becomes its own frame, a
/// `RecvError::Lagged(n)` becomes a `lagged` frame carrying `n`, a closed
/// channel ends the stream, and an idle `heartbeat` emits a comment. The stream
/// ends when the browser disconnects and axum drops the body.
/// Test: `super::tests::a_reconnect_resumes_without_gap_or_duplicate`,
/// `super::tests::a_resume_older_than_the_ring_opens_with_a_gap`.
pub(crate) fn event_stream(
    bus: &Arc<EventBus>,
    since_seq: Option<u64>,
    heartbeat_interval: Duration,
) -> impl futures_util::Stream<Item = Result<Bytes, Infallible>> + Send + 'static {
    let subscription = bus.subscribe_since(since_seq);
    let backfill = subscription.backfill;
    let next_seq = subscription.next_seq;
    let rx = subscription.receiver;

    // The seq the client will actually receive next: the oldest backfilled
    // event, or — with nothing buffered — whatever the bus mints next.
    let first_available = backfill.first().map_or(next_seq, |event| event.seq);

    let mut head: Vec<Result<Bytes, Infallible>> =
        vec![Ok(ready_frame(next_seq, since_seq, backfill.len()))];
    // #6851: a resume point the ring can no longer reach is reported, never
    // silently skipped (DOC-73 §4.3 "overflow is reported, never silent").
    if let Some(since) = since_seq
        && since + 1 < first_available
    {
        debug!(
            since,
            first_available, "event_stream: resume point predates the ring"
        );
        head.push(Ok(gap_frame(since, first_available)));
    }
    head.extend(backfill.into_iter().map(|event| {
        Ok(event_frame(&BusFrame {
            event,
            // Read back out of the ring, not off the live path — the durable
            // write's outcome is unknown here, so claiming `true` would be a
            // guess. `event_bus::bus`'s `BusFrame` docs own this contract.
            persisted: false,
        }))
    }));

    let head = futures_util::stream::iter(head);

    let heartbeat = tokio::time::interval_at(
        tokio::time::Instant::now() + heartbeat_interval,
        heartbeat_interval,
    );

    let tail = futures_util::stream::unfold(Some((rx, heartbeat)), |state| async move {
        let (mut rx, mut heartbeat) = state?;
        tokio::select! {
            biased;
            received = rx.recv() => match received {
                Ok(frame) => Some((Ok(event_frame(&frame)), Some((rx, heartbeat)))),
                // #6851: a reader slower than the broadcast buffer loses
                // events. It is told how many rather than left with a hole it
                // cannot see. Ingest is never slowed by this reader —
                // `broadcast::Sender::send` is synchronous and never waits.
                Err(RecvError::Lagged(dropped)) => {
                    debug!(dropped, "event_stream: subscriber lagged the bus buffer");
                    Some((Ok(lagged_frame(dropped)), Some((rx, heartbeat))))
                }
                // Only reachable once the bus itself is dropped, i.e. at
                // shutdown. Ending the stream is the honest answer.
                Err(RecvError::Closed) => None,
            },
            _ = heartbeat.tick() => Some((
                Ok(Bytes::from_static(HEARTBEAT)),
                Some((rx, heartbeat)),
            )),
        }
    });

    futures_util::StreamExt::chain(head, tail)
}
