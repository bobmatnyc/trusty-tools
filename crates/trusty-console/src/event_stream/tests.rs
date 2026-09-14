//! Tests for the event-bus SSE fan-out (issue #6851).
//!
//! `stream::event_stream` is driven directly wherever the assertion is about
//! frame sequence — no HTTP client, no running server. The route-level tests
//! exist only for the three answers HTTP itself carries: 200/503/400.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode, header::CONTENT_TYPE};
use chrono::Utc;
use futures_util::StreamExt as _;
use http_body_util::BodyExt as _;
use tower::ServiceExt as _;
use trusty_common::control_bus::{EventId, HarnessEvent, HarnessPayload, HarnessSource};

use super::frames::parse_last_event_id;
use super::stream::event_stream;
use crate::event_bus::{EventBus, EventBusConfig};
use crate::server::{AppState, build_router};

/// A heartbeat short enough to observe inside a test without waiting on the
/// production 20 s interval.
const FAST_HEARTBEAT: Duration = Duration::from_millis(30);

/// How long any single frame read may take before the test fails rather than
/// hangs. Generous: these streams are in-process, so the real wait is ~0.
const FRAME_TIMEOUT: Duration = Duration::from_secs(5);

/// Build a minimal `HarnessEvent`. `seq` is overwritten by the bus on ingest.
fn make_event() -> HarnessEvent {
    HarnessEvent {
        source: HarnessSource::Mpm,
        session: None,
        seq: 0,
        at: Utc::now(),
        payload: HarnessPayload::Ping,
        id: EventId::new(),
        parent_id: None,
    }
}

/// One parsed SSE frame: its optional `id:`, its `event:` name, and its data.
#[derive(Debug)]
struct Frame {
    id: Option<u64>,
    kind: String,
    data: serde_json::Value,
}

/// Split one SSE frame's bytes into [`Frame`]. Panics on anything that is not
/// a well-formed `[id:]/event:/data:` frame, which is the assertion.
fn parse(bytes: &axum::body::Bytes) -> Frame {
    let text = String::from_utf8(bytes.to_vec()).expect("utf8 frame");
    let mut id = None;
    let mut rest = text.as_str();
    if let Some(after) = rest.strip_prefix("id: ") {
        let (value, tail) = after.split_once('\n').expect("id line ends");
        id = Some(value.parse::<u64>().expect("id is a decimal seq"));
        rest = tail;
    }
    let (kind, tail) = rest
        .strip_prefix("event: ")
        .and_then(|r| r.split_once('\n'))
        .unwrap_or_else(|| panic!("frame has no event line: {text:?}"));
    let data = tail
        .strip_prefix("data: ")
        .and_then(|d| d.strip_suffix("\n\n"))
        .unwrap_or_else(|| panic!("frame has no data line: {text:?}"));
    Frame {
        id,
        kind: kind.to_string(),
        data: serde_json::from_str(data).expect("frame data is json"),
    }
}

/// Read the next frame, failing rather than hanging.
async fn next_frame<S>(stream: &mut std::pin::Pin<Box<S>>) -> Frame
where
    S: futures_util::Stream<Item = Result<axum::body::Bytes, std::convert::Infallible>>,
{
    let bytes = tokio::time::timeout(FRAME_TIMEOUT, stream.next())
        .await
        .expect("a frame arrived before the timeout")
        .expect("the stream is still open")
        .expect("infallible");
    parse(&bytes)
}

/// Read the next raw frame as text (heartbeats are comments, not frames).
async fn next_text<S>(stream: &mut std::pin::Pin<Box<S>>) -> String
where
    S: futures_util::Stream<Item = Result<axum::body::Bytes, std::convert::Infallible>>,
{
    let bytes = tokio::time::timeout(FRAME_TIMEOUT, stream.next())
        .await
        .expect("a frame arrived before the timeout")
        .expect("the stream is still open")
        .expect("infallible");
    String::from_utf8(bytes.to_vec()).expect("utf8")
}

// ─── the resume contract ───────────────────────────────────────────────────

