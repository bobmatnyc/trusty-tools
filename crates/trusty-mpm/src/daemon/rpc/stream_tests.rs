//! Parity, lag, and receiver-leak tests for the two streaming event methods
//! (#6288 slice 6).
//!
//! Why: this slice's acceptance bar is that a UDS subscriber and a concurrent
//! HTTP `EventSource` on the same session observe IDENTICAL sequences, so a test
//! that drove only the socket would pass on a handler that streamed something
//! else. Every parity test here therefore opens both transports against ONE
//! `DaemonState`, publishes through the daemon's own `push_hook_event`, and
//! compares what each side saw.
//!
//! What, in order: the shared fixtures, the two parity tests, the wire test over
//! a real socket, the receiver-leak test, and the protocol refusals.
//!
//! Nothing here touches the operator's fleet: `DaemonState::with_root` is rooted
//! in a fresh temp dir, and no test spawns a process or a tmux session.
//!
//! Test: this file IS the test module for `super`.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tower::ServiceExt;
use trusty_common::uds::server::{
    CODE_STREAM_REQUIRED, RpcOutcome, RpcRouter, RpcServeOptions, RpcStreamItems, serve_until,
};

use crate::core::hook::{HookEvent, HookEventRecord};
use crate::core::session::SessionId;
use crate::daemon::api;
use crate::daemon::state::DaemonState;

/// How long a stream read is given before the test calls it stuck.
///
/// A local broadcast delivers in microseconds; this is headroom on a loaded CI
/// machine, not a latency budget.
const GENEROUS: Duration = Duration::from_secs(10);

/// A `DaemonState` rooted in a fresh temp dir.
///
/// The `TempDir` is returned so it outlives the test — dropping it early would
/// unlink the framework root out from under the state.
fn test_state() -> (Arc<DaemonState>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("temp dir");
    let state = Arc::new(DaemonState::with_root(dir.path().to_path_buf()));
    (state, dir)
}

/// The RPC router this slice registers, alone.
///
/// Only `super::register` is mounted: a test that built the whole
/// `daemon::socket` router would also be asserting slices 2–5, which own their
/// own tests.
fn rpc_router(state: &Arc<DaemonState>) -> RpcRouter {
    super::register(RpcRouter::new(), state)
}

/// Open one streaming method and hand back its items.
///
/// Why `dispatch_streaming` rather than a socket: the frames `write_stream`
/// emits carry these items verbatim as their `result`, so comparing items is
/// comparing frames — and it removes a socket's scheduling from a parity
/// assertion that is about content. `stream_over_a_real_socket_carries_item_frames`
/// covers the wire itself.
async fn open_stream(router: &RpcRouter, method: &str, params: Value) -> RpcStreamItems {
    let frame = json!({
        "jsonrpc": "2.0", "id": 1, "stream": true,
        "method": method, "params": params,
    });
    let outcome = router
        .dispatch_streaming(frame.to_string().as_bytes())
        .await;
    let RpcOutcome::Stream { items, .. } = outcome else {
        panic!("{method} must answer a streaming request with a stream outcome");
    };
    items
}

/// Read exactly `n` items, failing the test rather than hanging.
async fn take_items(items: &mut RpcStreamItems, n: usize) -> Vec<Value> {
    let mut seen = Vec::with_capacity(n);
    for _ in 0..n {
        let item = tokio::time::timeout(GENEROUS, items.recv())
            .await
            .expect("a stream item must arrive rather than hang")
            .expect("the stream must not end early");
        seen.push(item.expect("an item, not a terminal error"));
    }
    seen
}

/// Read exactly `n` SSE `data:` payloads off an open HTTP response body.
///
/// The `KeepAlive` pings are comment lines (`:ping`), never `data:` lines, so
/// they are skipped here the same way a browser's `EventSource` skips them.
async fn take_sse_data(body: &mut Body, n: usize) -> Vec<Value> {
    let mut pending = String::new();
    let mut seen = Vec::with_capacity(n);
    tokio::time::timeout(GENEROUS, async {
        while seen.len() < n {
            let frame = body
                .frame()
                .await
                .expect("the SSE body must not end early")
                .expect("frame read ok");
            let Ok(chunk) = frame.into_data() else {
                continue;
            };
            pending.push_str(std::str::from_utf8(&chunk).expect("utf8"));
            while let Some(cut) = pending.find('\n') {
                let line: String = pending.drain(..=cut).collect();
                if let Some(payload) = line.trim_end().strip_prefix("data:") {
                    seen.push(
                        serde_json::from_str::<Value>(payload.trim())
                            .expect("an SSE data line is one JSON event"),
                    );
                }
            }
        }
    })
    .await
    .expect("the SSE stream must deliver rather than hang");
    seen
}

