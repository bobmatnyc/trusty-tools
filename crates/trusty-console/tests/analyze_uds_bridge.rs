//! `/api/analyze/*` reaches trusty-analyze over its Unix socket (#6155).
//!
//! Why: the analyze dashboard moved into this binary, and its every API call
//! resolves to `/api/analyze/…`. trusty-analyze has no HTTP surface since #6287,
//! so that prefix has to reach the daemon over a socket instead — and the SPA
//! must not be able to tell. These cases drive the REAL router against a stub
//! daemon socket, which is the only way to prove the whole path: route, mapping
//! table and RPC exchange.
//!
//! What: one stub socket per case, a `build_router` pointed at it through
//! `AppState::with_analyze_socket`, and one assertion per behaviour the
//! dashboard depends on.
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
fn stub_daemon<F>(dir: &Path, respond: F) -> PathBuf
where
    F: Fn(&Value) -> Vec<String> + Send + Sync + 'static,
{
    let socket = dir.join("sockets").join("analyze.sock");
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

// ─── driving the router ──────────────────────────────────────────────────────

/// Send one request through the real router with analyze pointed at `socket`.
async fn through_router(socket: PathBuf, method: &str, uri: &str) -> (StatusCode, String, String) {
    with_body(socket, method, uri, Body::empty()).await
}

/// The same, carrying a request body.
async fn with_body(
    socket: PathBuf,
    method: &str,
    uri: &str,
    body: Body,
) -> (StatusCode, String, String) {
    let router = build_router(AppState::new(vec![]).with_analyze_socket(socket));
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(body)
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

// ─── reads ───────────────────────────────────────────────────────────────────

/// Why: this is the whole dashboard. `GET /api/analyze/health` used to be a
/// plain HTTP `/health` on the daemon; it now has to become one `analyze.health`
/// call whose result reaches the browser unchanged, or the SPA renders a
/// connection error against a running daemon.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn unary_route_returns_the_daemon_body_verbatim() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), |request| {
        vec![result_frame(json!({
            "status": "ok",
            "version": "0.12.7",
            "method_seen": request["method"].clone(),
        }))]
    });

    let (status, content_type, body) = through_router(socket, "GET", "/api/analyze/health").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(content_type.contains("application/json"), "{content_type}");
    let parsed: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(parsed["version"], json!("0.12.7"));
    assert_eq!(parsed["method_seen"], json!("analyze.health"));
}

/// Why: the index id used to be a path segment and is a `params` field on this
/// wire. A row that forgot the rename would send `{}` and the daemon would
/// answer `invalid_params` for every index the SPA opened.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_path_segment_reaches_the_daemon_as_a_params_field() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), |request| {
        vec![result_frame(json!({
            "method_seen": request["method"].clone(),
            "index_seen": request["params"]["index_id"].clone(),
        }))]
    });

    let (status, _, body) =
        through_router(socket, "GET", "/api/analyze/indexes/trusty-tools/quality").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let parsed: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(parsed["method_seen"], json!("analyze.quality"));
    assert_eq!(parsed["index_seen"], json!("trusty-tools"));
}

/// Why: a query string is text while the RPC params are typed, AND this one row
/// renames its key — `api.js` sends `top_k` where `HotspotsRequest` expects
/// `top_n`. Getting either wrong caps every hotspot list at the daemon's default
/// behind a panel that shows a shorter list without saying why.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn the_hotspot_cap_arrives_renamed_and_typed() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), |request| {
        vec![result_frame(json!({
            "method_seen": request["method"].clone(),
            "top_n": request["params"]["top_n"].clone(),
            "top_k_present": request["params"]["top_k"].is_null(),
        }))]
    });

    let (status, _, body) = through_router(
        socket,
        "GET",
        "/api/analyze/indexes/proj/complexity_hotspots?top_k=50",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let parsed: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(parsed["method_seen"], json!("analyze.complexity_hotspots"));
    assert_eq!(parsed["top_n"], json!(50), "must arrive as a number");
    assert_eq!(parsed["top_k_present"], json!(true), "renamed, not doubled");
}

/// Why: an index id the SPA percent-encodes has to reach the daemon decoded, or
/// a path-shaped id answers `not found` against an index that exists.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_percent_encoded_index_id_reaches_the_daemon_decoded() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), |request| {
        vec![result_frame(
            json!({ "index_seen": request["params"]["index_id"].clone() }),
        )]
    });

    let (status, _, body) =
        through_router(socket, "GET", "/api/analyze/indexes/my%20proj/quality").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let parsed: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(parsed["index_seen"], json!("my proj"));
}

// ─── writes ──────────────────────────────────────────────────────────────────

