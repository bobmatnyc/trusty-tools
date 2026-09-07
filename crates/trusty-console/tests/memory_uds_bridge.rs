//! `/api/memory/*` reaches trusty-memory over its Unix socket (#6155).
//!
//! Why: the memory dashboard moved into this binary, and its every API call
//! resolves to `/api/memory/…`. trusty-memory has no HTTP surface since #6286,
//! so that prefix has to reach the daemon over a socket instead — and the SPA
//! must not be able to tell. These cases drive the REAL router against a stub
//! daemon socket, which is the only way to prove the whole path: route, mapping
//! table, RPC exchange, and the SSE bridge.
//!
//! What: one stub socket per case, a `build_router` pointed at it through
//! `AppState::with_memory_socket`, and one assertion per behaviour the dashboard
//! depends on.
//! Test: this file IS the test.

use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use trusty_console::server::{AppState, build_router};

// ─── the stub daemon ─────────────────────────────────────────────────────────

/// Bind a socket that answers each framed request with `respond`'s frames.
///
/// `respond` returns the LINES to write back, so one case can answer a single
/// response frame and another a whole stream — the two shapes the bridge has to
/// tell apart.
fn stub_daemon<F>(dir: &Path, respond: F) -> PathBuf
where
    F: Fn(&Value) -> Vec<String> + Send + Sync + 'static,
{
    let socket = dir.join("sockets").join("memory.sock");
    let listener = trusty_common::uds::bind_hardened(&socket).expect("bind");
    let respond = std::sync::Arc::new(respond);
    tokio::spawn(async move {
        loop {
            let Ok((mut conn, _)) = listener.accept().await else {
                return;
            };
            let respond = std::sync::Arc::clone(&respond);
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
                let mut raw = Vec::new();
                let _ = conn.read_to_end(&mut raw).await;
                let request: Value = serde_json::from_slice(&raw).unwrap_or(Value::Null);
                for line in respond(&request) {
                    if conn.write_all(line.as_bytes()).await.is_err() {
                        return;
                    }
                    let _ = conn.write_all(b"\n").await;
                    let _ = conn.flush().await;
                }
            });
        }
    });
    socket
}

/// One ordinary response frame carrying `result`.
fn result_frame(result: Value) -> String {
    json!({ "jsonrpc": "2.0", "id": 1, "result": result }).to_string()
}

/// One ordinary response frame carrying a JSON-RPC `error`.
fn error_frame(code: i64, message: &str) -> String {
    json!({ "jsonrpc": "2.0", "id": 1, "error": { "code": code, "message": message } }).to_string()
}

/// One `"stream":"item"` frame.
fn item_frame(result: Value) -> String {
    json!({ "jsonrpc": "2.0", "id": 1, "stream": "item", "result": result }).to_string()
}

/// The terminal `"stream":"end"` frame.
fn end_frame() -> String {
    json!({ "jsonrpc": "2.0", "id": 1, "stream": "end" }).to_string()
}

/// The terminal `"stream":"error"` frame.
fn stream_error_frame(code: i64, message: &str) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "stream": "error",
        "error": { "code": code, "message": message },
    })
    .to_string()
}

// ─── driving the router ──────────────────────────────────────────────────────

/// Send one request through the real router with memory pointed at `socket`.
async fn through_router(socket: PathBuf, method: &str, uri: &str) -> (StatusCode, String, String) {
    let router = build_router(AppState::new(vec![]).with_memory_socket(socket));
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::empty())
        .expect("request");
    let resp = router.oneshot(request).await.expect("response");
    let status = resp.status();
    let content_type = resp
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    (
        status,
        content_type,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

// ─── unary ───────────────────────────────────────────────────────────────────

/// Why: this is the whole dashboard. `GET /api/memory/health` used to be a plain
/// HTTP `/health` on the daemon; it now has to become one `memory.health` call
/// whose result reaches the browser unchanged, or the SPA renders a connection
/// error against a running daemon.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn unary_route_returns_the_daemon_body_verbatim() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), |request| {
        vec![result_frame(json!({
            "status": "ok",
            "version": "0.26.0",
            "method_seen": request["method"].clone(),
        }))]
    });

    let (status, content_type, body) = through_router(socket, "GET", "/api/memory/health").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(content_type.contains("application/json"), "{content_type}");
    let parsed: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(parsed["version"], json!("0.26.0"));
    assert_eq!(parsed["method_seen"], json!("memory.health"));
}

