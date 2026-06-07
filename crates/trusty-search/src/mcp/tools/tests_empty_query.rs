//! Issue #882 — MCP tool tests for empty / whitespace-only query rejection.
//!
//! Why: the daemon returns HTTP 400 for empty queries; we must verify that the
//! MCP layer maps that 400 to a clean InvalidParams error (bare-method) or
//! `isError: true` (tools/call) rather than an opaque Transport failure.
//! What: spins up a tiny mock daemon per test and drives the McpServer dispatcher.
//! Each test wires a `tokio::sync::oneshot` shutdown channel into axum's graceful
//! shutdown so the listener is torn down deterministically at the end of the test,
//! preventing port leaks under parallel test runs.
//! Test: this file.

use serde_json::Value;

use super::{error_codes, McpServer, Request};

fn req(method: &str, params: Value) -> Request {
    Request {
        jsonrpc: Some("2.0".into()),
        id: Some(Value::from(1u64)),
        method: method.into(),
        params: Some(params),
    }
}

/// Why: when the daemon returns HTTP 400 for an empty query, the MCP `search`
/// tool must surface it as a clean InvalidParams / tool error rather than an
/// opaque transport failure, so the LLM can react with a helpful message.
/// What: spins up a mock daemon that returns 400 for any search request, then
/// asserts the bare-method form returns INVALID_PARAMS and the `tools/call`
/// form returns `isError: true`. Graceful shutdown via oneshot channel.
/// Test: this test.
#[tokio::test]
async fn search_tool_empty_query_surfaces_as_invalid_params() {
    use axum::routing::post;
    use axum::{Json, Router};
    use tokio::sync::oneshot;

    async fn bad_search(Json(_body): Json<Value>) -> (axum::http::StatusCode, Json<Value>) {
        (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "query must not be empty" })),
        )
    }

    let app = Router::new().route("/indexes/demo/search", post(bad_search));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async {
                shutdown_rx.await.ok();
            })
            .await;
    });

    let server = McpServer::new(format!("http://{addr}"));

    // Bare-method form: expect INVALID_PARAMS JSON-RPC error.
    let resp = server
        .dispatch(req(
            "search",
            serde_json::json!({ "index_id": "demo", "query": "" }),
        ))
        .await;
    let err = resp.error.expect("expected JSON-RPC error for empty query");
    assert_eq!(
        err.code,
        error_codes::INVALID_PARAMS,
        "empty query must map to INVALID_PARAMS, got code={}",
        err.code
    );
    assert!(
        err.message.contains("empty"),
        "error message must mention 'empty': {}",
        err.message
    );

    // tools/call form: expect isError=true in the result envelope.
    let resp = server
        .dispatch(req(
            "tools/call",
            serde_json::json!({
                "name": "search",
                "arguments": { "index_id": "demo", "query": "   " }
            }),
        ))
        .await;
    let result = resp.result.expect("tools/call must return result envelope");
    assert_eq!(
        result["isError"], true,
        "whitespace-only query must return isError=true"
    );

    let _ = shutdown_tx.send(());
    let _ = handle.await;
}

/// Why: per-lane tools (`search_lexical`, `search_semantic`, `search_kg`,
/// `search_all`) share the same POST path — confirm they too return a clean
/// InvalidParams / tool error when the daemon rejects an empty query.
/// What: mock daemon returns 400 for any search; asserts `search_lexical`
/// returns INVALID_PARAMS (bare-method) and isError=true (tools/call).
/// Graceful shutdown via oneshot channel.
/// Test: this test.
#[tokio::test]
async fn search_lexical_empty_query_surfaces_as_invalid_params() {
    use axum::routing::{get, post};
    use axum::{extract::Path, Json, Router};
    use tokio::sync::oneshot;

    async fn bad_search(
        Path(_id): Path<String>,
        Json(_body): Json<Value>,
    ) -> (axum::http::StatusCode, Json<Value>) {
        (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "query must not be empty" })),
        )
    }
    async fn status_ok(Path(id): Path<String>) -> Json<Value> {
        Json(serde_json::json!({
            "index_id": id,
            "search_capabilities": ["bm25", "literal"],
            "stages": {
                "lexical": { "status": "ready" },
                "semantic": { "status": "pending" },
                "graph":   { "status": "pending" },
            }
        }))
    }

    let app = Router::new()
        .route("/indexes/{id}/search", post(bad_search))
        .route("/indexes/{id}/status", get(status_ok));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async {
                shutdown_rx.await.ok();
            })
            .await;
    });

    let server = McpServer::new(format!("http://{addr}"));

    // Bare method — expect INVALID_PARAMS.
    let resp = server
        .dispatch(req(
            "search_lexical",
            serde_json::json!({ "index_id": "demo", "query": "" }),
        ))
        .await;
    let err = resp.error.expect("expected JSON-RPC error");
    assert_eq!(err.code, error_codes::INVALID_PARAMS);

    // tools/call — expect isError=true.
    let resp = server
        .dispatch(req(
            "tools/call",
            serde_json::json!({
                "name": "search_lexical",
                "arguments": { "index_id": "demo", "query": "   " }
            }),
        ))
        .await;
    let result = resp.result.expect("result envelope");
    assert_eq!(result["isError"], true);

    let _ = shutdown_tx.send(());
    let _ = handle.await;
}

