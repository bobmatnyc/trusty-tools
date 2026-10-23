//! The webview's requests reach the `tcode` daemon over its Unix socket (#6637),
//! through the two in-process entry points the webview uses (#10166).
//!
//! Why: every `fetch()` in `ui/src` goes through the `tcode-bridge://` scheme
//! handler, and both event streams through the `open_daemon_stream` command.
//! These cases drive the REAL router, entered exactly that way, against a stub
//! daemon socket — the only way to prove the whole path: scheme dispatch or
//! stream pump, guard stack, route, mapping table, RPC exchange, and the SSE
//! encoding.
//!
//! What: one stub socket per case, a `build_router` pointed at it through
//! `BridgeState::with_socket`, unary requests sent through
//! `bridge::scheme::dispatch` and streams opened through `bridge::channel`,
//! and one assertion per behaviour the webview depends on — including each way
//! this bridge is allowed to fail.
//! Test: this file IS the test.

use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::http::{self, StatusCode};
use serde_json::{Value, json};
use trusty_code_gui::bridge::channel::{self, StreamEvent};
use trusty_code_gui::bridge::routes::{BridgeState, build_router};
use trusty_code_gui::bridge::scheme;
use trusty_common::server::DaemonAuth;

/// The credential every case presents, except the one that presents none.
const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef";

// ─── the stub daemon ─────────────────────────────────────────────────────────