/// Why: this is the whole acceptance criterion of #6851 — a forced
/// disconnect and reconnect must lose nothing and repeat nothing.
/// What: opens a stream, ingests six events, reads three, DROPS the stream,
/// reopens with the last id it saw, and asserts the concatenation of both
/// connections' event ids is exactly seq 1..=6, in order, once each.
/// Test: this test.
#[tokio::test]
async fn a_reconnect_resumes_without_gap_or_duplicate() {
    let bus = Arc::new(EventBus::new(EventBusConfig { capacity: 64 }));

    let mut first = Box::pin(event_stream(&bus, None, FAST_HEARTBEAT));
    assert_eq!(next_frame(&mut first).await.kind, "ready");

    for _ in 0..6 {
        bus.ingest(make_event());
    }

    let mut seen: Vec<u64> = Vec::new();
    let mut last_id = 0;
    for _ in 0..3 {
        let frame = next_frame(&mut first).await;
        assert_eq!(frame.kind, "harness_event");
        last_id = frame.id.expect("an event frame carries its seq as the id");
        seen.push(last_id);
    }
    // The forced disconnect: exactly what axum does when a browser goes away.
    drop(first);

    let mut second = Box::pin(event_stream(&bus, Some(last_id), FAST_HEARTBEAT));
    let ready = next_frame(&mut second).await;
    assert_eq!(ready.kind, "ready");
    assert_eq!(ready.data["resumed_from"], last_id);
    assert_eq!(
        ready.data["backfill"], 3,
        "the three unread events are still in the ring: {}",
        ready.data
    );

    for _ in 0..3 {
        let frame = next_frame(&mut second).await;
        assert_eq!(frame.kind, "harness_event", "no gap frame was needed");
        seen.push(frame.id.expect("id"));
    }

    assert_eq!(
        seen,
        vec![1, 2, 3, 4, 5, 6],
        "the two connections concatenate to the bus contents exactly — \
         no gap, no duplicate"
    );
}

/// Why: the ring is bounded, so a client that was away long enough cannot be
/// resumed. DOC-73 §4.3 requires that be reported, never silent.
/// What: a two-event ring, a resume point of 1, and five events ingested — the
/// events at seq 2 and 3 are gone. Asserts a `gap` frame naming
/// `(after_seq: 1, before_seq: 4)` arrives before the surviving events, and
/// that it carries no `id:`.
/// Test: this test.
#[tokio::test]
async fn a_resume_older_than_the_ring_opens_with_a_gap() {
    let bus = Arc::new(EventBus::new(EventBusConfig { capacity: 2 }));
    for _ in 0..5 {
        bus.ingest(make_event());
    }

    let mut stream = Box::pin(event_stream(&bus, Some(1), FAST_HEARTBEAT));
    assert_eq!(next_frame(&mut stream).await.kind, "ready");

    let gap = next_frame(&mut stream).await;
    assert_eq!(gap.kind, "gap", "the unservable range is named");
    assert_eq!(gap.data["after_seq"], 1);
    assert_eq!(gap.data["before_seq"], 4, "the ring now starts at seq 4");
    assert!(
        gap.id.is_none(),
        "a gap is not an event and must never become a resume point"
    );

    let first = next_frame(&mut stream).await;
    assert_eq!(first.kind, "harness_event");
    assert_eq!(first.id, Some(4), "delivery resumes at the ring's oldest");
}

/// Why: an in-ring resume point must produce NO gap frame. The gap arm is a
/// loss report; firing it on a servable resume would train a viewer to ignore
/// it.
/// What: a caught-up client (`Last-Event-ID` equal to the newest seq) gets
/// `ready` and then only live events.
/// Test: this test.
#[tokio::test]
async fn a_caught_up_resume_emits_no_gap() {
    let bus = Arc::new(EventBus::new(EventBusConfig { capacity: 8 }));
    for _ in 0..3 {
        bus.ingest(make_event());
    }

    let mut stream = Box::pin(event_stream(&bus, Some(3), FAST_HEARTBEAT));
    let ready = next_frame(&mut stream).await;
    assert_eq!(ready.kind, "ready");
    assert_eq!(ready.data["backfill"], 0);
    assert_eq!(ready.data["next_seq"], 4);

    bus.ingest(make_event());
    let live = next_frame(&mut stream).await;
    assert_eq!(live.kind, "harness_event");
    assert_eq!(live.id, Some(4));
}

/// Why: the snapshot and the subscription are taken under one lock precisely
/// so an event ingested at that moment lands in exactly one of them. A
/// duplicate here would be invisible in production and corrupt every tree the
/// dashboard builds.
/// What: ingests events, opens a resuming stream, ingests more, and asserts
/// every id arrives exactly once and in ascending order.
/// Test: this test.
#[tokio::test]
async fn backfill_and_live_frames_never_overlap() {
    let bus = Arc::new(EventBus::new(EventBusConfig { capacity: 64 }));
    for _ in 0..4 {
        bus.ingest(make_event());
    }

    let mut stream = Box::pin(event_stream(&bus, Some(0), FAST_HEARTBEAT));
    assert_eq!(next_frame(&mut stream).await.kind, "ready");

    for _ in 0..4 {
        bus.ingest(make_event());
    }

    let mut ids = Vec::new();
    while ids.len() < 8 {
        let frame = next_frame(&mut stream).await;
        if frame.kind == "harness_event" {
            ids.push(frame.id.expect("id"));
        }
    }
    assert_eq!(
        ids,
        (1..=8).collect::<Vec<u64>>(),
        "each seq appears exactly once, in order"
    );
}

