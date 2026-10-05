//! Issue #882 — MCP tool tests for empty / whitespace-only query rejection.
//!
//! Why: the daemon refuses an empty query as invalid params (HTTP 400's
//! socket twin); we must verify that the MCP layer maps that refusal to a
//! clean InvalidParams error (bare-method) or `isError: true` (tools/call)
//! rather than an opaque Transport failure.
//! What: a mock socket daemon per test (#9168) drives the McpServer
//! dispatcher. Dropping the mock at the end of the test stops its accept loop
//! and its `TempDir` removes the socket.
//! Test: this file.

use serde_json::Value;

use super::test_daemon::{mock_daemon, refusal, MockDaemon};
use super::{error_codes, Request};

/// A daemon that refuses every search as empty and answers status with
/// `status` — the 400 `{"error": "query must not be empty"}` the real search
/// route sends, rendered the way the daemon renders it on the socket.
async fn empty_query_daemon(status: Value) -> MockDaemon {
    mock_daemon(move |method, params| match method {
        "search.index.status" => {
            let mut v = status.clone();
            v["index_id"] = params["index_id"].clone();
            Ok(v)
        }
        _ => Err(refusal(
            400,
            &serde_json::json!({ "error": "query must not be empty" }),
        )),
    })
    .await
}

fn req(method: &str, params: Value) -> Request {
    Request {
        jsonrpc: Some("2.0".into()),
        id: Some(Value::from(1u64)),
        method: method.into(),
        params: Some(params),
    }
}

/// Why: when the daemon refuses an empty query as invalid params, the MCP `search`
/// tool must surface it as a clean InvalidParams / tool error rather than an
/// opaque transport failure, so the LLM can react with a helpful message.
/// What: a mock daemon refuses every search as invalid params, then the
/// bare-method form must return INVALID_PARAMS and the `tools/call` form
/// `isError: true`.
/// Test: this test.
#[tokio::test]
async fn search_tool_empty_query_surfaces_as_invalid_params() {
    let daemon = empty_query_daemon(serde_json::json!({})).await;
    let server = daemon.server();

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
}

/// Why: per-lane tools (`search_lexical`, `search_semantic`, `search_kg`,
/// `search_all`) share the same `search.query` call — confirm they too return a clean
/// InvalidParams / tool error when the daemon rejects an empty query.
/// What: mock daemon refuses any search; asserts `search_lexical`
/// returns INVALID_PARAMS (bare-method) and isError=true (tools/call).
/// Test: this test.
#[tokio::test]
async fn search_lexical_empty_query_surfaces_as_invalid_params() {
    let daemon = empty_query_daemon(serde_json::json!({
        "search_capabilities": ["bm25", "literal"],
        "stages": {
            "lexical": { "status": "ready" },
            "semantic": { "status": "pending" },
            "graph":   { "status": "pending" },
        }
    }))
    .await;
    let server = daemon.server();

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
}

/// Why: `search_semantic` goes through the same `run_lane_search` / `call_scoped()`
/// pipeline as `search_lexical`; the invalid-params mapping must fire even
/// after the pre-flight stage check passes. This guards regressions where a
/// future refactor adds an early-exit path that skips the empty-query guard.
/// What: mock daemon reports `vector` capability ready (so the pre-flight
/// passes) but refuses the actual search. Asserts both the bare-method and
/// `tools/call` forms surface InvalidParams / isError=true.
/// Test: this test.
#[tokio::test]
async fn search_semantic_empty_query_surfaces_as_invalid_params() {
    let daemon = empty_query_daemon(serde_json::json!({
        "search_capabilities": ["bm25", "literal", "vector"],
        "stages": {
            "lexical":  { "status": "ready" },
            "semantic": { "status": "ready" },
            "graph":    { "status": "pending" },
        }
    }))
    .await;
    let server = daemon.server();

    // Bare-method form — the pre-flight passes (vector ready) but the search
    // is refused; the MCP layer must map it to INVALID_PARAMS.
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
}

/// Why: `search_all` with an `index_id` routes through `run_lane_search` with
/// no stage pre-check (the All lane has no required capability). The
/// invalid-params mapping must still fire when the daemon rejects an empty
/// query, verifying the guard lives in the shared transport and not just in
/// the pre-flight branch.
/// What: mock daemon refuses any search. Asserts both the bare-method and
/// `tools/call` forms surface InvalidParams / isError=true.
/// Test: this test.
#[tokio::test]
async fn search_all_empty_query_surfaces_as_invalid_params() {
    let daemon = empty_query_daemon(serde_json::json!({})).await;
    let server = daemon.server();

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
}
