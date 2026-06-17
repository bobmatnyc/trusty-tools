//! Tests for `tools/list` completeness and per-lane dispatch validation.
//!
//! Why: validates that the four per-lane search tools (#138) plus the legacy
//! `search` tool all appear in `tools/list` with correct descriptions and
//! schemas, that `summarise_stages` renders correctly, and that `search_all`
//! without `index_id` fans out to the global `/search` endpoint.
//! What: unit/integration tests using a mock base URL or a tiny axum mock
//! daemon; no shared state with the core tests file.
//! Test: this file.

use serde_json::Value;

use super::tests::req;
use super::{error_codes, McpServer};

/// `summarise_stages` renders the three known keys in lexical →
/// semantic → graph order and Title-cases snake_case statuses.
#[test]
fn summarise_stages_renders_in_order() {
    use super::search::summarise_stages;
    let stages = serde_json::json!({
        "lexical":  { "status": "ready" },
        "semantic": { "status": "in_progress" },
        "graph":    { "status": "pending" },
    });
    let s = summarise_stages(&stages);
    assert_eq!(s, "lexical=Ready, semantic=InProgress, graph=Pending");
}

/// `tools/list` returns five search tools after #138 (legacy `search`
/// plus the four per-lane tools). Bumps the original
/// `test_tools_list_complete` assertion.
#[tokio::test]
async fn tools_list_returns_five_search_tools() {
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
        "search_lexical",
        "search_semantic",
        "search_kg",
        "search_all",
    ] {
        assert!(
            names.contains(&required),
            "tools/list missing '{required}' (got {names:?})"
        );
    }
    // Spec: exactly five "search*" tools (the four new + legacy).
    let search_tools: Vec<&str> = names
        .iter()
        .copied()
        .filter(|n| *n == "search" || n.starts_with("search_"))
        .collect();
    // `search_similar` and `search_health` also start with "search_"
    // but are distinct surfaces; assert only on the lane-related ones.
    let lane_tools: Vec<&str> = names
        .iter()
        .copied()
        .filter(|n| {
            matches!(
                *n,
                "search" | "search_lexical" | "search_semantic" | "search_kg" | "search_all"
            )
        })
        .collect();
    assert_eq!(
        lane_tools.len(),
        5,
        "expected exactly 5 lane-related search tools, got {lane_tools:?} (all: {search_tools:?})"
    );
}

/// Each per-lane tool description embeds the authoring-guide hook
/// (when-to-use phrasing) so the LLM can pick reliably.
#[tokio::test]
async fn per_lane_tool_descriptions_carry_when_to_use_hooks() {
    let server = McpServer::new("http://127.0.0.1:1");
    let resp = server.dispatch(req("tools/list", Value::Null)).await;
    let result = resp.result.expect("expected result");
    let tools = result
        .get("tools")
        .and_then(Value::as_array)
        .expect("array");
    for (name, hook) in [
        ("search_lexical", "exact symbol name"),
        ("search_semantic", "by meaning"),
        ("search_kg", "from a known seed"),
        ("search_all", "When in doubt"),
    ] {
        let tool = tools
            .iter()
            .find(|t| t.get("name").and_then(Value::as_str) == Some(name))
            .unwrap_or_else(|| panic!("tool {name} missing"));
        let desc = tool["description"].as_str().expect("description");
        assert!(
            desc.contains(hook),
            "tool {name} description must mention '{hook}': {desc}"
        );
    }
}

/// Missing-arg fast-fail: every per-lane tool rejects an empty arg
/// object before any HTTP round-trip.
#[tokio::test]
async fn per_lane_tools_require_index_id_and_query() {
    let server = McpServer::new("http://127.0.0.1:1");
    for tool in ["search_lexical", "search_semantic", "search_kg"] {
        let resp = server.dispatch(req(tool, serde_json::json!({}))).await;
        let err = resp.error.expect("expected error");
        assert_eq!(
            err.code,
            error_codes::INVALID_PARAMS,
            "{tool} must reject empty args"
        );
    }
}

/// `search_all` without `index_id` keeps the legacy fan-out behaviour
/// (issue #10) — the tool's input schema requires `query` only, and
/// the daemon's `POST /search` endpoint is responsible for the fan-out
/// logic.
#[tokio::test]
async fn search_all_without_index_id_calls_global_fanout_endpoint() {
    // Mock daemon that returns a fan-out response from POST /search.
    use axum::routing::post;
    use axum::{Json, Router};
    use std::sync::Arc;
    use tokio::sync::Mutex;

    let captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let captured_clone = Arc::clone(&captured);

    async fn fanout_handler(
        axum::extract::State(captured): axum::extract::State<Arc<Mutex<Vec<String>>>>,
        Json(_body): Json<Value>,
    ) -> Json<Value> {
        captured.lock().await.push("/search".into());
        Json(serde_json::json!({ "results": [] }))
    }

    let app = Router::new()
        .route("/search", post(fanout_handler))
        .with_state(captured_clone);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let server = McpServer::new(format!("http://{addr}"));
    let resp = server
        .dispatch(req(
            "search_all",
            serde_json::json!({ "query": "anything" }),
        ))
        .await;
    assert!(resp.error.is_none());
    assert_eq!(captured.lock().await.as_slice(), &["/search".to_string()]);
}