/// Why: `POST /facts` is the one call carrying a body, and
/// `facts::UpsertFactRequest` requires four named fields. A handler that dropped
/// the body would answer `invalid_params` for every write.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_fact_write_forwards_its_body_to_the_daemon() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), |request| {
        vec![result_frame(json!({
            "method_seen": request["method"].clone(),
            "params_seen": request["params"].clone(),
        }))]
    });

    let (status, _, body) = with_body(
        socket,
        "POST",
        "/api/analyze/facts",
        Body::from(
            r#"{"subject":"fn auth","predicate":"uses","object":"bcrypt","index_id":"proj"}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let parsed: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(parsed["method_seen"], json!("analyze.facts_upsert"));
    assert_eq!(parsed["params_seen"]["object"], json!("bcrypt"));
    assert_eq!(parsed["params_seen"]["index_id"], json!("proj"));
}

/// Why: `DeleteFactRequest::id` is a `u64` and a path segment is text. A string
/// forwarded blind answers `invalid_params`, so the number has to be made here.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_fact_delete_sends_a_numeric_id() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), |request| {
        vec![result_frame(json!({
            "method_seen": request["method"].clone(),
            "id_is_number": request["params"]["id"].is_number(),
            "id": request["params"]["id"].clone(),
        }))]
    });

    let (status, _, body) = through_router(socket, "DELETE", "/api/analyze/facts/42").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let parsed: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(parsed["method_seen"], json!("analyze.facts_delete"));
    assert_eq!(parsed["id_is_number"], json!(true));
    assert_eq!(parsed["id"], json!(42));
}

// ─── failures ────────────────────────────────────────────────────────────────

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
        "/api/analyze/indexes",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    let parsed: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(parsed["service"], json!("trusty-analyze"));
    assert!(
        parsed["error"]
            .as_str()
            .unwrap_or_default()
            .contains("analyze.list_indexes"),
        "the failure must name the method it was making: {body}"
    );
}

/// Why: a refusal has to arrive as the status the same refusal carried over
/// HTTP, or the SPA cannot tell "no such index" from "the daemon is broken".
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_refusal_carries_the_status_it_came_from() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), |_| {
        vec![error_frame(-32004, "no index 'ghost'")]
    });

    let (status, _, body) =
        through_router(socket, "GET", "/api/analyze/indexes/ghost/quality").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert!(body.contains("no index 'ghost'"), "{body}");
}

/// Why: `analyze.diagnostics` and `analyze.deep_analysis` report a handler
/// cutoff with their own code (#6034/#6041), and a cutoff is not a broken
/// daemon. Collapsing the two into one 5xx is the distinction those issues
/// exist to keep.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_handler_deadline_is_a_gateway_timeout_not_a_generic_failure() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), |_| {
        vec![error_frame(-32005, "diagnostics exceeded its deadline")]
    });

    let (status, _, body) = through_router(socket, "GET", "/api/analyze/indexes/proj/smells").await;
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "{body}");
    assert!(body.contains("exceeded its deadline"), "{body}");
}

/// Why: a path this table does not name must be refused with a body an operator
/// can act on, rather than forwarded somewhere approximate.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn an_unmapped_path_is_not_implemented_and_names_itself() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), |_| vec![result_frame(json!({}))]);

    let (status, _, body) = through_router(socket, "GET", "/api/analyze/indexes/proj/graph").await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{body}");
    assert!(body.contains("indexes/proj/graph"), "{body}");
    assert!(body.contains("trusty-analyze"), "{body}");
}

/// Why: `/sse` is the one path the SPA used to open and #6287 left nothing
/// serving. It must refuse rather than hang or answer an empty `200` stream —
/// the console has no analyze stream method to bridge onto.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn the_retired_sse_path_is_refused_rather_than_bridged() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), |_| vec![result_frame(json!({}))]);

    let (status, content_type, body) = through_router(socket, "GET", "/api/analyze/sse").await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{body}");
    assert!(
        !content_type.contains("text/event-stream"),
        "there is no stream to open: {content_type}"
    );
}

/// Why: this SPA's own `base.js` documented `/proxy/analyze/` as a supported
/// mount, and #6287 deleted the proxy row that answered it. The alias keeps a
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

    let (status, _, body) = through_router(socket, "GET", "/proxy/analyze/health").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let parsed: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(parsed["method_seen"], json!("analyze.health"));
}

/// Why: three bridges share one router and each must reach its OWN daemon. A
/// mis-ordered route would send analyze's calls to the generic proxy, which has
/// no `analyze` row and answers `400 unknown daemon`.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread")]
async fn the_analyze_prefix_wins_over_the_generic_proxy_route() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket = stub_daemon(tmp.path(), |request| {
        vec![result_frame(
            json!({ "method_seen": request["method"].clone() }),
        )]
    });

    let (status, _, body) = through_router(socket, "GET", "/api/analyze/indexes").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the literal /api/analyze/ route must take the prefix: {body}"
    );
    let parsed: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(parsed["method_seen"], json!("analyze.list_indexes"));
}
