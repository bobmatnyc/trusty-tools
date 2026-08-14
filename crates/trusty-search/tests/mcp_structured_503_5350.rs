//! End-to-end proof that a structured daemon 503 reaches an MCP client as data
//! (issue #5350).
//!
//! Why: the unit tests in `mcp/tools/tests_unavailable.rs` fix the daemon's
//! response with a hand-written body, which proves the transport but not that
//! the transport and the daemon still agree. This file removes the fixture: a
//! real axum router built by `service::server::build_router` renders the
//! `index_not_resident` verdict from `degraded.rs`, and a real `McpServer`
//! speaks JSON-RPC against it over a loopback socket. What the test asserts is
//! literally what an MCP client receives.
//!
//! What: registers a cold-parked index in a real `SearchAppState`, serves the
//! real router, and drives `index_status` in both MCP call forms.
//! Test: `cargo test -p trusty-search --test mcp_structured_503_5350`

use std::path::PathBuf;

use serde_json::{json, Value};

use trusty_search::core::registry::IndexRegistry;
use trusty_search::mcp::tools::{INDEX_UNAVAILABLE, INDEX_UNAVAILABLE_CODE};
use trusty_search::mcp::{McpServer, Request};
use trusty_search::service::persistence::PersistedIndex;
use trusty_search::service::server::{build_router, SearchAppState};

/// Serve the real daemon router with one index registered as cold-parked, and
/// return its base URL.
async fn spawn_daemon_with_cold_index(id: &str) -> String {
    let state = SearchAppState::new(IndexRegistry::new());
    state
        .cold_store
        .register_cold_entries(vec![PersistedIndex::new(
            id.to_string(),
            PathBuf::from(format!("/tmp/trusty-5350-{id}")),
        )]);
    let app = build_router(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

fn req(method: &str, params: Value) -> Request {
    Request {
        jsonrpc: Some("2.0".into()),
        id: Some(Value::from(1u64)),
        method: method.into(),
        params: Some(params),
    }
}

/// A real daemon's `503 index_not_resident` arrives at the MCP surface with
/// every field a caller branches on intact.
#[tokio::test]
async fn mcp_client_receives_the_daemons_structured_503() {
    let base = spawn_daemon_with_cold_index("cold-e2e").await;
    let server = McpServer::new(base);

    let resp = server
        .dispatch(req(
            "tools/call",
            json!({ "name": "index_status", "arguments": { "index_id": "cold-e2e" } }),
        ))
        .await;

    let result = resp.result.expect("tools/call always returns a result");
    assert_eq!(result["isError"], Value::Bool(true));

    let m = &result["_meta"];
    assert_eq!(m["error_code"], INDEX_UNAVAILABLE);
    assert_eq!(m["error"], "index_not_resident");
    assert_eq!(m["index_id"], "cold-e2e");
    assert_eq!(m["retryable"], Value::Bool(true));
    assert_eq!(m["restore_via"], "POST /indexes/cold-e2e/search");
    assert_eq!(m["http_status"], 503);

    let text = result["content"][0]["text"]
        .as_str()
        .expect("a prose content node");
    assert!(
        !text.contains("returned 503 Service Unavailable"),
        "the client must not be handed a stringified HTTP failure: {text}"
    );
}

/// The bare-method form of the same call carries it under `error.data`.
#[tokio::test]
async fn bare_method_client_receives_the_daemons_structured_503() {
    let base = spawn_daemon_with_cold_index("cold-e2e-bare").await;
    let server = McpServer::new(base);

    let resp = server
        .dispatch(req("index_status", json!({ "index_id": "cold-e2e-bare" })))
        .await;

    let err = resp
        .error
        .expect("bare-method failures are JSON-RPC errors");
    assert_eq!(err.code, INDEX_UNAVAILABLE_CODE);
    let data = err.data.expect("structured payload rides in error.data");
    assert_eq!(data["error"], "index_not_resident");
    assert_eq!(data["retryable"], Value::Bool(true));
    assert_eq!(data["restore_via"], "POST /indexes/cold-e2e-bare/search");
}
