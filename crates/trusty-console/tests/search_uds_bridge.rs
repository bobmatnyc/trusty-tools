//! `/api/search/*` reaches trusty-search over its Unix socket (#6285).
//!
//! Why: #6384 moved the trusty-search dashboard into this binary, and its every
//! API call resolves to `/api/search/…`. #6285 takes trusty-search's HTTP
//! surface away, so that prefix has to reach the daemon over a socket instead —
//! and the SPA must not be able to tell. These cases drive the REAL router
//! against a stub daemon socket, which is the only way to prove the whole path:
//! route, mapping table, RPC exchange, and the SSE bridge.
//!
//! What: one stub socket per case, a `build_router` pointed at it through
//! `AppState::with_search_socket`, and one assertion per behaviour the dashboard
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
    let socket = dir.join("sockets").join("search.sock");
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

/// Send one request through the real router with search pointed at `socket`.
async fn through_router(
    socket: PathBuf,
    method: &str,
    uri: &str,
    body: &str,
) -> (StatusCode, String, String) {
    let router = build_router(AppState::new(vec![]).with_search_socket(socket));
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
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

/// Why: this is the whole dashboard. `GET /api/search/health` used to be
/// forwarded verbatim to the daemon's HTTP `/health`; it now has to become one
/// `search.health` call whose result reaches the browser unchanged, or the SPA
/// renders an offline badge against a running daemon.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn unary_route_returns_the_daemon_body_verbatim() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), |request| {
        vec![result_frame(json!({
            "status": "ok",
            "version": "0.49.6",
            "method_seen": request["method"].clone(),
        }))]
    });

    let (status, content_type, body) =
        through_router(socket, "GET", "/api/search/health", "").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(content_type.contains("application/json"), "{content_type}");
    let parsed: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(parsed["version"], json!("0.49.6"));
    assert_eq!(parsed["method_seen"], json!("search.health"));
}

/// Why: the index roster carries `?details=true`, and a query string is text
/// while the RPC params are typed. A `"true"` string there answers
/// `invalid_params` and the roster renders empty.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_query_parameter_reaches_the_daemon_as_its_own_type() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), |request| {
        vec![result_frame(json!({
            "details": request["params"]["details"].clone(),
            "method_seen": request["method"].clone(),
        }))]
    });

    let (status, _, body) =
        through_router(socket, "GET", "/api/search/indexes?details=true", "").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let parsed: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(parsed["details"], json!(true), "must arrive as a bool");
    assert_eq!(parsed["method_seen"], json!("search.indexes.list"));
}

/// Why: a POST body has to arrive nested under the key the RPC params expect,
/// or every search from the dashboard is refused.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_post_body_reaches_the_daemon_under_the_params_it_expects() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), |request| {
        vec![result_frame(json!({
            "params": request["params"].clone(),
            "method_seen": request["method"].clone(),
        }))]
    });

    let (status, _, body) = through_router(
        socket,
        "POST",
        "/api/search/indexes/scratch/search",
        r#"{"text":"hello","top_k":5}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let parsed: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(parsed["method_seen"], json!("search.query"));
    assert_eq!(parsed["params"]["index_id"], json!("scratch"));
    assert_eq!(parsed["params"]["body"]["text"], json!("hello"));
}

/// Why: the fail-open branch. A daemon that refuses must not reach the browser
/// as a `200` carrying an error-shaped body — the SPA branches on status, so a
/// refusal read as a success renders an empty index rather than an error.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn an_rpc_error_frame_becomes_the_http_status_it_stands_for() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), |_| {
        vec![error_frame(-32004, "no index 'ghost'")]
    });

    let (status, _, body) =
        through_router(socket, "GET", "/api/search/indexes/ghost/status", "").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert!(
        body.contains("no index 'ghost'"),
        "the daemon's words must reach the caller: {body}"
    );
}