/// Why: `search_semantic` goes through the same `run_lane_search` / `post()`
/// pipeline as `search_lexical`; the 400→InvalidParams mapping must fire even
/// after the pre-flight stage check passes. This guards regressions where a
/// future refactor adds an early-exit path that skips the empty-query guard.
/// What: mock daemon reports `vector` capability ready (so the pre-flight
/// passes) but returns 400 on the actual search POST. Asserts both the
/// bare-method and `tools/call` forms surface InvalidParams / isError=true.
/// Graceful shutdown via oneshot channel.
/// Test: this test.
#[tokio::test]
async fn search_semantic_empty_query_surfaces_as_invalid_params() {
    use axum::routing::{get, post};
    use axum::{extract::Path, Json, Router};
    use tokio::sync::oneshot;

    async fn bad_search(
        Path(_id): Path<String>,
        Json(_body): Json<Value>,
    ) -> (axum::http::StatusCode, Json<Value>) {
        (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "query must not be empty" })),
        )
    }
    async fn status_vector_ready(Path(id): Path<String>) -> Json<Value> {
        Json(serde_json::json!({
            "index_id": id,
            "search_capabilities": ["bm25", "literal", "vector"],
            "stages": {
                "lexical":  { "status": "ready" },
                "semantic": { "status": "ready" },
                "graph":    { "status": "pending" },
            }
        }))
    }

    let app = Router::new()
        .route("/indexes/{id}/search", post(bad_search))
        .route("/indexes/{id}/status", get(status_vector_ready));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async {
                shutdown_rx.await.ok();
            })
            .await;
    });

    let server = McpServer::new(format!("http://{addr}"));

    // Bare-method form — the pre-flight passes (vector ready) but the POST
    // returns 400; the MCP layer must map it to INVALID_PARAMS.
    let resp = server
        .dispatch(req(
            "search_semantic",
            serde_json::json!({ "index_id": "demo", "query": "" }),
        ))
        .await;
    let err = resp.error.expect("expected JSON-RPC error");
    assert_eq!(
        err.code,
        error_codes::INVALID_PARAMS,
        "search_semantic empty query must map to INVALID_PARAMS, got code={}",
        err.code
    );

    // tools/call form — must return isError=true.
    let resp = server
        .dispatch(req(
            "tools/call",
            serde_json::json!({
                "name": "search_semantic",
                "arguments": { "index_id": "demo", "query": "   " }
            }),
        ))
        .await;
    let result = resp.result.expect("tools/call must return result envelope");
    assert_eq!(
        result["isError"], true,
        "search_semantic whitespace-only query must return isError=true"
    );

    let _ = shutdown_tx.send(());
    let _ = handle.await;
}

/// Why: `search_all` with an `index_id` routes through `run_lane_search` with
/// no stage pre-check (the All lane has no required capability). The
/// 400→InvalidParams mapping must still fire when the daemon rejects an empty
/// query, verifying the guard lives in the shared `post()` helper and not just
/// in the pre-flight branch.
/// What: mock daemon returns 400 for any search POST. Asserts both the
/// bare-method and `tools/call` forms surface InvalidParams / isError=true.
/// Graceful shutdown via oneshot channel.
/// Test: this test.
#[tokio::test]
async fn search_all_empty_query_surfaces_as_invalid_params() {
    use axum::routing::post;
    use axum::{extract::Path, Json, Router};
    use tokio::sync::oneshot;

    async fn bad_search(
        Path(_id): Path<String>,
        Json(_body): Json<Value>,
    ) -> (axum::http::StatusCode, Json<Value>) {
        (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "query must not be empty" })),
        )
    }

    // search_all with index_id routes to /indexes/{id}/search (no status probe).
    let app = Router::new().route("/indexes/{id}/search", post(bad_search));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async {
                shutdown_rx.await.ok();
            })
            .await;
    });

    let server = McpServer::new(format!("http://{addr}"));

    // Bare-method form.
    let resp = server
        .dispatch(req(
            "search_all",
            serde_json::json!({ "index_id": "demo", "query": "" }),
        ))
        .await;
    let err = resp.error.expect("expected JSON-RPC error");
    assert_eq!(
        err.code,
        error_codes::INVALID_PARAMS,
        "search_all empty query must map to INVALID_PARAMS, got code={}",
        err.code
    );

    // tools/call form.
    let resp = server
        .dispatch(req(
            "tools/call",
            serde_json::json!({
                "name": "search_all",
                "arguments": { "index_id": "demo", "query": "   " }
            }),
        ))
        .await;
    let result = resp.result.expect("tools/call must return result envelope");
    assert_eq!(
        result["isError"], true,
        "search_all whitespace-only query must return isError=true"
    );

    let _ = shutdown_tx.send(());
    let _ = handle.await;
}