/// Bind a socket that answers each framed request with `respond`'s frames.
///
/// `respond` returns the LINES to write back, so one case can answer a single
/// response frame and another a whole stream — the two shapes the bridge has to
/// tell apart. The same stub shape trusty-console's `tests/search_uds_bridge.rs`
/// uses, so both bridges are exercised against the same kind of daemon.
fn stub_daemon<F>(dir: &Path, respond: F) -> PathBuf
where
    F: Fn(&Value) -> Vec<String> + Send + Sync + 'static,
{
    let socket = dir.join("sockets").join("tcode.sock");
    let listener = trusty_common::uds::bind_hardened(&socket).expect("bind");
    let respond = std::sync::Arc::new(respond);
    tokio::spawn(async move {
        loop {
            let Ok((mut conn, _)) = listener.accept().await else {
                return;
            };
            let respond = std::sync::Arc::clone(&respond);
            tokio::spawn(async move {
                use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};
                // One request frame is one line; reading to EOF would block
                // forever on a client that keeps the connection open to read a
                // stream back.
                let (rx, mut tx) = conn.split();
                let mut reader = tokio::io::BufReader::new(rx);
                let mut raw = Vec::new();
                if reader.read_until(b'\n', &mut raw).await.is_err() {
                    return;
                }
                let request: Value = serde_json::from_slice(&raw).unwrap_or(Value::Null);
                for line in respond(&request) {
                    if tx.write_all(line.as_bytes()).await.is_err() {
                        return;
                    }
                    let _ = tx.write_all(b"\n").await;
                    let _ = tx.flush().await;
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

/// A router dialing `socket`, guarded by [`TOKEN`].
fn router_for(socket: PathBuf) -> (axum::Router, DaemonAuth) {
    let auth = DaemonAuth::new(TOKEN, Vec::<String>::new()).expect("a strong enough token");
    let state = BridgeState::new(auth.clone()).with_socket(socket);
    (build_router(state), auth)
}

/// One credentialed request through the real router.
async fn through_router(
    socket: PathBuf,
    method: &str,
    uri: &str,
    body: &str,
) -> (StatusCode, String, String) {
    let (router, _) = router_for(socket);
    send(router, method, uri, body, Some(TOKEN), None).await
}

/// One request through the scheme handler, with full control over the
/// credential and `Origin` header.
///
/// `uri` is a path; it is sent as `tcode-bridge://localhost{uri}`, the form the
/// webview's `fetch()` produces.
async fn send(
    router: axum::Router,
    method: &str,
    uri: &str,
    body: &str,
    token: Option<&str>,
    origin: Option<&str>,
) -> (StatusCode, String, String) {
    let (status, content_type, bytes) =
        send_bytes(router, method, uri, body.as_bytes().to_vec(), token, origin).await;
    (
        status,
        content_type,
        String::from_utf8_lossy(&bytes).to_string(),
    )
}

/// [`send`] with a raw body, for the body-limit case.
async fn send_bytes(
    router: axum::Router,
    method: &str,
    uri: &str,
    body: Vec<u8>,
    token: Option<&str>,
    origin: Option<&str>,
) -> (StatusCode, String, Vec<u8>) {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("{}{uri}", scheme::SCHEME_ORIGIN))
        .header("content-type", "application/json");
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    if let Some(origin) = origin {
        builder = builder.header("origin", origin);
    }
    let request = builder.body(body).expect("request");
    let resp = tokio::time::timeout(Duration::from_secs(30), scheme::dispatch(router, request))
        .await
        .expect("the scheme handler must answer");
    let content_type = resp
        .headers()
        .get(http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    (resp.status(), content_type, resp.into_body())
}

/// Mint a ticket for `path` through the scheme, as `daemon-auth.ts` does.
async fn mint(router: &axum::Router, path: &str) -> String {
    let (status, _, body) = send(
        router.clone(),
        "POST",
        &format!("/auth/sse-ticket?path={}", path.replace('/', "%2F")),
        "",
        Some(TOKEN),
        Some("tauri://localhost"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    serde_json::from_str::<Value>(&body).expect("json")["ticket"]
        .as_str()
        .expect("a ticket")
        .to_string()
}

/// Open `path` the way `open_daemon_stream` does and collect every event until
/// the stream's `End`.
async fn stream_events(router: axum::Router, path: &str) -> Vec<StreamEvent> {
    let ticket = mint(&router, path).await;
    let body = channel::open(router, path, &ticket)
        .await
        .expect("the stream must open");
    let mut seen = Vec::new();
    tokio::time::timeout(
        Duration::from_secs(10),
        channel::pump(body, |e| {
            seen.push(e);
            true
        }),
    )
    .await
    .expect("the stream must end");
    seen
}

/// The `data` of every message in `events`.
fn messages(events: &[StreamEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            StreamEvent::Message { data } => Some(data.clone()),
            StreamEvent::End { .. } => None,
        })
        .collect()
}

// ─── the unary path ──────────────────────────────────────────────────────────

/// Why: the webview parses the daemon's own documents. A body this bridge
/// re-shaped would be a second wire contract nobody wrote down.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn unary_route_returns_the_daemon_body_verbatim() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), |req| {
        assert_eq!(req["method"], "session.list", "mapped to the wrong method");
        vec![result_frame(json!({ "sessions": [{ "id": "s-1" }] }))]
    });
    let (status, content_type, body) = through_router(socket, "GET", "/sessions", "").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(content_type, "application/json");
    assert_eq!(
        serde_json::from_str::<Value>(&body).expect("json"),
        json!({ "sessions": [{ "id": "s-1" }] })
    );
}

/// Why: the daemon's REST layer answered `201` for a create and `202` for
/// `task.run`, and a caller that checks for one would break on a flat `200`.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_create_keeps_the_rest_layers_201() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), |_| {
        vec![result_frame(json!({ "id": "w-1", "name": "w" }))]
    });
    let (status, _, body) = through_router(socket, "POST", "/workstreams", r#"{"name":"w"}"#).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

/// Why: a refusal carries a code, and the webview branches on the status that
/// code stood for — a `404` is "this session is gone", anything else is an error
/// line the operator has to read.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_daemon_refusal_arrives_as_the_status_it_came_from() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), |_| {
        vec![error_frame(-32007, "no session 's-9'")]
    });
    let (status, _, body) = through_router(socket, "GET", "/sessions/s-9", "").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert!(
        body.contains("s-9"),
        "the daemon's words must reach the caller: {body}"
    );
}

/// Why (the fail-open check): a daemon that is not running must be
/// distinguishable from a healthy one with nothing to show. `502` with a reason
/// is that distinction; a `200` with an empty body would not be.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_dead_socket_is_a_bad_gateway_not_an_empty_success() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let (status, _, body) =
        through_router(tmp.path().join("absent.sock"), "GET", "/sessions", "").await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert!(
        body.contains("trusty-code"),
        "the failure must name the daemon: {body}"
    );
}

