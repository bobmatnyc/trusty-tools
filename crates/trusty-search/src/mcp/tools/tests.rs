//! Core dispatch tests for the MCP tool dispatcher.
//!
//! Why: validates the JSON-RPC protocol layer (version check, notification
//! suppression, `tools/call` vs bare-method dispatch, error code mapping)
//! and the cross-cutting tool characteristics (all tools appear in
//! `tools/list`, schema requires the right fields, `grep`/`search_all`
//! param validation).
//! What: unit tests that either do not need a live daemon (mock base URL
//! `http://127.0.0.1:1` that cannot connect) or spin up a tiny axum mock
//! daemon on a loopback port.
//! Test: this file.

use serde_json::Value;

use super::{error_codes, McpServer, Request};

pub(super) fn req(method: &str, params: Value) -> Request {
    Request {
        jsonrpc: Some("2.0".into()),
        id: Some(Value::from(1u64)),
        method: method.into(),
        params: Some(params),
    }
}

#[tokio::test]
async fn rejects_wrong_jsonrpc_version() {
    let server = McpServer::new("http://127.0.0.1:1");
    let r = Request {
        jsonrpc: Some("1.0".into()),
        id: Some(Value::from(7u64)),
        method: "search_health".into(),
        params: None,
    };
    let resp = server.dispatch(r).await;
    let err = resp.error.expect("expected error");
    assert_eq!(err.code, error_codes::INVALID_REQUEST);
    assert_eq!(resp.id, Some(Value::from(7u64)));
}

#[tokio::test]
async fn unknown_tool_returns_method_not_found() {
    let server = McpServer::new("http://127.0.0.1:1");
    let resp = server.dispatch(req("not_a_tool", Value::Null)).await;
    let err = resp.error.expect("expected error");
    assert_eq!(err.code, error_codes::METHOD_NOT_FOUND);
}

#[tokio::test]
async fn missing_params_returns_invalid_params() {
    let server = McpServer::new("http://127.0.0.1:1");
    let resp = server
        .dispatch(req("index_file", serde_json::json!({})))
        .await;
    let err = resp.error.expect("expected error");
    assert_eq!(err.code, error_codes::INVALID_PARAMS);
}

#[tokio::test]
async fn tools_list_returns_all_tools() {
    let server = McpServer::new("http://127.0.0.1:1");
    let resp = server.dispatch(req("tools/list", Value::Null)).await;
    let result = resp.result.expect("expected result");
    let tools = result
        .get("tools")
        .and_then(Value::as_array)
        .expect("array");
    // Issue #36 requires the 6 core MCP tools to be present; we ship
    // additional tools beyond that minimum.
    assert!(
        tools.len() >= 6,
        "expected at least 6 tools, got {}",
        tools.len()
    );
    let names: Vec<&str> = tools
        .iter()
        .filter_map(|t| t.get("name").and_then(Value::as_str))
        .collect();
    for required in [
        "search",
        "index_file",
        "remove_file",
        "list_indexes",
        "create_index",
        "search_health",
    ] {
        assert!(
            names.contains(&required),
            "missing required tool: {required}"
        );
    }
}

/// Issue #36 — verify the `initialize` handshake returns the spec-shaped
/// payload Claude Code expects on startup.
#[tokio::test]
async fn test_initialize_response() {
    let server = McpServer::new("http://127.0.0.1:1");
    let r = Request {
        jsonrpc: Some("2.0".into()),
        id: Some(Value::from(1u64)),
        method: "initialize".into(),
        params: Some(serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "test", "version": "0.0.0" }
        })),
    };
    let resp = server.dispatch(r).await;
    assert!(resp.error.is_none(), "initialize must not error");
    let result = resp.result.expect("expected result");
    assert_eq!(result["protocolVersion"], "2024-11-05");
    assert!(result["capabilities"].get("tools").is_some());
    assert_eq!(result["serverInfo"]["name"], "trusty-search");
    assert!(result["serverInfo"]["version"].is_string());
}

