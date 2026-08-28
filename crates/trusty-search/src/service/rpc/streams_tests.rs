//! Socket-versus-SSE parity for the two streaming surfaces (#6285 slice 5).
//!
//! Why: a stream can fail in two ways a one-shot body cannot, and both are
//! silent. It can deliver a DIFFERENT sequence than the SSE route delivers for
//! the same operation — a missing replay event, a reordered progress event — and
//! it can deliver the right sequence while leaking the task and the broadcast
//! subscription that produced it, once per abandoned dashboard. So every case
//! here drives the REAL axum router or the REAL RPC router against one shared
//! state, and the disconnect case drives a REAL Unix socket rather than the
//! in-process dispatcher, because only a real client can hang up.
//!
//! **The comparison excludes SSE's framing and nothing else.** `data: ` and the
//! blank-line separator come off; the JSON document between them is compared
//! whole. The heartbeat comment frame has no socket counterpart by design — see
//! this module's parent header — and no test here produces one, because the
//! interval is 20 s and every case completes well inside it.
//!
//! Test: this file IS the test module for `super`.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use futures_util::StreamExt as _;
use tower::ServiceExt as _;
use trusty_common::uds::server::{RpcOutcome, RpcRouter, RpcStreamItems};

use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
use crate::service::concurrency::ConcurrencyLimiter;
use crate::service::query_timeout::QueryTimeoutConfig;
use crate::service::reindex::{ReindexProgress, ReindexStatus};
use crate::service::rpc::error::CODE_NOT_FOUND;
use crate::service::rpc::streams;
use crate::service::server::{build_router_on, DaemonEvent, SearchAppState};

/// How long a real socket dial or a bounded poll is given before the test calls
/// it stuck. Headroom on a loaded machine, not a latency budget.
const GENEROUS: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------- fixtures ---

/// One state carrying one registered index, plus both routers built on it.
///
/// Why both from the SAME `Arc`: the property under test is that the socket
/// stream and the SSE route read ONE `reindex_progress` map and ONE event
/// broadcaster. Two `Arc::new` calls would make every comparison below a
/// comparison of two independent daemons.
async fn routers(index_ids: &[&str]) -> (Arc<SearchAppState>, Router, RpcRouter) {
    let registry = IndexRegistry::new();
    for id in index_ids {
        let root = format!("/nonexistent/streams-{id}");
        registry.register(IndexHandle::bare(
            IndexId::new((*id).to_string()),
            Arc::new(tokio::sync::RwLock::new(CodeIndexer::new(*id, &root))),
            root.into(),
        ));
    }
    let state = Arc::new(SearchAppState::new(registry));
    let http = build_router_on(
        Arc::clone(&state),
        trusty_common::server::SelfOrigins::default(),
    );
    let rpc = streams::register(RpcRouter::new(), &state);
    (state, http, rpc)
}

/// Plant a reindex-progress record for `id` holding `events`, at `status`.
///
/// `push` is the daemon's own emission point — it appends to the replay buffer
/// AND broadcasts — so a record built through it is the record a real reindex
/// would have left behind.
async fn plant_progress(
    state: &Arc<SearchAppState>,
    id: &str,
    status: ReindexStatus,
    events: &[serde_json::Value],
) -> Arc<ReindexProgress> {
    let progress = Arc::new(ReindexProgress::new());
    for event in events {
        progress.push(event.clone()).await;
    }
    progress.status.store(status);
    state
        .reindex_progress
        .insert(IndexId::new(id.to_string()), Arc::clone(&progress));
    progress
}

/// The three-event sequence a short reindex leaves in its replay buffer.
fn progress_sequence() -> Vec<serde_json::Value> {
    vec![
        serde_json::json!({ "event": "start", "total_files": 2 }),
        serde_json::json!({
            "event": "progress", "indexed": 1, "total": 2, "current_file": "src/a.rs",
        }),
        serde_json::json!({ "event": "complete", "indexed": 2, "elapsed_ms": 12 }),
    ]
}

// ------------------------------------------------------------- transports ---

/// Every `data:` payload of an SSE body that ENDS, parsed.
///
/// Only usable on a stream that terminates — the completed-reindex replay arm.
/// A live stream is read incrementally by [`sse_first_payloads`] instead.
async fn sse_payloads(router: &Router, uri: &str) -> Vec<serde_json::Value> {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .body(Body::empty())
                .expect("build the request"),
        )
        .await
        .expect("the router must answer");
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "GET {uri} must be served"
    );
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read the SSE body");
    parse_payloads(&String::from_utf8_lossy(&bytes))
}