/// Why: an unmapped path must refuse loudly and name the gap, not answer an
/// approximate `502` that reads as "the daemon is down". `POST /rpc` is the
/// deliberate omission — see `bridge::map`'s docs.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn an_unmapped_path_is_not_implemented_and_names_itself() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), |_| vec![result_frame(json!({}))]);
    let (status, _, body) = through_router(socket, "POST", "/rpc", "{}").await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{body}");
    assert!(body.contains("/rpc"), "{body}");
}

/// Why (#6637): the Markdown transcript is the one route with no single RPC
/// twin — it makes two calls and renders. A `200 application/json` here would
/// save a JSON blob under a `.md` filename.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn the_markdown_route_answers_text_markdown() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), |req| {
        let result = match req["method"].as_str() {
            Some("session.get_transcript") => json!({
                "session_id": "s-1",
                "turns": [{
                    "role": "pm", "model": "opus", "text": "hello",
                    "tool_calls": ["bash"], "ran_test_command": false,
                }],
            }),
            _ => json!({ "id": "s-1", "task": "ship it", "status": "running" }),
        };
        vec![result_frame(result)]
    });
    let (status, content_type, body) =
        through_router(socket, "GET", "/sessions/s-1/transcript.md", "").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(content_type, "text/markdown; charset=utf-8");
    assert!(
        body.starts_with("# Workstream transcript — ship it"),
        "{body}"
    );
    assert!(body.contains("- `PM` ran: bash\n"), "{body}");
}

/// Why: an unknown session must `404` through the Markdown route exactly as it
/// does through the JSON one — it is the same refusal from the same call.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn the_markdown_route_404s_an_unknown_session() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), |_| {
        vec![error_frame(-32007, "no session 's-9'")]
    });
    let (status, _, body) = through_router(socket, "GET", "/sessions/s-9/transcript.md", "").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

/// Why (the fail-open check): a body over the socket's frame budget must be
/// refused before it is framed, with a status — not truncated and forwarded,
/// and not left unanswered.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn an_oversized_body_is_refused_before_it_reaches_the_daemon() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), |_| {
        panic!("an oversized body must never reach the daemon")
    });
    let (router, _) = router_for(socket);
    let too_big = vec![b' '; trusty_common::uds::MAX_FRAME_BYTES as usize + 1];
    let (status, _, _) = send_bytes(
        router,
        "POST",
        "/workstreams",
        too_big,
        Some(TOKEN),
        Some("tauri://localhost"),
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
}

// ─── the two event streams ───────────────────────────────────────────────────

/// Why: the activity pane replays a session's ring buffer and then tails it, and
/// every frame has to reach the webview as the `data:` payload the daemon's own
/// SSE route wrote — the webview parses the envelope, not a re-shaped wrapper.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_stream_reaches_the_webview_frame_for_frame() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), |req| {
        assert_eq!(req["method"], "session.events");
        assert_eq!(req["stream"], json!(true), "the stream flag must be set");
        assert_eq!(req["params"]["session_id"], "s-1");
        vec![
            item_frame(json!({ "seq": 1, "kind": "turn" })),
            item_frame(json!({ "seq": 2, "kind": "turn" })),
            end_frame(),
        ]
    });
    let (router, _) = router_for(socket);
    let events = stream_events(router, "/sessions/s-1/events").await;
    assert_eq!(
        events,
        vec![
            StreamEvent::Message {
                data: "{\"kind\":\"turn\",\"seq\":1}".to_string()
            },
            StreamEvent::Message {
                data: "{\"kind\":\"turn\",\"seq\":2}".to_string()
            },
            StreamEvent::End { error: None },
        ],
        "each item must be exactly one message, then one end"
    );
}

/// Why (the fail-open check): a stream that dies mid-tail must say so. The
/// webview reads the channel's `end` as a drop to reconnect through, and the
/// error event before it is what tells a broken tail from a finished one.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_mid_stream_failure_becomes_an_error_event() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), |_| {
        vec![
            item_frame(json!({ "seq": 1 })),
            stream_error_frame(-32603, "the producer went away"),
        ]
    });
    let (router, _) = router_for(socket);
    let events = stream_events(router, "/sessions/s-1/events").await;
    let data = messages(&events);
    assert_eq!(data[0], "{\"seq\":1}", "{events:?}");
    assert!(
        data[1].contains(r#""type":"error""#) && data[1].contains("the producer went away"),
        "a truncated stream must carry a terminal error event: {events:?}"
    );
    assert!(
        matches!(events.last(), Some(StreamEvent::End { .. })),
        "the stream must end, not hang: {events:?}"
    );
}