/// Publish one hook event through the daemon's own broadcast entry point.
fn publish(state: &Arc<DaemonState>, session: SessionId, tool: &str) {
    state.push_hook_event(HookEventRecord::now(
        session,
        HookEvent::PostToolUse,
        json!({"tool": tool}),
    ));
}

// ── Parity ───────────────────────────────────────────────────────────────────

/// `mpm.events.stream` and `GET /events` deliver the same events, in the same
/// order, to two subscribers open at once (#6288).
///
/// Why: the slice is accepted only if a socket subscriber is interchangeable
/// with a browser `EventSource`. Asserting the socket alone would pass on a
/// handler that reordered, dropped, or re-encoded events.
/// Test: this test.
#[tokio::test]
async fn stream_matches_the_sse_sequence() {
    let (state, _dir) = test_state();
    let session = SessionId::new();

    // Both subscribers must be open BEFORE the first publish: the broadcast
    // channel delivers only what is sent after `subscribe`.
    let mut items = open_stream(&rpc_router(&state), "mpm.events.stream", json!({})).await;
    let response = api::router(Arc::clone(&state))
        .oneshot(
            Request::builder()
                .uri("/events")
                .body(Body::empty())
                .expect("build the SSE request"),
        )
        .await
        .expect("the SSE route must answer");
    let mut sse = response.into_body();

    const N: usize = 5;
    for i in 0..N {
        publish(&state, session, &format!("Edit-{i}"));
    }

    let over_socket = take_items(&mut items, N).await;
    let over_http = take_sse_data(&mut sse, N).await;
    assert_eq!(
        over_socket, over_http,
        "the socket and the SSE route must observe identical sequences"
    );
    assert_eq!(over_socket.len(), N, "every published event must arrive");
}

/// `mpm.sessions.events_stream` and `GET /sessions/{id}/events` agree, and both
/// drop the other session's events (#6288).
///
/// Why: the per-session leg adds a filter, which is the part most likely to
/// diverge. Interleaving a second session's events proves the filter fired on
/// both transports rather than that nothing was published.
/// Test: this test.
#[tokio::test]
async fn session_stream_matches_the_sse_sequence() {
    let (state, _dir) = test_state();
    let watched = SessionId::new();
    let other = SessionId::new();

    let mut items = open_stream(
        &rpc_router(&state),
        "mpm.sessions.events_stream",
        json!({"id": watched.0.to_string()}),
    )
    .await;
    let response = api::router(Arc::clone(&state))
        .oneshot(
            Request::builder()
                .uri(format!("/sessions/{}/events", watched.0))
                .body(Body::empty())
                .expect("build the SSE request"),
        )
        .await
        .expect("the SSE route must answer");
    let mut sse = response.into_body();

    const N: usize = 4;
    for i in 0..N {
        publish(&state, other, &format!("Read-{i}"));
        publish(&state, watched, &format!("Edit-{i}"));
    }

    let over_socket = take_items(&mut items, N).await;
    let over_http = take_sse_data(&mut sse, N).await;
    assert_eq!(
        over_socket, over_http,
        "the socket and the SSE route must observe identical sequences"
    );
    let watched_id = watched.0.to_string();
    let other_id = other.0.to_string();
    for event in &over_socket {
        let text = event.to_string();
        assert!(text.contains(&watched_id), "wrong session in {text}");
        assert!(
            !text.contains(&other_id),
            "unrelated session leaked: {text}"
        );
    }
}

/// The shared predicate matches on the serialized session id, and nothing else.
#[tokio::test]
async fn event_mentions_session_matches_the_serialized_id() {
    let id = SessionId::new();
    let event = serde_json::to_value(HookEventRecord::now(
        id,
        HookEvent::PostToolUse,
        json!({"tool": "Edit"}),
    ))
    .expect("serialize");
    assert!(super::event_mentions_session(&event, &id.0.to_string()));
    assert!(!super::event_mentions_session(
        &event,
        &SessionId::new().0.to_string()
    ));
}