/// Why (#6285, the fail-open check): a daemon that is not running must be
/// distinguishable from a healthy one with nothing to show. `502` with a reason
/// is that distinction; a `200` with an empty body would not be.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_dead_socket_is_a_bad_gateway_not_an_empty_success() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let (status, _, body) = through_router(
        tmp.path().join("absent.sock"),
        "GET",
        "/api/search/indexes",
        "",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert!(
        body.contains("trusty-search"),
        "the failure must name the daemon: {body}"
    );
}

/// Why: `POST /chat` and `POST /admin/stop` are called by the SPA and have no
/// socket method. They must refuse loudly and name the gap, not answer an
/// approximate `502` that reads as "the daemon is down".
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn an_unmapped_path_is_not_implemented_and_names_itself() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), |_| vec![result_frame(json!({}))]);

    let (status, _, body) = through_router(socket, "POST", "/api/search/chat", "{}").await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{body}");
    assert!(body.contains("chat"), "{body}");
}

/// Why: the SPA's own `base.js` documents `/proxy/search/` as a supported mount,
/// so callers predating the `/api/` rename exist. Removing the search row from
/// the generic proxy would have answered them `400 unknown daemon`.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn deprecated_alias_reaches_the_same_handler() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), |_| {
        vec![result_frame(json!({ "status": "ok" }))]
    });

    let (status, _, body) = through_router(socket, "GET", "/proxy/search/health", "").await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

// ─── streams ─────────────────────────────────────────────────────────────────

/// Why: the reindex view reads `/reindex/stream` as Server-Sent Events, and the
/// socket answers the same event sequence as typed frames. One item must become
/// exactly one `data:` line carrying the same document — that is the parity
/// `trusty_search::service::rpc::streams` pins on its side, and this is the
/// other half of it.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_stream_reaches_the_browser_frame_for_frame() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), |request| {
        assert_eq!(request["method"], json!("search.status.stream"));
        assert_eq!(
            request["stream"],
            json!(true),
            "a streaming method needs the negotiation field"
        );
        vec![
            item_frame(json!({ "type": "connected" })),
            item_frame(json!({ "type": "stats", "indexes": 3 })),
            item_frame(json!({ "type": "lag", "skipped": 7 })),
            end_frame(),
        ]
    });

    let (status, content_type, body) =
        through_router(socket, "GET", "/api/search/status/stream", "").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(content_type, "text/event-stream");
    assert_eq!(
        body,
        "data: {\"type\":\"connected\"}\n\n\
         data: {\"indexes\":3,\"type\":\"stats\"}\n\n\
         data: {\"skipped\":7,\"type\":\"lag\"}\n\n",
        "each item must be exactly one SSE data line"
    );
}

/// Why: `GET /indexes/{id}/reindex/stream` answered `404` over HTTP for an index
/// with no progress record, and on the socket that refusal is the stream's FIRST
/// frame. Committing to `200 text/event-stream` before reading it would turn
/// every such refusal into an empty stream — which the SPA reads as a finished
/// reindex.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_stream_refusal_before_the_first_item_is_an_http_status() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), |_| {
        vec![stream_error_frame(
            -32004,
            "no reindex in progress for 'ghost'",
        )]
    });

    let (status, content_type, body) = through_router(
        socket,
        "GET",
        "/api/search/indexes/ghost/reindex/stream",
        "",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert!(
        !content_type.contains("event-stream"),
        "a refusal must not open a stream: {content_type}"
    );
    assert!(body.contains("no reindex in progress"), "{body}");
}

/// Why: once the stream is open the status is already sent, so a failure can
/// only be reported in the body. A silent close is what the SPA reads as a
/// COMPLETED reindex, so the failure has to arrive as an event.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_mid_stream_failure_becomes_an_error_event() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), |_| {
        vec![
            item_frame(json!({ "type": "progress", "files": 1 })),
            stream_error_frame(-32603, "the reindex worker died"),
        ]
    });

    let (status, content_type, body) = through_router(
        socket,
        "GET",
        "/api/search/indexes/scratch/reindex/stream",
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(content_type, "text/event-stream");
    assert!(body.starts_with("data: {"), "{body}");
    assert!(
        body.contains("the reindex worker died"),
        "a broken stream must say so rather than closing quietly: {body}"
    );
    assert!(body.contains("\"type\":\"error\""), "{body}");
}