// ─── the two upstream shapes, and lag ──────────────────────────────────────

/// Why: DOC-73 §8.5 — "no events and a healthy SSE connection is 'nothing is
/// happening'". A viewer needs that signal without waiting for an event that
/// may never come.
/// What: a running bus with an empty ring answers a `ready` frame immediately,
/// then a `: heartbeat` comment on the idle timer.
/// Test: this test.
#[tokio::test]
async fn a_healthy_empty_bus_opens_ready_then_heartbeats() {
    let bus = Arc::new(EventBus::new(EventBusConfig { capacity: 8 }));
    let mut stream = Box::pin(event_stream(&bus, None, FAST_HEARTBEAT));

    let ready = next_frame(&mut stream).await;
    assert_eq!(ready.kind, "ready");
    assert_eq!(ready.data["next_seq"], 1, "nothing has been ingested");
    assert_eq!(ready.data["backfill"], 0);
    assert!(ready.data["resumed_from"].is_null());

    assert_eq!(
        next_text(&mut stream).await,
        ": heartbeat\n\n",
        "an idle healthy stream keeps the connection alive"
    );
}

/// Why: a browser that stops reading must never slow ingest — DOC-73 §4.3's
/// "ingest never blocks on fan-out". And its own loss must be reported.
/// What: opens a stream against a two-event channel, ingests six events
/// WITHOUT reading, asserts every ingest was accepted (the bus never stalled),
/// then reads and finds a `lagged` frame carrying a non-zero count and no id.
/// Test: this test.
#[tokio::test]
async fn a_slow_reader_is_told_it_lagged_and_never_stalls_the_bus() {
    let bus = Arc::new(EventBus::new(EventBusConfig { capacity: 2 }));
    let mut stream = Box::pin(event_stream(&bus, None, Duration::from_secs(60)));

    for _ in 0..6 {
        // `ingest` is synchronous and takes no reader into account; a return
        // at all is the proof it did not wait on this stalled subscriber.
        assert_eq!(
            bus.ingest(make_event()),
            crate::event_bus::bus::IngestOutcome::Ingested
        );
    }
    assert_eq!(
        bus.metrics().ingested,
        6,
        "every event was accepted while nobody was reading"
    );

    assert_eq!(next_frame(&mut stream).await.kind, "ready");
    let lagged = next_frame(&mut stream).await;
    assert_eq!(lagged.kind, "lagged", "the loss is reported, not hidden");
    assert!(
        lagged.data["dropped"].as_u64().expect("dropped count") >= 1,
        "the dropped count is reported: {}",
        lagged.data
    );
    assert!(
        lagged.id.is_none(),
        "a lag notice must not become a resume point"
    );
}

// ─── framing and the Last-Event-ID parse ───────────────────────────────────

/// Why: `id:` IS the resume protocol. A status frame carrying one would let a
/// client claim receipt of events it never got.
/// What: asserts an event frame has an id equal to its seq and that `ready`,
/// `gap` and `lagged` do not — proven across the tests above and restated
/// here on one stream.
/// Test: this test.
#[tokio::test]
async fn only_event_frames_carry_an_id_line() {
    let bus = Arc::new(EventBus::new(EventBusConfig { capacity: 2 }));
    for _ in 0..4 {
        bus.ingest(make_event());
    }
    let mut stream = Box::pin(event_stream(&bus, Some(0), FAST_HEARTBEAT));

    let ready = next_frame(&mut stream).await;
    assert_eq!(ready.kind, "ready");
    assert!(ready.id.is_none());

    let gap = next_frame(&mut stream).await;
    assert_eq!(gap.kind, "gap");
    assert!(gap.id.is_none());

    let event = next_frame(&mut stream).await;
    assert_eq!(event.kind, "harness_event");
    assert_eq!(
        event.id,
        event.data["event"]["seq"].as_u64(),
        "the id line is the event's own seq"
    );
    assert_eq!(
        event.data["persisted"], false,
        "a ring read makes no durability claim"
    );
}