// ── Lag ──────────────────────────────────────────────────────────────────────

/// A subscriber that falls behind loses frames and KEEPS streaming — the same
/// load-shedding the SSE handlers' `filter_map(… Err(_) => None)` does (#6288).
///
/// Why: the alternative reading of a lag — end the stream — would be a semantic
/// HTTP does not have, and a caller would see a live session go silent. This
/// test overruns `EVENT_CHANNEL_CAPACITY` without draining, then proves the
/// stream still delivers a later event and that frames were in fact dropped.
/// Test: this test.
#[tokio::test]
async fn stream_skips_lagged_events_and_keeps_streaming() {
    let (state, _dir) = test_state();
    let session = SessionId::new();
    let mut items = open_stream(&rpc_router(&state), "mpm.events.stream", json!({})).await;

    // Comfortably past the broadcast channel's capacity, with nothing draining
    // the stream, so the forwarder's receiver is guaranteed to lag.
    let overrun = crate::daemon::state::EVENT_CHANNEL_CAPACITY * 2;
    for i in 0..overrun {
        publish(&state, session, &format!("Edit-{i}"));
    }
    publish(&state, session, "MARKER");

    let mut received = 0usize;
    let mut saw_marker = false;
    while !saw_marker {
        let item = tokio::time::timeout(GENEROUS, items.recv())
            .await
            .expect("a lagged stream must keep delivering rather than hang")
            .expect("a lag must not end the stream");
        let event = item.expect("a lag is skipped, never a terminal error frame");
        received += 1;
        saw_marker = event.to_string().contains("MARKER");
    }
    assert!(saw_marker, "the post-lag event must still arrive");
    assert!(
        received <= overrun,
        "the subscriber must have shed frames, saw {received} of {overrun}"
    );
}

// ── The wire, and the receiver it holds ──────────────────────────────────────