/// Why: the palace id used to be a path segment and is a `params` field on this
/// wire. A row that forgot the rename would send `{}` and the daemon would
/// answer `invalid_params` for every palace the SPA opened.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_path_segment_reaches_the_daemon_as_a_params_field() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), |request| {
        vec![result_frame(json!({
            "method_seen": request["method"].clone(),
            "palace_seen": request["params"]["palace_id"].clone(),
        }))]
    });

    let (status, _, body) = through_router(socket, "GET", "/api/memory/api/v1/palaces/izzie").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let parsed: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(parsed["method_seen"], json!("memory.palace_get"));
    assert_eq!(parsed["palace_seen"], json!("izzie"));
}

/// Why: a query string is text while the RPC params are typed. A `"200"` string
/// where `limit: usize` is expected answers `invalid_params`, and the KG
/// explorer's subject list renders empty against a populated palace.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_query_parameter_reaches_the_daemon_as_its_own_type() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), |request| {
        vec![result_frame(json!({
            "limit": request["params"]["limit"].clone(),
            "method_seen": request["method"].clone(),
        }))]
    });

    let (status, _, body) = through_router(
        socket,
        "GET",
        "/api/memory/api/v1/palaces/izzie/kg/subjects_with_counts?limit=200",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let parsed: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(parsed["limit"], json!(200), "must arrive as a number");
    assert_eq!(
        parsed["method_seen"],
        json!("memory.kg_subjects_with_counts")
    );
}

/// Why: `kg?subject=` is the one row that reaches the dispatcher's tool surface
/// by raw name rather than a folded `memory.*` method, and its argument is
/// spelled `palace`, not `palace_id`. Getting either wrong is a
/// `method_not_found` or an `invalid_params` behind a panel that renders a toast.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn the_kg_subject_read_reaches_the_tool_dispatcher_by_raw_name() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), |request| {
        vec![result_frame(json!({
            "method_seen": request["method"].clone(),
            "palace_seen": request["params"]["palace"].clone(),
            "subject_seen": request["params"]["subject"].clone(),
        }))]
    });

    let (status, _, body) = through_router(
        socket,
        "GET",
        "/api/memory/api/v1/palaces/izzie/kg?subject=tag%3Arust",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let parsed: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(parsed["method_seen"], json!("kg_query"));
    assert_eq!(parsed["palace_seen"], json!("izzie"));
    assert_eq!(parsed["subject_seen"], json!("tag:rust"));
}

/// Why: the dashboard's two writes carry no body, and a POST that arrived as
/// `params: null` would answer `invalid_params` — the empty OBJECT is what every
/// param type accepts.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_write_with_no_body_sends_an_empty_params_object() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), |request| {
        vec![result_frame(json!({
            "method_seen": request["method"].clone(),
            "params_is_object": request["params"].is_object(),
        }))]
    });

    let (status, _, body) = through_router(socket, "POST", "/api/memory/api/v1/dream/run").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let parsed: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(parsed["method_seen"], json!("memory.dream_run"));
    assert_eq!(parsed["params_is_object"], json!(true));
}

/// Why: the fail-open branch the bridge exists to close. A daemon that is not
/// running must reach the browser as a gateway failure, never as an empty
/// success that renders a healthy dashboard for a dead daemon.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_dead_socket_is_a_bad_gateway_not_an_empty_success() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let (status, _, body) = through_router(
        tmp.path().join("absent.sock"),
        "GET",
        "/api/memory/api/v1/status",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    let parsed: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(parsed["service"], json!("trusty-memory"));
    assert!(
        parsed["error"]
            .as_str()
            .unwrap_or_default()
            .contains("memory.status"),
        "the failure must name the method it was making: {body}"
    );
}

/// Why: a refusal has to arrive as the status the same refusal carried over
/// HTTP, or the SPA cannot tell "no such palace" from "the daemon is broken".
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_refusal_carries_the_status_it_came_from() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), |_| {
        vec![error_frame(-32004, "no palace 'ghost'")]
    });

    let (status, _, body) = through_router(socket, "GET", "/api/memory/api/v1/palaces/ghost").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert!(body.contains("no palace 'ghost'"), "{body}");
}

