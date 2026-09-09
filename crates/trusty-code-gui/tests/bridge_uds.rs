//! The webview's requests reach the `tcode` daemon over its Unix socket (#6637).
//!
//! Why: the daemon's TCP listener is what PR 2c deletes, and every `fetch()` and
//! `EventSource` in `ui/src` now points at this bridge instead. These cases
//! drive the REAL router against a stub daemon socket, which is the only way to
//! prove the whole path: guard stack, route, mapping table, RPC exchange, and
//! the SSE bridge.
//!
//! What: one stub socket per case, a `build_router` pointed at it through
//! `BridgeState::with_socket`, and one assertion per behaviour the webview
//! depends on — including the four ways this bridge is allowed to fail.
//! Test: this file IS the test.

use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use trusty_code_gui::bridge::routes::{BridgeState, build_router};
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

/// One request with full control over the credential and `Origin` header.
async fn send(
    router: axum::Router,
    method: &str,
    uri: &str,
    body: &str,
    token: Option<&str>,
    origin: Option<&str>,
) -> (StatusCode, String, String) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    if let Some(origin) = origin {
        builder = builder.header("origin", origin);
    }
    let request = builder.body(Body::from(body.to_string())).expect("request");
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
        String::from_utf8_lossy(&bytes).to_string(),
    )
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

// ─── the two event streams ───────────────────────────────────────────────────

/// Why: the activity pane replays a session's ring buffer and then tails it, and
/// every frame has to reach the browser as the `data:` line the daemon's own SSE
/// route wrote — the webview parses the envelope, not a re-shaped wrapper.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_stream_reaches_the_browser_frame_for_frame() {
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
    let (status, content_type, body) =
        through_router(socket, "GET", "/sessions/s-1/events", "").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(content_type, "text/event-stream");
    assert_eq!(
        body, "data: {\"kind\":\"turn\",\"seq\":1}\n\ndata: {\"kind\":\"turn\",\"seq\":2}\n\n",
        "each item must be exactly one SSE frame"
    );
}

/// Why (the fail-open check): a stream that dies mid-tail must say so. The
/// webview reads a CLOSED stream as a drop to reconnect through and a closed one
/// carrying no error as a stream that simply ended — so a silent close would
/// report a broken tail as a finished one.
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
    let (status, _, body) = through_router(socket, "GET", "/sessions/s-1/events", "").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the head was already written: {body}"
    );
    assert!(body.starts_with("data: {\"seq\":1}\n\n"), "{body}");
    assert!(
        body.contains(r#""type":"error""#),
        "a truncated stream must carry a terminal error event: {body}"
    );
    assert!(body.contains("the producer went away"), "{body}");
}

/// Why: `workstream.events` is a pure live tail — a healthy stream on a quiet
/// workstream sends NOTHING until something happens. A bridge that read the
/// first frame before choosing a status would withhold the response head for as
/// long as that took, and the webview's `EventSource` reads a head that never
/// arrives as a connection that never opened, then reconnect-loops. Measured on
/// a real `tcode serve`: `curl` on a freshly created workstream got zero bytes
/// and no status line. This is the arm that pins the fix.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_healthy_but_silent_stream_gets_its_head_immediately() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    // Accepts the open, then says nothing at all — the quiet-workstream case.
    let socket = stub_daemon(tmp.path(), |req| {
        assert_eq!(req["method"], "workstream.events");
        Vec::new()
    });
    let (router, _) = router_for(socket);
    let request = Request::builder()
        .method("GET")
        .uri("/workstreams/w-1/events")
        .header("authorization", format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .expect("request");

    // The HEAD must arrive without waiting for a first frame. Only the head is
    // awaited here; collecting the body would wait out the (silent) stream.
    let resp = tokio::time::timeout(std::time::Duration::from_secs(5), router.oneshot(request))
        .await
        .expect("the response head must not wait for a first frame")
        .expect("response");

    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/event-stream")
    );
}

/// Why: a streaming method can refuse AT OPEN — `workstream.events` answers
/// `not_found` for an unknown id — and the socket contract sends that as the
/// stream's first `"stream":"error"` frame rather than as an ordinary response.
/// Since the head is already written by then (see the test above for why it has
/// to be), the refusal reaches the browser as the terminal error event
/// `sse_tail` renders. What must NOT happen is a silent close: the webview would
/// read that as a tail that simply ended.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_stream_refusal_at_open_becomes_a_terminal_error_event() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), |req| {
        assert_eq!(req["method"], "workstream.events");
        vec![stream_error_frame(-32002, "no workstream 'w-9'")]
    });
    let (status, content_type, body) =
        through_router(socket, "GET", "/workstreams/w-9/events", "").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(content_type, "text/event-stream", "{body}");
    assert!(
        body.contains(r#""type":"error""#),
        "the refusal must not close silently: {body}"
    );
    assert!(body.contains("w-9"), "{body}");
}

// ─── the guard stack ─────────────────────────────────────────────────────────

/// Why (the fail-open check): the bridge is a TCP listener on loopback, so any
/// local process can reach it. The per-launch token is the whole of what stops
/// one — an unauthenticated request must reach no daemon method at all.
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

/// Why (the fail-open check): a page the operator visits in an ordinary browser
/// can POST at loopback. The credential stops it, but the origin guard is the
/// layer that stops it BEFORE the credential is even considered — which is what
/// makes a cross-origin write a `403` rather than a `401` a page could go and
/// try to satisfy.
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

/// Why: `EventSource` cannot send a header, so a ticket in the query string is
/// the only way it opens a guarded stream. Single use is what keeps a ticket
/// read from a log worthless.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_ticket_opens_one_stream_then_is_spent() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), |_| {
        vec![item_frame(json!({ "seq": 1 })), end_frame()]
    });
    let (router, _) = router_for(socket);

    let (status, _, body) = send(
        router.clone(),
        "POST",
        "/auth/sse-ticket?path=%2Fsessions%2Fs-1%2Fevents",
        "",
        Some(TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let ticket = serde_json::from_str::<Value>(&body).expect("json")["ticket"]
        .as_str()
        .expect("a ticket")
        .to_string();

    let uri = format!("/sessions/s-1/events?ticket={ticket}");
    let (first, content_type, _) = send(router.clone(), "GET", &uri, "", None, None).await;
    assert_eq!(first, StatusCode::OK, "the ticket must open the stream");
    assert_eq!(content_type, "text/event-stream");

    let (second, _, _) = send(router, "GET", &uri, "", None, None).await;
    assert_eq!(
        second,
        StatusCode::UNAUTHORIZED,
        "a spent ticket must not open a second stream"
    );
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