/// Why: a truncated stream — the socket closing with no terminal frame — is the
/// same hazard as a mid-stream failure and must not read as a clean end.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_truncated_stream_becomes_an_error_event() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), |_| {
        vec![item_frame(json!({ "type": "progress", "files": 1 }))]
    });

    let (status, _, body) = through_router(
        socket,
        "GET",
        "/api/search/indexes/scratch/reindex/stream",
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body.contains("\"type\":\"error\""),
        "an unterminated stream must not read as complete: {body}"
    );
}

/// Why: a status stream can be silent for minutes, and the reader used to
/// notice a departed browser only when the next frame arrived. Until then it
/// held the socket open, and the daemon's producer behind it — so closing a
/// quiet dashboard tab leaked a producer for as long as the stream stayed quiet.
/// The reader now selects on the sender's closure, so the socket goes with the
/// body.
/// What: opens the real stream route against a stub that answers ONE item and
/// then stays silent, reads that item to prove the stream is live, drops the
/// response body, and waits for the stub to observe the socket closing.
///
/// The stub detects the close by writing, not reading: the RPC client
/// half-closes its write side once the request frame is out, so this end is
/// already at EOF and a read proves nothing. Each probe is a single space rather
/// than a frame — a reader still sitting there parks on the unterminated line
/// instead of being woken by it, which is what makes a bridge without the
/// disconnect arm hang here rather than pass.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_browser_disconnect_releases_the_daemon_socket() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = tmp.path().join("sockets").join("quiet-stream.sock");
    let listener = trusty_common::uds::bind_hardened(&socket).expect("bind");
    let (closed_tx, closed_rx) = tokio::sync::oneshot::channel::<()>();

    let _serving = tokio::spawn(async move {
        let Ok((mut conn, _)) = listener.accept().await else {
            return;
        };
        let mut raw = Vec::new();
        let _ = conn.read_to_end(&mut raw).await;

        let opener = format!("{}\n", item_frame(json!({ "type": "connected" })));
        if conn.write_all(opener.as_bytes()).await.is_err() {
            return;
        }
        let _ = conn.flush().await;

        loop {
            if conn.write_all(b" ").await.is_err() {
                let _ = closed_tx.send(());
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    });

    let router = build_router(AppState::new(vec![]).with_search_socket(socket));
    let request = Request::builder()
        .method("GET")
        .uri("/api/search/status/stream")
        .body(Body::empty())
        .expect("request");
    let response = router.oneshot(request).await.expect("response");
    assert_eq!(response.status(), StatusCode::OK);

    let mut body = response.into_body();
    let first = body
        .frame()
        .await
        .expect("the stream carries its opener")
        .expect("a first chunk");
    let data = first.into_data().expect("a data frame");
    assert!(
        String::from_utf8_lossy(&data).contains("connected"),
        "the opener proves the stream is live before the disconnect"
    );

    let dropped_at = std::time::Instant::now();
    drop(body);

    tokio::time::timeout(std::time::Duration::from_secs(2), closed_rx)
        .await
        .expect("the daemon side must see the socket close, not wait for a frame that never comes")
        .expect("the stub reports the close");
    assert!(
        dropped_at.elapsed() < std::time::Duration::from_secs(2),
        "the socket must be released on disconnect: {:?}",
        dropped_at.elapsed()
    );
}

/// Why: nothing bound to the socket must fail the stream open with a status,
/// not with an empty `200` event stream.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_stream_against_a_dead_socket_is_a_bad_gateway() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let (status, content_type, body) = through_router(
        tmp.path().join("absent.sock"),
        "GET",
        "/api/search/status/stream",
        "",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert!(!content_type.contains("event-stream"), "{content_type}");
}
