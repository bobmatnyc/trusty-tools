//! Per-lane MCP search tool tests (issue #138).
//!
//! Why: the four per-lane search tools (`search_lexical`, `search_semantic`,
//! `search_kg`, `search_all`) each have a specific routing contract — which
//! `stage` is pinned, whether `expand_graph` is set, and when they return
//! `STAGE_NOT_READY`. Isolating these tests keeps `tests.rs` focused on the
//! protocol layer and this file focused on lane behaviour.
//! What: exercises each lane's happy path, stage-not-ready path, routing
//! shape, and equivalence with the legacy `search` tool.
//! Test: this file.

use serde_json::Value;

use super::tests::{req, spawn_mock_daemon};
use super::{McpServer, STAGE_NOT_READY_CODE};

/// `search_lexical` pins `stage=lexical` and `expand_graph=false` on
/// the dispatched SearchQuery. Always-available — no status pre-check
/// because Stage 1 is the baseline for every index.
#[tokio::test]
async fn search_lexical_tool_routes_to_lexical_stage_only() {
    let status = serde_json::json!({
        "stages": {
            "lexical":  { "status": "ready" },
            "semantic": { "status": "pending" },
            "graph":    { "status": "pending" },
        },
        "search_capabilities": ["bm25", "literal", "exact_match"],
    });
    let search = serde_json::json!({
        "results": [],
        "intent": "Definition",
        "latency_ms": 1,
    });
    let (base, bodies, paths) = spawn_mock_daemon(status, search).await;
    let server = McpServer::new(base);
    let resp = server
        .dispatch(req(
            "search_lexical",
            serde_json::json!({
                "index_id": "demo",
                "query": "apply_archive_downrank",
                "top_k": 5,
            }),
        ))
        .await;
    assert!(resp.error.is_none(), "lexical tool must not error");

    let bodies = bodies.lock().await;
    assert_eq!(bodies.len(), 1, "exactly one search dispatched");
    let dispatched = &bodies[0];
    assert_eq!(dispatched["stage"], "lexical");
    assert_eq!(dispatched["expand_graph"], false);
    assert_eq!(dispatched["text"], "apply_archive_downrank");
    assert_eq!(dispatched["top_k"], 5);
    let paths = paths.lock().await;
    assert_eq!(paths[0], "/indexes/demo/search");
}

/// `search_semantic` pins `stage=semantic` and `expand_graph=false`.
/// Requires Stage 2 (`vector`) capability; happy path verifies the
/// pre-flight status check sees the ready vector lane.
#[tokio::test]
async fn search_semantic_tool_routes_to_semantic_stage_when_stage_2_ready() {
    let status = serde_json::json!({
        "stages": {
            "lexical":  { "status": "ready" },
            "semantic": { "status": "ready" },
            "graph":    { "status": "pending" },
        },
        "search_capabilities": ["bm25", "literal", "exact_match", "vector"],
    });
    let search = serde_json::json!({
        "results": [],
        "intent": "Conceptual",
        "latency_ms": 7,
    });
    let (base, bodies, _paths) = spawn_mock_daemon(status, search).await;
    let server = McpServer::new(base);
    let resp = server
        .dispatch(req(
            "search_semantic",
            serde_json::json!({
                "index_id": "demo",
                "query": "code that handles JWT verification",
            }),
        ))
        .await;
    assert!(resp.error.is_none());

    let bodies = bodies.lock().await;
    let dispatched = &bodies[0];
    assert_eq!(dispatched["stage"], "semantic");
    assert_eq!(dispatched["expand_graph"], false);
}