/// The first `count` `data:` payloads of an SSE body that never ends.
async fn sse_first_payloads(router: &Router, uri: &str, count: usize) -> Vec<serde_json::Value> {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .body(Body::empty())
                .expect("build the request"),
        )
        .await
        .expect("the router must answer");
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "GET {uri} must be served"
    );

    let mut chunks = response.into_body().into_data_stream();
    let mut text = String::new();
    let mut payloads = Vec::new();
    while payloads.len() < count {
        let chunk = tokio::time::timeout(GENEROUS, chunks.next())
            .await
            .expect("the SSE body must keep flowing rather than hang")
            .expect("the SSE body must not end before it has written its frames")
            .expect("read one SSE chunk");
        text.push_str(&String::from_utf8_lossy(&chunk));
        payloads = parse_payloads(&text);
    }
    payloads.truncate(count);
    payloads
}

/// Strip SSE framing, keeping every `data:` document and nothing else.
fn parse_payloads(text: &str) -> Vec<serde_json::Value> {
    text.split("\n\n")
        .filter_map(|frame| frame.trim_start().strip_prefix("data: "))
        .map(|line| serde_json::from_str(line).expect("every data: line must be one JSON document"))
        .collect()
}

/// Open one socket stream through the router's own streaming dispatcher.
///
/// The `"stream": true` flag is what selects the streaming table — without it
/// the same name answers one frame carrying `CODE_STREAM_REQUIRED`.
async fn open_stream(rpc: &RpcRouter, method: &str, params: serde_json::Value) -> RpcStreamItems {
    let frame = serde_json::to_vec(&serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": method, "params": params, "stream": true,
    }))
    .expect("encode the frame");
    match rpc.dispatch_streaming(&frame).await {
        RpcOutcome::Stream { items, .. } => items,
        other => panic!("{method} must answer a stream, got {other:?}"),
    }
}

/// Read `count` items off a stream, failing rather than hanging.
async fn take_items(items: &mut RpcStreamItems, count: usize) -> Vec<serde_json::Value> {
    let mut taken = Vec::with_capacity(count);
    for _ in 0..count {
        let item = tokio::time::timeout(GENEROUS, items.recv())
            .await
            .expect("the producer must deliver rather than hang")
            .expect("the stream must not end before it has delivered its items");
        taken.push(item.expect("this stream must not carry a terminal error"));
    }
    taken
}

/// Drain a stream that ends on its own.
async fn drain(mut items: RpcStreamItems) -> Vec<serde_json::Value> {
    let mut all = Vec::new();
    while let Some(item) = tokio::time::timeout(GENEROUS, items.recv())
        .await
        .expect("the producer must terminate rather than hang")
    {
        all.push(item.expect("this stream must not carry a terminal error"));
    }
    all
}

// -------------------------------------------------------- reindex parity ---

/// Why: the replay buffer is the whole reason a late subscriber sees the `start`
/// event at all, and it is where the two transports could most easily disagree
/// — the SSE handler reads it under one lock ordering and this module copies
/// that ordering by hand. This drives both against ONE planted record and
/// compares the sequences whole, so a dropped, reordered, or re-encoded event
/// fails here rather than in a dashboard.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn reindex_progress_events_match_the_sse_body_frame_for_frame() {
    let (state, http, rpc) = routers(&["rs"]).await;
    let expected = progress_sequence();
    plant_progress(&state, "rs", ReindexStatus::Complete, &expected).await;

    let over_sse = sse_payloads(&http, "/indexes/rs/reindex/stream").await;
    let over_socket = drain(
        open_stream(
            &rpc,
            streams::METHOD_INDEX_REINDEX_STREAM,
            serde_json::json!({ "index_id": "rs" }),
        )
        .await,
    )
    .await;

    assert_eq!(
        over_sse, expected,
        "the SSE route must replay the planted sequence"
    );
    assert_eq!(
        over_socket, over_sse,
        "the socket stream must deliver the SSE sequence, framing excluded"
    );
}

/// Why: a finished reindex never sends again, so a producer that subscribed and
/// then waited would hold the connection open forever after the replay. The SSE
/// handler ends the body instead, and this proves the socket ends the stream —
/// the terminal frame arrives, rather than the client timing out and reporting a
/// truncated stream.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_finished_reindex_stream_ends_after_its_replay() {
    let (state, _http, rpc) = routers(&["rs"]).await;
    plant_progress(&state, "rs", ReindexStatus::Complete, &progress_sequence()).await;

    let items = open_stream(
        &rpc,
        streams::METHOD_INDEX_REINDEX_STREAM,
        serde_json::json!({ "index_id": "rs" }),
    )
    .await;
    assert_eq!(
        drain(items).await.len(),
        3,
        "the stream must end on its own once the replay is delivered"
    );
}