/// Why: a path this table does not name must be refused with a body an operator
/// can act on, rather than forwarded somewhere approximate.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn an_unmapped_path_is_not_implemented_and_names_itself() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), |_| vec![result_frame(json!({}))]);

    let (status, _, body) =
        through_router(socket, "DELETE", "/api/memory/api/v1/palaces/izzie").await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{body}");
    assert!(body.contains("api/v1/palaces/izzie"), "{body}");
    assert!(body.contains("trusty-memory"), "{body}");
}

/// Why: this SPA's own `base.js` documents `/proxy/memory/` as a supported
/// mount, and #6286 deleted the proxy row that answered it. The alias keeps a
/// pre-rename caller working instead of giving it `400 unknown daemon`.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn deprecated_alias_reaches_the_same_handler() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), |request| {
        vec![result_frame(
            json!({ "method_seen": request["method"].clone() }),
        )]
    });

    let (status, _, body) = through_router(socket, "GET", "/proxy/memory/health").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let parsed: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(parsed["method_seen"], json!("memory.health"));
}

// ─── streaming ───────────────────────────────────────────────────────────────

/// Why: `/sse` was the daemon's broadcast route and `ActivityFeed.svelte` still
/// opens it. Every frame has to reach the browser as the `data:` line the old
/// route wrote, or the live feed renders nothing against a busy daemon.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_stream_reaches_the_browser_frame_for_frame() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), |request| {
        vec![
            item_frame(json!({ "type": "connected", "method": request["method"].clone() })),
            item_frame(json!({ "type": "Remembered", "palace": "izzie" })),
            end_frame(),
        ]
    });

    let (status, content_type, body) = through_router(socket, "GET", "/api/memory/sse").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(content_type.contains("text/event-stream"), "{content_type}");
    assert!(
        body.contains(r#"data: {"method":"memory.activity_stream","type":"connected"}"#),
        "{body}"
    );
    assert!(
        body.contains(r#"data: {"palace":"izzie","type":"Remembered"}"#),
        "{body}"
    );
    assert_eq!(
        body.matches("data: ").count(),
        2,
        "exactly two events: {body}"
    );
}

/// Why: a refusal arrives as the stream's FIRST frame, after the dial already
/// succeeded. Committing to `200 text/event-stream` before reading it would turn
/// every refusal into an empty stream, which the feed reads as "nothing
/// happened".
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_stream_refusal_before_the_first_item_is_an_http_status() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), |_| {
        vec![stream_error_frame(-32004, "no activity source")]
    });

    let (status, content_type, body) = through_router(socket, "GET", "/api/memory/sse").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert!(
        !content_type.contains("text/event-stream"),
        "a refusal must not be a 200 stream: {content_type}"
    );
    assert!(body.contains("no activity source"), "{body}");
}

/// Why: a mid-stream failure must not close the body silently — the SPA reads a
/// closed feed as one that ended normally, so a broken subscription would look
/// like a quiet daemon.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_mid_stream_failure_becomes_an_error_event() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), |_| {
        vec![
            item_frame(json!({ "type": "connected" })),
            stream_error_frame(-32603, "activity store went away"),
        ]
    });

    let (status, _, body) = through_router(socket, "GET", "/api/memory/sse").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains(r#""type":"error""#), "{body}");
    assert!(body.contains("activity store went away"), "{body}");
}

/// Why: the peek must not turn a legitimately empty stream into a failure. An
/// immediately-closed event stream is what the browser got for a daemon with
/// nothing to replay.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn an_empty_stream_is_an_immediately_closed_event_stream() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), |_| vec![end_frame()]);

    let (status, content_type, body) = through_router(socket, "GET", "/api/memory/sse").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(content_type.contains("text/event-stream"), "{content_type}");
    assert!(body.is_empty(), "no events, no bytes: {body:?}");
}