// Issue #1373 — pinned-index descriptor advertisement
// ----------------------------------------------------------------

/// Find a tool's inputSchema in a `tool_descriptors*` array by name.
fn schema_of<'a>(tools: &'a Value, name: &str) -> &'a Value {
    tools
        .as_array()
        .expect("descriptors array")
        .iter()
        .find(|t| t.get("name").and_then(Value::as_str) == Some(name))
        .unwrap_or_else(|| panic!("tool {name} missing"))
        .get("inputSchema")
        .expect("inputSchema")
}

/// `tool_descriptors_pinned(None)` is byte-identical to `tool_descriptors()`.
///
/// Why: the no-pin path must be a pure pass-through so legacy sessions see
/// exactly the same catalogue they always have.
/// What: asserts equality of the two values.
/// Test: this is the test.
#[test]
fn tool_descriptors_pinned_none_is_unchanged() {
    use super::descriptors::{tool_descriptors, tool_descriptors_pinned};
    assert_eq!(tool_descriptors_pinned(None), tool_descriptors());
}

/// When pinned, `search`'s `inputSchema` no longer lists `index_id` as
/// required (the pin supplies it) but `query` still is.
///
/// Why: the LLM should be able to call `search` with only a `query` once the
/// session is pinned — that is what makes a bare search resolve to the right
/// index.
/// What: pins to `trusty-tools` and asserts `required` lost `index_id` but kept
/// `query`.
/// Test: this is the test.
#[test]
fn pinned_descriptors_make_index_id_optional() {
    use super::descriptors::tool_descriptors_pinned;
    let pinned = tool_descriptors_pinned(Some("trusty-tools"));
    let schema = schema_of(&pinned, "search");
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .expect("required")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(
        !required.contains(&"index_id"),
        "index_id should not be required when pinned: {required:?}"
    );
    assert!(
        required.contains(&"query"),
        "query stays required: {required:?}"
    );
}

/// When pinned, the `index_id` property description names the pinned default.
///
/// Why: the schema description is a first-class prompt — telling the model what
/// `index_id` defaults to means it never has to call `list_indexes`.
/// What: pins to `trusty-tools` and asserts the `search` `index_id` description
/// mentions the pinned id.
/// Test: this is the test.
#[test]
fn pinned_descriptors_annotate_index_id() {
    use super::descriptors::tool_descriptors_pinned;
    let pinned = tool_descriptors_pinned(Some("trusty-tools"));
    let schema = schema_of(&pinned, "search");
    let desc = schema
        .get("properties")
        .and_then(|p| p.get("index_id"))
        .and_then(|i| i.get("description"))
        .and_then(Value::as_str)
        .expect("index_id description");
    assert!(
        desc.contains("trusty-tools") && desc.contains("pinned"),
        "index_id description should name the pinned default: {desc:?}"
    );
    // The note must be joined with exactly one separating period — never a
    // double-period, regardless of whether the base description ended in `.`.
    assert!(
        !desc.contains(".."),
        "annotated description must not contain a double-period: {desc:?}"
    );
    assert!(
        desc.contains(". Defaults to this session's pinned project index"),
        "note must follow exactly one period+space: {desc:?}"
    );
}

/// `tools/list` over a PINNED server advertises the optional `index_id`.
///
/// Why: end-to-end — the dispatcher must feed its `pinned_index` into the
/// descriptor builder so the wire `tools/list` reflects the pin.
/// What: dispatches `tools/list` on a pinned server and asserts `search`'s
/// schema dropped `index_id` from `required`.
/// Test: this is the test.
#[tokio::test]
async fn tools_list_reflects_session_pin() {
    let server = McpServer::new("http://127.0.0.1:1").with_pinned_index("trusty-tools");
    let resp = server.dispatch(req("tools/list", Value::Null)).await;
    let tools = resp
        .result
        .expect("result")
        .get("tools")
        .cloned()
        .expect("tools");
    let schema = schema_of(&tools, "search");
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .expect("required")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(
        !required.contains(&"index_id"),
        "pin must reach tools/list: {required:?}"
    );
}