/// Why: the gap payload is the shape a viewer parses, and it must name both
/// ends of what it lost.
/// What: asserts the JSON keys directly off the builder.
/// Test: this test.
#[test]
fn a_gap_frame_names_the_missing_range() {
    let bytes = super::frames::gap_frame(7, 12);
    let frame = parse(&bytes);
    assert_eq!(frame.kind, "gap");
    assert_eq!(frame.data["after_seq"], 7);
    assert_eq!(frame.data["before_seq"], 12);
}

/// Why: "start from now" on an unreadable resume point silently skips history
/// the client asked for. Fail closed.
/// What: every non-`u64` shape is an error, including the empty header a
/// lenient parse would have swallowed.
/// Test: this test.
#[test]
fn an_unparseable_last_event_id_is_rejected() {
    for raw in ["", "   ", "abc", "-1", "12.5", "9999999999999999999999"] {
        assert!(
            parse_last_event_id(raw).is_err(),
            "{raw:?} is not a seq this stream ever issued"
        );
    }
}

/// Why: whitespace around a header value is common and harmless.
/// What: a decimal seq parses, trimmed.
/// Test: this test.
#[test]
fn a_numeric_last_event_id_parses() {
    assert_eq!(parse_last_event_id("42"), Ok(42));
    assert_eq!(parse_last_event_id(" 42 "), Ok(42));
}

// ─── the route's three answers ─────────────────────────────────────────────

/// A router over a state with no event bus — the console whose ingest socket
/// never bound.
fn dead_bus_router() -> axum::Router {
    build_router(AppState::new(vec![]))
}

/// A router over a state with a running, empty bus.
fn live_bus_router() -> axum::Router {
    let bus = Arc::new(EventBus::new(EventBusConfig { capacity: 8 }));
    build_router(AppState::new(vec![]).with_event_bus(bus))
}

/// Why: DOC-73 §8.5 — "no events and a dead connection is 'cannot see'". An
/// open-but-silent 200 is indistinguishable from a healthy idle stream, so a
/// console with no bus must answer differently.
/// What: 503 with a JSON body naming the condition.
/// Test: this test.
#[tokio::test]
async fn a_dead_bus_answers_503_json() {
    let req = Request::builder()
        .uri(super::SSE_PATH)
        .body(Body::empty())
        .expect("request");
    let resp = dead_bus_router().oneshot(req).await.expect("response");

    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        resp.headers().get(CONTENT_TYPE).and_then(|v| v.to_str().ok()),
        Some("application/json")
    );
    let body = resp.into_body().collect().await.expect("body").to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json body");
    assert_eq!(json["error"], "event_bus_unavailable");
}

/// Why: the other half of the same distinction — a running bus with nothing to
/// say is a 200 stream, not an error.
/// What: 200 `text/event-stream` with the no-buffering headers, whose first
/// frame is `ready`.
/// Test: this test.
#[tokio::test]
async fn a_healthy_empty_bus_answers_200_event_stream() {
    let req = Request::builder()
        .uri(super::SSE_PATH)
        .body(Body::empty())
        .expect("request");
    let resp = live_bus_router().oneshot(req).await.expect("response");

    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers().get(CONTENT_TYPE).and_then(|v| v.to_str().ok()),
        Some("text/event-stream")
    );
    assert_eq!(
        resp.headers()
            .get("x-accel-buffering")
            .and_then(|v| v.to_str().ok()),
        Some("no")
    );

    // The stream never ends on its own, so read exactly the first frame.
    let mut body = resp.into_body();
    let frame = body
        .frame()
        .await
        .expect("a first frame")
        .expect("frame ok")
        .into_data()
        .expect("data frame");
    let text = String::from_utf8(frame.to_vec()).expect("utf8");
    assert!(
        text.starts_with("event: ready\ndata: {"),
        "a healthy stream says so immediately, got {text:?}"
    );
}

/// Why: a resume point this stream could not have issued is a client error;
/// answering 200 and starting from now would silently skip history.
/// What: `Last-Event-ID: not-a-seq` is a 400 naming the offending value.
/// Test: this test.
#[tokio::test]
async fn a_bad_last_event_id_header_answers_400() {
    let req = Request::builder()
        .uri(super::SSE_PATH)
        .header("last-event-id", "not-a-seq")
        .body(Body::empty())
        .expect("request");
    let resp = live_bus_router().oneshot(req).await.expect("response");

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = resp.into_body().collect().await.expect("body").to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json body");
    assert_eq!(json["error"], "bad_last_event_id");
    assert_eq!(json["value"], "not-a-seq");
}