/// `search_semantic` returns a STAGE_NOT_READY structured error when
/// the index lacks the `vector` capability. The error includes the
/// full stages snapshot and a `suggested_tools` retry hint.
#[tokio::test]
async fn search_semantic_tool_returns_stage_not_ready_when_stage_2_missing() {
    let status = serde_json::json!({
        "stages": {
            "lexical":  { "status": "ready" },
            "semantic": { "status": "in_progress" },
            "graph":    { "status": "pending" },
        },
        "search_capabilities": ["bm25", "literal", "exact_match"],
    });
    let search = serde_json::json!({ "results": [] });
    let (base, bodies, _) = spawn_mock_daemon(status, search).await;
    let server = McpServer::new(base);

    // Bare-method form returns a JSON-RPC error with code STAGE_NOT_READY_CODE.
    let resp = server
        .dispatch(req(
            "search_semantic",
            serde_json::json!({
                "index_id": "demo",
                "query": "anything",
            }),
        ))
        .await;
    let err = resp.error.expect("expected JSON-RPC error");
    assert_eq!(err.code, STAGE_NOT_READY_CODE);
    assert!(err.message.contains("Stage 2"), "{}", err.message);
    assert!(err.message.contains("embeddings"), "{}", err.message);
    let data = err.data.expect("data field");
    assert_eq!(data["error_code"], "STAGE_NOT_READY");
    let suggested = data["suggested_tools"]
        .as_array()
        .expect("suggested_tools array");
    assert!(suggested
        .iter()
        .any(|v| v.as_str() == Some("search_lexical")));
    assert_eq!(data["current_stages"]["semantic"]["status"], "in_progress");

    // No daemon search call must have happened — the pre-check short-circuited.
    assert!(bodies.lock().await.is_empty());

    // `tools/call` form returns the same condition as
    // `{ isError: true, _meta: { error_code: ... } }`.
    let resp = server
        .dispatch(req(
            "tools/call",
            serde_json::json!({
                "name": "search_semantic",
                "arguments": { "index_id": "demo", "query": "x" }
            }),
        ))
        .await;
    let result = resp.result.expect("tools/call returns result envelope");
    assert_eq!(result["isError"], true);
    assert_eq!(result["_meta"]["error_code"], "STAGE_NOT_READY");
    let suggested = result["_meta"]["suggested_tools"]
        .as_array()
        .expect("suggested array");
    assert!(suggested
        .iter()
        .any(|v| v.as_str() == Some("search_lexical")));
}

/// `search_kg` pins `stage=graph`, `expand_graph=true`, and pre-checks
/// the `kg` capability.
#[tokio::test]
async fn search_kg_tool_routes_to_graph_stage_when_stage_3_ready() {
    let status = serde_json::json!({
        "stages": {
            "lexical":  { "status": "ready" },
            "semantic": { "status": "ready" },
            "graph":    { "status": "ready" },
        },
        "search_capabilities": ["bm25", "literal", "exact_match", "vector", "kg"],
    });
    let search = serde_json::json!({
        "results": [],
        "intent": "Usage",
        "latency_ms": 12,
    });
    let (base, bodies, _) = spawn_mock_daemon(status, search).await;
    let server = McpServer::new(base);
    let resp = server
        .dispatch(req(
            "search_kg",
            serde_json::json!({
                "index_id": "demo",
                "query": "validate_token",
            }),
        ))
        .await;
    assert!(resp.error.is_none());

    let bodies = bodies.lock().await;
    let dispatched = &bodies[0];
    assert_eq!(dispatched["stage"], "graph");
    assert_eq!(dispatched["expand_graph"], true);
}

/// `search_kg` returns STAGE_NOT_READY when the index lacks the `kg`
/// capability, with appropriate fallback hints.
#[tokio::test]
async fn search_kg_tool_returns_stage_not_ready_when_stage_3_missing() {
    let status = serde_json::json!({
        "stages": {
            "lexical":  { "status": "ready" },
            "semantic": { "status": "ready" },
            "graph":    { "status": "in_progress" },
        },
        "search_capabilities": ["bm25", "literal", "exact_match", "vector"],
    });
    let search = serde_json::json!({ "results": [] });
    let (base, bodies, _) = spawn_mock_daemon(status, search).await;
    let server = McpServer::new(base);
    let resp = server
        .dispatch(req(
            "search_kg",
            serde_json::json!({
                "index_id": "demo",
                "query": "Authenticator",
            }),
        ))
        .await;
    let err = resp.error.expect("expected JSON-RPC error");
    assert_eq!(err.code, STAGE_NOT_READY_CODE);
    assert!(err.message.contains("Stage 3"), "{}", err.message);
    assert!(err.message.contains("symbol graph"), "{}", err.message);
    let data = err.data.expect("data");
    // Semantic IS ready, so the fallback should suggest search_semantic
    // ahead of search_lexical.
    let suggested = data["suggested_tools"].as_array().expect("suggested_tools");
    assert_eq!(
        suggested[0].as_str(),
        Some("search_semantic"),
        "stage 3 missing with stage 2 ready should suggest search_semantic first"
    );
    // No search was dispatched.
    assert!(bodies.lock().await.is_empty());
}