/// Serve `router` on a fresh socket until the returned trigger fires.
async fn spawn_listener(
    socket: &std::path::Path,
    router: RpcRouter,
) -> (
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<()>,
) {
    let listener = trusty_common::uds::bind_hardened(socket).expect("bind a fresh socket path");
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let handle = tokio::spawn(async move {
        serve_until(
            &listener,
            Arc::new(router),
            RpcServeOptions::default(),
            async {
                let _ = stop_rx.await;
            },
        )
        .await;
    });
    for _ in 0..200 {
        if trusty_common::uds::socket_is_serving(socket, Duration::from_millis(50)).await {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    (stop_tx, handle)
}

/// Dial `socket`, ask for a stream, and return the reader once the server's
/// forwarder has subscribed.
///
/// Why it waits: writing the request frame only proves the bytes left this
/// process. The broadcast channel delivers nothing published BEFORE
/// `event_subscribe`, so a test that published the moment this returned would
/// race the server's accept-read-dispatch and see an empty stream on a loaded
/// machine. The receiver count is the server's own acknowledgement that the
/// subscription exists, which is the condition these tests actually depend on.
async fn dial_stream(
    socket: &std::path::Path,
    state: &Arc<DaemonState>,
    request: &Value,
) -> BufReader<UnixStream> {
    let baseline = state.event_tx.receiver_count();
    let stream = tokio::time::timeout(GENEROUS, UnixStream::connect(socket))
        .await
        .expect("connect must not hang")
        .expect("a live socket must accept a connection");
    let mut reader = BufReader::new(stream);
    let mut body = serde_json::to_vec(request).expect("encode the request");
    body.push(b'\n');
    reader
        .get_mut()
        .write_all(&body)
        .await
        .expect("write the request frame");

    let mut subscribed = false;
    for _ in 0..1_000 {
        if state.event_tx.receiver_count() > baseline {
            subscribed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        subscribed,
        "the server must subscribe to the event broadcast"
    );
    reader
}

/// Over a real socket, `mpm.events.stream` writes one `"stream":"item"` frame
/// per event, carrying the event verbatim (#6288).
#[tokio::test]
async fn stream_over_a_real_socket_carries_item_frames() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (state, _state_dir) = test_state();
    let socket = dir.path().join("events.sock");
    let (stop_tx, handle) = spawn_listener(&socket, rpc_router(&state)).await;

    let mut reader = dial_stream(
        &socket,
        &state,
        &json!({
            "jsonrpc": "2.0", "id": 9, "stream": true,
            "method": "mpm.events.stream", "params": {},
        }),
    )
    .await;

    let session = SessionId::new();
    publish(&state, session, "Edit");

    let mut line = String::new();
    tokio::time::timeout(GENEROUS, reader.read_line(&mut line))
        .await
        .expect("the server must write a frame rather than hang")
        .expect("read the item frame");
    let frame: Value = serde_json::from_str(&line).expect("one JSON frame per line");
    assert_eq!(frame["stream"], json!("item"), "got {frame}");
    assert_eq!(frame["id"], json!(9));
    assert_eq!(frame["result"]["session"], json!(session.0.to_string()));
    assert_eq!(frame["result"]["event"], json!("PostToolUse"));

    let _ = stop_tx.send(());
    let _ = handle.await;
}

/// A socket client that hangs up releases the broadcast receiver its stream held
/// — the daemon's receiver count returns to baseline (#6288).
///
/// Why: every open stream holds one receiver, so a subscriber that leaks one per
/// disconnect degrades a long-running daemon until every publish fans out to
/// dead subscribers. Nothing else in this file would fail on that.
/// Test: this test.
#[tokio::test]
async fn disconnecting_a_socket_client_releases_the_broadcast_receiver() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (state, _state_dir) = test_state();
    let socket = dir.path().join("events.sock");
    let (stop_tx, handle) = spawn_listener(&socket, rpc_router(&state)).await;

    let baseline = state.event_tx.receiver_count();
    let mut reader = dial_stream(
        &socket,
        &state,
        &json!({
            "jsonrpc": "2.0", "id": 3, "stream": true,
            "method": "mpm.events.stream", "params": {},
        }),
    )
    .await;

    // Read one frame, which proves the stream is live end to end before the
    // disconnect — otherwise a passing count would prove only that nothing ever
    // subscribed.
    let session = SessionId::new();
    publish(&state, session, "Edit");
    let mut line = String::new();
    tokio::time::timeout(GENEROUS, reader.read_line(&mut line))
        .await
        .expect("the server must write a frame rather than hang")
        .expect("read the item frame");
    assert!(
        state.event_tx.receiver_count() > baseline,
        "the open stream must hold a receiver"
    );

    drop(reader);

    // Keep publishing: the forwarder is parked on the broadcast channel, so it
    // learns its consumer is gone on the next event, and the socket writer
    // learns the peer is gone on the next write.
    let mut settled = false;
    for i in 0..200 {
        publish(&state, session, &format!("Edit-{i}"));
        if state.event_tx.receiver_count() == baseline {
            settled = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        settled,
        "a disconnected client must not leak a receiver: {} open, baseline {baseline}",
        state.event_tx.receiver_count()
    );

    let _ = stop_tx.send(());
    let _ = handle.await;
}

// ── Protocol ─────────────────────────────────────────────────────────────────

/// Both names are registered as STREAMING methods, and neither leaked into the
/// unary table.
#[tokio::test]
async fn rpc_router_registers_every_documented_stream_method() {
    let (state, _dir) = test_state();
    let router = rpc_router(&state);
    let registered: Vec<&str> = router.stream_names().collect();
    assert_eq!(
        registered,
        super::METHODS.to_vec(),
        "the documented table and the router must agree"
    );
    assert!(
        router.method_names().next().is_none(),
        "this slice registers streaming methods only"
    );
}

/// A unary request for either streaming method is refused, not answered with a
/// snapshot the caller would read as the whole feed.
#[tokio::test]
async fn stream_methods_refuse_a_unary_request() {
    let (state, _dir) = test_state();
    let router = rpc_router(&state);
    for method in super::METHODS {
        let frame = json!({
            "jsonrpc": "2.0", "id": 1,
            "method": method, "params": {"id": "x"},
        });
        let response = router.dispatch(frame.to_string().as_bytes()).await;
        assert_eq!(
            response.error.expect("{method} must refuse").code,
            CODE_STREAM_REQUIRED,
            "{method} must ask for the stream flag"
        );
    }
}
