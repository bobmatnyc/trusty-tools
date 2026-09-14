//! The four frame shapes `/api/console/events/stream/sse` puts on the wire,
//! and the `Last-Event-ID` parse in front of them (issue #6851, DOC-73 §4.4).
//!
//! Why a module of its own: the resume contract lives entirely in which frames
//! carry an `id:` line and which do not. An `id:` line is the value a browser
//! sends back as `Last-Event-ID` on reconnect, so stamping one on a frame that
//! is not an event would make the client claim receipt of events it never got.
//! Keeping the four builders side by side is what makes that rule reviewable.
//!
//! What: [`event_frame`] carries one bus event and IS the only frame with an
//! `id:` line — `id: <seq>`. [`gap_frame`] and [`lagged_frame`] report loss and
//! deliberately carry no id. [`ready_frame`] opens every stream so a viewer can
//! tell a healthy-but-silent bus from a dead one (DOC-73 §8.5) without waiting
//! for the first heartbeat. [`parse_last_event_id`] is the fail-closed parse:
//! a header that is present but not a `u64` is a client error, never a silent
//! "start from now".
//!
//! Test: `super::tests::only_event_frames_carry_an_id_line`,
//! `super::tests::a_gap_frame_names_the_missing_range`,
//! `super::tests::an_unparseable_last_event_id_is_rejected`.

use axum::body::Bytes;
use serde::Serialize;

use crate::event_bus::BusFrame;
// #6851: the `event: <kind>\ndata: {json}\n\n` renderer already exists for the
// machine-status stream (#6641) and is the crate's one SSE frame encoder — a
// second copy is how two streams start disagreeing about newline handling.
use crate::machine_history::events::sse_frame;

/// The `event:` name every bus event is delivered under.
pub(crate) const EVENT_KIND: &str = "harness_event";

/// The `event:` name for a range that will never be delivered.
pub(crate) const GAP_KIND: &str = "gap";

/// The `event:` name for the opening frame.
pub(crate) const READY_KIND: &str = "ready";

/// What went wrong parsing a `Last-Event-ID` header.
///
/// Why an error rather than a fallback: a header that cannot be read is a
/// client whose resume point is unknown. Starting from now would silently skip
/// every event between the client's real position and this moment.
/// Test: `super::tests::an_unparseable_last_event_id_is_rejected`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LastEventIdError {
    /// The header value as received, for the 400 body.
    pub value: String,
}

/// The JSON body of one `harness_event` frame.
///
/// Mirrors [`BusFrame`]'s two fields rather than flattening `persisted` into
/// the envelope, so the wire shape stays a superset of the envelope a
/// programmatic reader already parses.
#[derive(Debug, Serialize)]
struct EventPayload<'a> {
    event: &'a trusty_common::control_bus::HarnessEvent,
    persisted: bool,
}

/// The JSON body of the opening `ready` frame.
#[derive(Debug, Serialize)]
struct ReadyPayload {
    /// The seq the next accepted event will carry.
    next_seq: u64,
    /// The `Last-Event-ID` this stream resumed from, `null` for a fresh viewer.
    resumed_from: Option<u64>,
    /// How many already-ringed events follow this frame before the live tail.
    backfill: usize,
}

/// The JSON body of a `gap` frame.
#[derive(Debug, Serialize)]
struct GapPayload {
    after_seq: u64,
    before_seq: u64,
}

/// Render one bus event, stamped with its seq as the SSE `id:`.
///
/// Why the id line comes first: SSE field order is free, and putting the
/// resume point ahead of the payload keeps a truncated frame from ever
/// advancing a client's `Last-Event-ID` past data it did not receive — a
/// partial frame is discarded whole by the parser, id line included.
/// What: `id: <seq>\nevent: harness_event\ndata: {"event":…,"persisted":…}\n\n`.
/// Test: `super::tests::only_event_frames_carry_an_id_line`.
pub(crate) fn event_frame(frame: &BusFrame) -> Bytes {
    let payload = EventPayload {
        event: &frame.event,
        persisted: frame.persisted,
    };
    let body = sse_frame(EVENT_KIND, &payload);
    let mut out = Vec::with_capacity(body.len() + 16);
    out.extend_from_slice(format!("id: {}\n", frame.event.seq).as_bytes());
    out.extend_from_slice(&body);
    Bytes::from(out)
}

/// Announce a range of seqs that will never arrive on this stream.
///
/// Why: a resume point older than the ring's oldest surviving event cannot be
/// honoured. DOC-73 §4.3 requires that overrun be "reported, never silent", and
/// this is the frame that reports it — the same `(after_seq, before_seq)` shape
/// the durable log's own `ReplayItem::Gap` uses (`event_bus/log/replay.rs`), so
/// a viewer parses one gap shape whichever tier produced it.
/// What: `event: gap\ndata: {"after_seq":a,"before_seq":b}\n\n`, no `id:` line —
/// a gap is not an event and must not become a client's resume point.
/// Test: `super::tests::a_gap_frame_names_the_missing_range`,
/// `super::tests::a_resume_older_than_the_ring_opens_with_a_gap`.
pub(crate) fn gap_frame(after_seq: u64, before_seq: u64) -> Bytes {
    sse_frame(
        GAP_KIND,
        &GapPayload {
            after_seq,
            before_seq,
        },
    )
}

/// The opening frame of every stream.
///
/// Why it exists: DOC-73 §8.5 requires a viewer to distinguish "nothing is
/// happening" from "cannot see". A dead bus never reaches this module at all
/// (the route answers 503), so receiving this frame IS the healthy signal —
/// available immediately, rather than after the first heartbeat interval.
/// What: `event: ready\ndata: {"next_seq":…,"resumed_from":…,"backfill":…}\n\n`,
/// no `id:` line — it is a status frame, not an event.
/// Test: `super::tests::a_healthy_empty_bus_opens_ready_then_heartbeats`.
pub(crate) fn ready_frame(next_seq: u64, resumed_from: Option<u64>, backfill: usize) -> Bytes {
    sse_frame(
        READY_KIND,
        &ReadyPayload {
            next_seq,
            resumed_from,
            backfill,
        },
    )
}

/// Report how many events a slow reader's own lag cost it.
///
/// Why not a `gap` frame: the broadcast channel reports a COUNT, not a seq
/// range — the dropped events are gone before this reader ever saw their
/// numbers. Saying "you lost n" is the honest shape; inventing a range is not.
/// What: `event: lagged\ndata: {"dropped":n}\n\n`, no `id:` line. Reuses the
/// machine-status stream's own builder (#6641), which emits exactly this.
/// Test: `super::tests::a_slow_reader_is_told_it_lagged_and_never_stalls_the_bus`.
pub(crate) fn lagged_frame(dropped: u64) -> Bytes {
    crate::machine_history::events::lagged_frame(dropped)
}

/// Parse a `Last-Event-ID` header value into a resume point.
///
/// Why fail closed: this crate only ever stamps a decimal seq into an `id:`
/// line, so a value that is not one did not come from this stream. Treating it
/// as "start from now" would answer 200 and silently skip history the client
/// asked for; a 400 tells the client its resume point is unusable.
/// What: trims surrounding whitespace, then `u64::from_str`. An empty value
/// fails too — it is not a resume point this stream could have issued.
/// Test: `super::tests::an_unparseable_last_event_id_is_rejected`,
/// `super::tests::a_numeric_last_event_id_parses`.
pub(crate) fn parse_last_event_id(raw: &str) -> Result<u64, LastEventIdError> {
    raw.trim().parse::<u64>().map_err(|_| LastEventIdError {
        value: raw.to_string(),
    })
}