/// `search_all` with `index_id` runs the per-index full hybrid: no
/// stage pin, `expand_graph: true`. Mirrors the ticket's #138 spec.
#[tokio::test]
async fn search_all_with_index_id_routes_to_full_hybrid() {
    let status = serde_json::json!({
        "stages": {
            "lexical":  { "status": "ready" },
            "semantic": { "status": "ready" },
            "graph":    { "status": "ready" },
        },
        "search_capabilities": ["bm25", "literal", "exact_match", "vector", "kg"],
    });
    let search = serde_json::json!({
        "results": [],
        "intent": "Conceptual",
        "latency_ms": 8,
    });
    let (base, bodies, paths) = spawn_mock_daemon(status, search).await;
    let server = McpServer::new(base);
    let resp = server
        .dispatch(req(
            "search_all",
            serde_json::json!({
                "index_id": "demo",
                "query": "AuthValidator that handles refresh tokens",
            }),
        ))
        .await;
    assert!(resp.error.is_none());

    let bodies = bodies.lock().await;
    let dispatched = &bodies[0];
    // No stage pin (full hybrid adaptive).
    assert!(
        dispatched.get("stage").is_none() || dispatched["stage"].is_null(),
        "search_all must not pin a stage: got {dispatched:?}"
    );
    assert_eq!(dispatched["expand_graph"], true);
    let paths = paths.lock().await;
    assert_eq!(paths[0], "/indexes/demo/search");
}

/// `search_all` and the legacy `search` tool produce identical
/// dispatched SearchQuery shapes — `search` stays as a back-compat
/// alias per the ticket's spec.
#[tokio::test]
async fn search_all_and_legacy_search_dispatch_equivalent_bodies() {
    let status = serde_json::json!({
        "stages": {
            "lexical":  { "status": "ready" },
            "semantic": { "status": "ready" },
            "graph":    { "status": "ready" },
        },
        "search_capabilities": ["bm25", "vector", "kg"],
    });
    let search = serde_json::json!({ "results": [] });
    let (base, bodies, _) = spawn_mock_daemon(status, search).await;
    let server = McpServer::new(base);
    let args = serde_json::json!({
        "index_id": "demo",
        "query": "find the AuthValidator",
        "top_k": 7,
    });
    let _ = server.dispatch(req("search_all", args.clone())).await;
    let _ = server.dispatch(req("search", args.clone())).await;

    let bodies = bodies.lock().await;
    assert_eq!(bodies.len(), 2, "both tools must dispatch a search");
    // Compare text / top_k / expand_graph. `search_all` explicitly
    // sets `expand_graph=true`; the legacy `search` tool does NOT set
    // expand_graph in its body (the daemon defaults to true). Both
    // shapes resolve to identical SearchQuery semantics at the daemon.
    assert_eq!(bodies[0]["text"], bodies[1]["text"]);
    assert_eq!(bodies[0]["top_k"], bodies[1]["top_k"]);
    // Daemon-side: SearchQuery::default sets expand_graph=true, so
    // omitting the field is semantically equivalent to setting true.
    let expand_a = bodies[0]
        .get("expand_graph")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let expand_b = bodies[1]
        .get("expand_graph")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    assert!(expand_a && expand_b, "both must expand the graph");
    // Neither pins a stage.
    assert!(bodies[0].get("stage").is_none_or(|v| v.is_null()));
    assert!(bodies[1].get("stage").is_none_or(|v| v.is_null()));
}