/// Why: the replay arm proves the buffered half. This proves the LIVE half — an
/// event emitted after the subscription reaches the socket, in order, without
/// waiting for the reindex to finish. A producer that collected before sending
/// would pass every test above and fail this one.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_live_reindex_stream_delivers_events_in_order_as_they_are_emitted() {
    let (state, _http, rpc) = routers(&["rs"]).await;
    let start = serde_json::json!({ "event": "start", "total_files": 3 });
    let progress = plant_progress(
        &state,
        "rs",
        ReindexStatus::Running,
        std::slice::from_ref(&start),
    )
    .await;

    let mut items = open_stream(
        &rpc,
        streams::METHOD_INDEX_REINDEX_STREAM,
        serde_json::json!({ "index_id": "rs" }),
    )
    .await;
    assert_eq!(
        take_items(&mut items, 1).await,
        vec![start],
        "the buffered start event must arrive before any live one"
    );

    let live: Vec<serde_json::Value> = (1..=3)
        .map(|n| serde_json::json!({ "event": "progress", "indexed": n, "total": 3 }))
        .collect();
    for event in &live {
        progress.push(event.clone()).await;
    }
    assert_eq!(
        take_items(&mut items, live.len()).await,
        live,
        "live events must arrive in emission order"
    );
}

/// Why: the SSE route answers `404` for an index with no recorded reindex, and a
/// stream that opened and then ended empty would read to a consumer as "the
/// reindex produced nothing" rather than "there is no reindex". This pins the
/// refusal as a terminal error frame carrying the code the read surface uses for
/// the same HTTP status.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_index_is_refused_before_the_reindex_stream_opens() {
    let (_state, http, rpc) = routers(&["rs"]).await;

    let response = http
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/indexes/never-reindexed-6285/reindex/stream")
                .body(Body::empty())
                .expect("build the request"),
        )
        .await
        .expect("the router must answer");
    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "the SSE route refuses an index with no recorded reindex"
    );

    let mut items = open_stream(
        &rpc,
        streams::METHOD_INDEX_REINDEX_STREAM,
        serde_json::json!({ "index_id": "never-reindexed-6285" }),
    )
    .await;
    let refusal = tokio::time::timeout(GENEROUS, items.recv())
        .await
        .expect("the refusal must arrive rather than hang")
        .expect("a refused stream carries one item")
        .expect_err("that item must be the refusal, not a progress event");
    assert_eq!(
        refusal.code, CODE_NOT_FOUND,
        "the socket must report the SSE route's 404 as its own not-found code: {refusal:?}"
    );
}

// --------------------------------------------------------- status parity ---

/// Why: `{"type":"connected"}` is what tells a dashboard its subscription is
/// live before any event exists. A socket stream that omitted it would leave a
/// consumer unable to distinguish "subscribed, nothing happening" from "not
/// subscribed yet".
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn the_status_stream_opens_with_the_same_connected_frame_sse_writes() {
    let (_state, http, rpc) = routers(&[]).await;

    let over_sse = sse_first_payloads(&http, "/status/stream", 1).await;
    let mut items = open_stream(&rpc, streams::METHOD_STATUS_STREAM, serde_json::Value::Null).await;
    let over_socket = take_items(&mut items, 1).await;

    assert_eq!(
        over_sse,
        vec![serde_json::json!({ "type": "connected" })],
        "the SSE route opens with the connected frame"
    );
    assert_eq!(
        over_socket, over_sse,
        "the socket stream must open with the same frame"
    );
}

/// Why: the connected frame is written by the producer itself, so it would match
/// even if the socket never reached the shared broadcaster. This emits a REAL
/// `DaemonEvent` and asserts both transports carry the same document for it —
/// which they can only do by reading one `state.events`.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_daemon_event_reaches_both_the_sse_body_and_the_socket_stream() {
    let (state, http, rpc) = routers(&[]).await;

    // Both subscriptions must exist before the event is emitted: a broadcast
    // delivers only to receivers that are already attached.
    let mut items = open_stream(&rpc, streams::METHOD_STATUS_STREAM, serde_json::Value::Null).await;
    assert_eq!(take_items(&mut items, 1).await.len(), 1, "connected frame");
    let sse = tokio::spawn({
        let http = http.clone();
        async move { sse_first_payloads(&http, "/status/stream", 2).await }
    });
    for _ in 0..200 {
        if state.events.receiver_count() >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let emitted = DaemonEvent::IndexRegistered {
        id: "streams-6285".to_string(),
    };
    state
        .events
        .send(emitted.clone())
        .expect("two subscribers are attached");

    let over_socket = take_items(&mut items, 1).await;
    let over_sse = tokio::time::timeout(GENEROUS, sse)
        .await
        .expect("the SSE reader must finish rather than hang")
        .expect("the SSE reader task must not panic");

    assert_eq!(
        over_socket,
        vec![serde_json::to_value(&emitted).expect("a DaemonEvent serialises")],
        "the socket must carry the event's own JSON"
    );
    assert_eq!(
        over_sse.last(),
        over_socket.first(),
        "and it must be the document the SSE body carries"
    );
}