/// Why: `memory.activity_stream` sends no opener — it forwards the daemon's
/// event broadcast and nothing else — so on an idle daemon the first frame is
/// minutes away. Peeking it under the 60-second OPEN budget held the response
/// head for that long against a healthy daemon, and the browser showed a feed
/// that never connected. Observed live on 2026-09-07: `curl /api/memory/sse`
/// returned no status inside two seconds. The peek gets its own short budget and
/// its expiry commits to `200`, which is what the retired `/sse` route did.
/// What: a daemon that accepts the subscription and stays silent past the peek
/// window, then closes. The head must arrive well inside the open budget.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn an_idle_stream_still_gets_its_response_head() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = silent_stub_daemon(tmp.path(), std::time::Duration::from_secs(4));

    let started = std::time::Instant::now();
    let (status, content_type, body) = through_router(socket, "GET", "/api/memory/sse").await;
    let elapsed = started.elapsed();

    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(content_type.contains("text/event-stream"), "{content_type}");
    assert!(
        elapsed < std::time::Duration::from_secs(30),
        "the head must not wait out the 60s open budget; took {elapsed:?}"
    );
}

/// Bind a socket that accepts, stays silent for `quiet`, then closes.
///
/// The shape an idle event feed has: the subscription succeeded and there is
/// simply nothing to send yet.
fn silent_stub_daemon(dir: &Path, quiet: std::time::Duration) -> PathBuf {
    let socket = dir.join("sockets").join("memory-idle.sock");
    let listener = trusty_common::uds::bind_hardened(&socket).expect("bind");
    tokio::spawn(async move {
        let Ok((mut conn, _)) = listener.accept().await else {
            return;
        };
        use tokio::io::AsyncReadExt as _;
        let mut raw = vec![0u8; 4096];
        let _ = conn.read(&mut raw).await;
        tokio::time::sleep(quiet).await;
    });
    socket
}

/// Why: the peek's expiry commits to `200` before the daemon has said anything,
/// so a refusal that arrives AFTER it can no longer become an HTTP status. The
/// `STREAM_FIRST_FRAME_PEEK` doc claims that refusal still reaches the operator,
/// as the `{"type":"error"}` event `uds_sse` writes for a terminal frame — and
/// nothing asserted it. Every other stream case here answers immediately, leads
/// with an item, or answers nothing at all; this is the one that crosses the
/// window with a refusal behind it. Without the commit-on-expiry arm the request
/// fails outright and the browser sees no stream and no event.
/// What: a daemon that accepts the subscription, stays silent past the peek
/// window, then sends one terminal error frame and closes. The head must be a
/// `200` event stream and the body must carry the daemon's refusal.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_refusal_after_the_peek_window_arrives_as_an_error_event() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    // Just past the 2 s peek — long enough to cross it, short enough to keep the
    // suite's wall time honest.
    let socket = late_error_stub_daemon(
        tmp.path(),
        std::time::Duration::from_millis(2_500),
        stream_error_frame(-32004, "activity source went away"),
    );

    let (status, content_type, body) = through_router(socket, "GET", "/api/memory/sse").await;

    assert_eq!(
        status,
        StatusCode::OK,
        "the head is already committed when the refusal lands: {body}"
    );
    assert!(content_type.contains("text/event-stream"), "{content_type}");
    assert!(
        body.contains(r#""type":"error""#),
        "a late refusal must reach the browser as an error event, not a silent close: {body:?}"
    );
    assert!(body.contains("activity source went away"), "{body:?}");
}

/// Bind a socket that accepts, stays silent for `quiet`, then writes one frame.
///
/// The shape a refusal the daemon takes its time over has: the subscription is
/// accepted, the peek window passes with nothing buffered, and only then does
/// the daemon say no.
fn late_error_stub_daemon(dir: &Path, quiet: std::time::Duration, frame: String) -> PathBuf {
    let socket = dir.join("sockets").join("memory-late-error.sock");
    let listener = trusty_common::uds::bind_hardened(&socket).expect("bind");
    tokio::spawn(async move {
        let Ok((mut conn, _)) = listener.accept().await else {
            return;
        };
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        let mut raw = vec![0u8; 4096];
        let _ = conn.read(&mut raw).await;
        tokio::time::sleep(quiet).await;
        let _ = conn.write_all(frame.as_bytes()).await;
        let _ = conn.write_all(b"\n").await;
        let _ = conn.flush().await;
    });
    socket
}