/// Issue #36 — `tools/list` must surface every spec-required tool so
/// MCP clients can render the full manifest.
#[tokio::test]
async fn test_tools_list_response() {
    let server = McpServer::new("http://127.0.0.1:1");
    let resp = server.dispatch(req("tools/list", Value::Null)).await;
    let result = resp.result.expect("expected result");
    let tools = result
        .get("tools")
        .and_then(Value::as_array)
        .expect("array");
    let names: Vec<&str> = tools
        .iter()
        .filter_map(|t| t.get("name").and_then(Value::as_str))
        .collect();
    for required in [
        "search",
        "index_file",
        "remove_file",
        "list_indexes",
        "create_index",
        "search_health",
    ] {
        assert!(
            names.contains(&required),
            "tools/list missing '{required}' (got {names:?})"
        );
    }
    // Each tool must carry an inputSchema so clients can validate args.
    for t in tools {
        assert!(t.get("name").is_some());
        assert!(t.get("inputSchema").is_some());
    }
}

/// Issue #36 — JSON-RPC method-not-found surfaces as -32601.
#[tokio::test]
async fn test_unknown_method_returns_error() {
    let server = McpServer::new("http://127.0.0.1:1");
    let resp = server
        .dispatch(req("definitely_not_a_method", Value::Null))
        .await;
    let err = resp.error.expect("expected error");
    assert_eq!(err.code, error_codes::METHOD_NOT_FOUND);
}

/// `notifications/initialized` is a JSON-RPC notification — the server
/// must NOT emit a response, signalled by `Response::suppress = true`.
#[tokio::test]
async fn notification_initialized_is_suppressed() {
    let server = McpServer::new("http://127.0.0.1:1");
    let r = Request {
        jsonrpc: Some("2.0".into()),
        id: None, // notifications carry no id
        method: "notifications/initialized".into(),
        params: None,
    };
    let resp = server.dispatch(r).await;
    assert!(resp.suppress, "notifications must be suppressed");
}

/// Parity gate: every HTTP endpoint reachable via REST must also be callable
/// as an MCP tool. This guards the "MCP and HTTP are functionally equivalent"
/// invariant — if a new HTTP route lands without a matching tool, this fails.
#[tokio::test]
async fn test_tools_list_complete() {
    let server = McpServer::new("http://127.0.0.1:1");
    let resp = server.dispatch(req("tools/list", Value::Null)).await;
    let result = resp.result.expect("expected result");
    let tools = result
        .get("tools")
        .and_then(Value::as_array)
        .expect("array");
    let names: Vec<&str> = tools
        .iter()
        .filter_map(|t| t.get("name").and_then(Value::as_str))
        .collect();
    for required in [
        "search",
        "index_file",
        "remove_file",
        "list_indexes",
        "create_index",
        "search_health",
        "delete_index",
        "reindex",
        "index_status",
        "list_chunks",
        "chat",
        "search_all",
    ] {
        assert!(
            names.contains(&required),
            "tools/list missing '{required}' (got {names:?})"
        );
    }
}

/// Issue #10 — `search_all` requires the `query` arg and rejects missing it
/// before any HTTP round-trip.
#[tokio::test]
async fn search_all_missing_query_returns_invalid_params() {
    let server = McpServer::new("http://127.0.0.1:1");
    let resp = server
        .dispatch(req("search_all", serde_json::json!({})))
        .await;
    let err = resp.error.expect("expected error");
    assert_eq!(err.code, error_codes::INVALID_PARAMS);
}

#[tokio::test]
async fn tools_call_without_name_returns_invalid_params() {
    let server = McpServer::new("http://127.0.0.1:1");
    let resp = server
        .dispatch(req("tools/call", serde_json::json!({})))
        .await;
    let err = resp.error.expect("expected error");
    assert_eq!(err.code, error_codes::INVALID_PARAMS);
}

/// `grep` is listed and missing-pattern fast-fails before any HTTP hop.
#[tokio::test]
async fn grep_missing_pattern_returns_invalid_params() {
    let server = McpServer::new("http://127.0.0.1:1");
    let resp = server.dispatch(req("grep", serde_json::json!({}))).await;
    let err = resp.error.expect("expected error");
    assert_eq!(err.code, error_codes::INVALID_PARAMS);
}