/// Why: the daemon closing the socket with no `end` frame is the other way a
/// stream drops midway, and it must not read as a clean finish either.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_daemon_that_hangs_up_midway_is_an_error_event_then_end() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), |_| vec![item_frame(json!({ "seq": 1 }))]);
    let (router, _) = router_for(socket);
    let events = stream_events(router, "/sessions/s-1/events").await;
    let data = messages(&events);
    assert_eq!(data[0], "{\"seq\":1}", "{events:?}");
    assert!(
        data.iter().any(|d| d.contains(r#""type":"error""#)),
        "a hang-up without an end frame must surface as an error: {events:?}"
    );
    assert!(matches!(events.last(), Some(StreamEvent::End { .. })));
}

/// Why: `workstream.events` is a pure live tail — a healthy stream on a quiet
/// workstream sends NOTHING until something happens. A bridge that read the
/// first frame before answering would hold the open for as long as that took.
/// Measured on a real `tcode serve`: `curl` on a freshly created workstream got
/// zero bytes and no status line. This is the arm that pins the fix.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_healthy_but_silent_stream_opens_immediately() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    // Accepts the open, then says nothing at all — the quiet-workstream case.
    let socket = stub_daemon(tmp.path(), |req| {
        assert_eq!(req["method"], "workstream.events");
        Vec::new()
    });
    let (router, _) = router_for(socket);
    let ticket = mint(&router, "/workstreams/w-1/events").await;
    let _stream = tokio::time::timeout(
        Duration::from_secs(5),
        channel::open(router, "/workstreams/w-1/events", &ticket),
    )
    .await
    .expect("the open must not wait for a first frame")
    .expect("a silent stream is a healthy one");
}

/// Why: a streaming method can refuse AT OPEN — `workstream.events` answers
/// `not_found` for an unknown id — and the socket contract sends that as the
/// stream's first `"stream":"error"` frame. The open has already succeeded by
/// then (see the test above for why it has to), so the refusal reaches the
/// webview as the terminal error event `sse_tail` renders. What must NOT happen
/// is a silent close.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_stream_refusal_at_open_becomes_a_terminal_error_event() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), |req| {
        assert_eq!(req["method"], "workstream.events");
        vec![stream_error_frame(-32002, "no workstream 'w-9'")]
    });
    let (router, _) = router_for(socket);
    let events = stream_events(router, "/workstreams/w-9/events").await;
    let data = messages(&events);
    assert!(
        data.iter()
            .any(|d| d.contains(r#""type":"error""#) && d.contains("w-9")),
        "the refusal must not close silently: {events:?}"
    );
}

/// Why (the fail-open check): a stream whose daemon socket is absent must fail
/// to open with the gateway status, not open as an empty stream that reads as a
/// quiet one.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_stream_on_a_dead_socket_does_not_open() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let (router, _) = router_for(tmp.path().join("absent.sock"));
    let ticket = mint(&router, "/sessions/s-1/events").await;
    let err = channel::open(router, "/sessions/s-1/events", &ticket)
        .await
        .expect_err("an absent socket must not open");
    assert!(err.contains("502"), "{err}");
}

/// Why: a scheme response is one buffered body and a stream never ends. A
/// stream path fetched over the scheme must be refused promptly, or the
/// webview's `fetch()` would hang for the stream's life.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_stream_fetched_over_the_scheme_is_refused_not_hung() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), |_| Vec::new());
    let (router, _) = router_for(socket);
    let (status, _, body) = send(
        router,
        "GET",
        "/workstreams/w-1/events",
        "",
        Some(TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("open_daemon_stream"), "{body}");
}

// ─── the guard stack ─────────────────────────────────────────────────────────