// ------------------------------------------------------ early disconnect ---

/// Why: this is the failure a purely in-process test cannot reach — only a real
/// client can hang up. A dashboard that closes its tab leaves a producer task, a
/// broadcast subscription and a connection behind, and a producer that ignored
/// its `send` error would keep all three for the daemon's lifetime. The
/// subscriber count on the daemon's own broadcaster is the observable: it rises
/// when the stream opens and must fall back once the write fails.
///
/// **The disconnect costs ONE event to clear, and this test emits it.** The
/// stream client half-closes its write side after the request frame, so the
/// server's read-EOF does not mean the caller left — `write_stream` finds out at
/// its next write. Emitting here is what a live daemon's 2 s status ticker does
/// on its own; a test that asserted the count falls with nothing emitted would
/// be asserting a mechanism this transport does not have.
///
/// The limiter is checked on both sides of the disconnect: a stream that took a
/// permit would leak one per abandoned dashboard, which no later event clears.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_client_that_drops_mid_stream_leaves_no_producer_subscribed() {
    use trusty_common::uds::server::{serve_until, RpcServeOptions};

    // One permit, so a stream that took one would make the admissions below
    // fail outright rather than merely queue behind something.
    let state = Arc::new(SearchAppState::new(IndexRegistry::new()).with_query_guards(
        ConcurrencyLimiter::with_limits(1, 0),
        QueryTimeoutConfig::from_duration(Duration::from_secs(30)),
    ));
    let rpc = Arc::new(streams::register(RpcRouter::new(), &state));

    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("streams.sock");
    let listener = trusty_common::uds::bind_singleton_hardened(&socket)
        .await
        .expect("bind a fresh socket path");
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let served = tokio::spawn(async move {
        serve_until(&listener, rpc, RpcServeOptions::default(), async {
            let _ = stop_rx.await;
        })
        .await;
    });
    for _ in 0..200 {
        if trusty_common::uds::socket_is_serving(&socket, Duration::from_millis(50)).await {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let before = state.events.receiver_count();
    let mut stream: trusty_common::uds::FramedStream<serde_json::Value> =
        trusty_common::uds::send_framed_stream_request(
            &socket,
            &serde_json::json!({
                "jsonrpc": "2.0", "id": 7,
                "method": streams::METHOD_STATUS_STREAM,
                "params": null, "stream": true,
            }),
            GENEROUS,
        )
        .await
        .expect("open the status stream over a real socket");
    let first = tokio::time::timeout(GENEROUS, stream.next_frame())
        .await
        .expect("the first frame must arrive rather than hang")
        .expect("the stream must not end before its connected frame")
        .expect("the connected frame must not be an error");
    assert_eq!(first, serde_json::json!({ "type": "connected" }));
    assert!(
        state.events.receiver_count() > before,
        "the producer must be subscribed while the client is reading"
    );

    // The lane assertion, taken WHILE the stream is open: a permit held for the
    // stream's lifetime would already be visible here.
    let held = crate::service::concurrency::admit(&state.query_limiter)
        .await
        .expect("a stream must not hold the daemon's only admission permit");
    drop(held);

    // Hang up mid-stream. Everything after this is the server noticing.
    drop(stream);

    let mut settled = false;
    for _ in 0..400 {
        if state.events.receiver_count() == before {
            settled = true;
            break;
        }
        // The event the write fails on — the ticker's job on a live daemon.
        // `send` errors once the only subscriber is gone, which is the state
        // this loop is waiting for.
        let _ = state.events.send(DaemonEvent::IndexRegistered {
            id: "tick-6285".to_string(),
        });
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        settled,
        "the producer must drop its subscription once its write fails, not hold it \
         for the daemon's lifetime (subscribers: {} before, {} after)",
        before,
        state.events.receiver_count()
    );

    let held = crate::service::concurrency::admit(&state.query_limiter)
        .await
        .expect("no permit may be leaked by an abandoned stream");
    drop(held);

    let _ = stop_tx.send(());
    let _ = tokio::time::timeout(GENEROUS, served).await;
}