/// Issue #447 — `max_count` is forwarded as `max_results` to the daemon.
///
/// Why: when the MCP client passes `max_count` (ripgrep's `--max-count`
/// flag name) the dispatcher must translate it to `max_results` before
/// POSTing to the daemon. Without the alias the parameter was silently
/// dropped and the daemon applied its default cap of 100 regardless.
/// What: asserts that a `grep` call with `max_count=5` (and no
/// `max_results`) forwards `max_results: 5` in the daemon request body.
/// Test: spins up a tiny mock daemon that echoes back the request body,
/// then asserts the forwarded body contains `max_results == 5`.
#[tokio::test]
async fn grep_max_count_alias_forwarded_as_max_results() {
    use axum::routing::post;
    use axum::{Json, Router};
    use std::sync::Arc;
    use tokio::sync::Mutex;

    let captured: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
    let captured_clone = Arc::clone(&captured);

    async fn grep_handler(
        axum::extract::State(captured): axum::extract::State<Arc<Mutex<Option<Value>>>>,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        *captured.lock().await = Some(body);
        Json(serde_json::json!({ "matches": [], "total": 0, "truncated": false }))
    }

    let app = Router::new()
        .route("/indexes/idx/grep", post(grep_handler))
        .with_state(captured_clone);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let server = McpServer::new(format!("http://{addr}"));
    let resp = server
        .dispatch(req(
            "grep",
            serde_json::json!({
                "pattern": "fn foo",
                "index_id": "idx",
                "max_count": 5_u64,
            }),
        ))
        .await;
    assert!(resp.error.is_none(), "unexpected error: {:?}", resp.error);
    let body = captured.lock().await.clone().expect("no request captured");
    assert_eq!(
        body.get("max_results").and_then(Value::as_u64),
        Some(5),
        "max_count must be forwarded as max_results; got body: {body:?}"
    );
}

/// `grep` appears in `tools/list` with a `pattern`-required schema.
#[tokio::test]
async fn grep_listed_in_tools_with_required_pattern() {
    let server = McpServer::new("http://127.0.0.1:1");
    let resp = server.dispatch(req("tools/list", Value::Null)).await;
    let result = resp.result.expect("expected result");
    let tools = result
        .get("tools")
        .and_then(Value::as_array)
        .expect("array");
    let grep = tools
        .iter()
        .find(|t| t.get("name").and_then(Value::as_str) == Some("grep"))
        .expect("grep tool missing from tools/list");
    let required = grep["inputSchema"]["required"]
        .as_array()
        .expect("required array");
    assert!(
        required.iter().any(|v| v.as_str() == Some("pattern")),
        "grep schema must require 'pattern'"
    );
}

// ----------------------------------------------------------------
// Issue #138 — per-lane MCP tools: shared mock-daemon helper
// ----------------------------------------------------------------

/// Spin up a one-shot axum mock daemon on a loopback port.
///
/// Why: the per-lane tool tests in `tests_lane.rs` all need a controllable
/// daemon — this helper lets each test specify exactly what `GET
/// /indexes/:id/status` and `POST /indexes/:id/search` return, and
/// captures the inbound request bodies so tests can assert the correct
/// `SearchQuery` shape was dispatched.
/// What: returns `(base_url, captured_search_bodies, captured_search_paths)`.
/// Test: used by every test in `tests_lane.rs`.
pub(super) async fn spawn_mock_daemon(
    status_response: Value,
    search_response: Value,
) -> (
    String,
    std::sync::Arc<tokio::sync::Mutex<Vec<Value>>>,
    std::sync::Arc<tokio::sync::Mutex<Vec<String>>>,
) {
    use axum::extract::{Path, State};
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use std::sync::Arc;
    use tokio::sync::Mutex;

    #[derive(Clone)]
    struct MockState {
        status_response: Value,
        search_response: Value,
        captured_bodies: Arc<Mutex<Vec<Value>>>,
        captured_paths: Arc<Mutex<Vec<String>>>,
    }

    let captured_bodies: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let captured_paths: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let state = MockState {
        status_response,
        search_response,
        captured_bodies: Arc::clone(&captured_bodies),
        captured_paths: Arc::clone(&captured_paths),
    };

    async fn status_handler(Path(id): Path<String>, State(s): State<MockState>) -> Json<Value> {
        // Inject the index_id so the handler returns a payload that
        // looks like a real daemon response.
        let mut v = s.status_response.clone();
        if v.is_object() {
            v["index_id"] = Value::String(id);
        }
        Json(v)
    }

    async fn search_handler_mock(
        Path(id): Path<String>,
        State(s): State<MockState>,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        s.captured_paths
            .lock()
            .await
            .push(format!("/indexes/{id}/search"));
        s.captured_bodies.lock().await.push(body);
        Json(s.search_response.clone())
    }

    let app = Router::new()
        .route("/indexes/{id}/status", get(status_handler))
        .route("/indexes/{id}/search", post(search_handler_mock))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let base_url = format!("http://{addr}");
    (base_url, captured_bodies, captured_paths)
}