/// Why (the fail-open check): the per-launch token is kept as defence in depth
/// (owner ruling Q3, #10166) — any other content that can reach the scheme must
/// still present it, so an unauthenticated request reaches no daemon method.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_request_without_the_token_is_unauthorized() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), |_| {
        panic!("an unauthenticated request must never reach the daemon")
    });
    let (router, _) = router_for(socket);
    let (status, _, _) = send(router, "GET", "/sessions", "", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// Why: a wrong credential must be as unusable as none — and must not be
/// distinguishable from none, which would tell a prober it had the right shape.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_wrong_token_is_unauthorized_too() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), |_| vec![result_frame(json!({}))]);
    let (router, _) = router_for(socket);
    let (status, _, _) = send(
        router,
        "GET",
        "/sessions",
        "",
        Some(&"f".repeat(TOKEN.len())),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// Why (the fail-open check): a request carrying a foreign `Origin` — content
/// from anywhere but this app's webview — must be refused by the origin guard
/// BEFORE the credential is even considered, which is what makes a
/// cross-origin write a `403` rather than a `401` a page could go and try to
/// satisfy.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_cross_origin_write_is_rejected() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), |_| {
        panic!("a cross-origin write must never reach the daemon")
    });
    let (router, _) = router_for(socket);
    let (status, _, body) = send(
        router,
        "POST",
        "/workstreams",
        r#"{"name":"w"}"#,
        Some(TOKEN),
        Some("https://attacker.example"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

/// Why: the packaged app's webview presents a synthetic origin, not an
/// `http://127.0.0.1` one. A guard that only knew loopback would `403` the app's
/// own writes — which is the whole app, since every action is a write.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn the_tauri_webviews_own_origin_is_allowed() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), |_| vec![result_frame(json!({ "id": "w-1" }))]);
    let (router, _) = router_for(socket);
    let (status, _, body) = send(
        router,
        "POST",
        "/workstreams",
        r#"{"name":"w"}"#,
        Some(TOKEN),
        Some("tauri://localhost"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

// ─── the SSE ticket ──────────────────────────────────────────────────────────

/// Why: the stream command opens a guarded stream with a ticket minted over the
/// scheme (owner ruling Q3 keeps the flow). Single use is what keeps a ticket
/// read from a log worthless, and both entry points must share one ticket table.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_ticket_opens_one_stream_then_is_spent() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), |_| {
        vec![item_frame(json!({ "seq": 1 })), end_frame()]
    });
    let (router, _) = router_for(socket);

    let ticket = mint(&router, "/sessions/s-1/events").await;
    let _stream = channel::open(router.clone(), "/sessions/s-1/events", &ticket)
        .await
        .expect("the ticket must open the stream");

    let err = channel::open(router, "/sessions/s-1/events", &ticket)
        .await
        .expect_err("a spent ticket must not open a second stream");
    assert!(err.contains("401"), "{err}");
}

/// Why: a ticket is bound to the path it was minted for; presenting it for a
/// different stream must not open that one.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_ticket_does_not_open_another_stream() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), |_| vec![end_frame()]);
    let (router, _) = router_for(socket);
    let ticket = mint(&router, "/sessions/s-1/events").await;
    let err = channel::open(router, "/sessions/s-2/events", &ticket)
        .await
        .expect_err("a ticket for s-1 must not open s-2");
    assert!(err.contains("401"), "{err}");
}

/// Why: a ticket minted for an arbitrary path would buy one arbitrary
/// authenticated request, which is exactly what binding it to a stream removes.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_ticket_cannot_be_minted_for_a_non_stream_path() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), |_| vec![result_frame(json!({}))]);
    let (router, _) = router_for(socket);
    for path in ["%2Frpc", "%2Fsessions", "%2Fsessions%2Fs-1%2Ftranscript"] {
        let (status, _, body) = send(
            router.clone(),
            "POST",
            &format!("/auth/sse-ticket?path={path}"),
            "",
            Some(TOKEN),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}: {body}");
    }
}

/// Why: minting is itself a guarded action. A ticket route that answered an
/// anonymous caller would be a way IN rather than a way to carry an existing
/// right onto a URL.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn minting_a_ticket_requires_the_token() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), |_| vec![result_frame(json!({}))]);
    let (router, _) = router_for(socket);
    let (status, _, _) = send(
        router,
        "POST",
        "/auth/sse-ticket?path=%2Fsessions%2Fs-1%2Fevents",
        "",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
