//! Compact field mode tests for the MCP search tools (#7676).
//!
//! Why: the guarantee is about the JSON a caller receives, not about
//! `CodeChunk`'s struct definition — a dropped field must be ABSENT, not
//! `null`, and a default call must keep the shape every existing caller
//! already parses. Both are only observable on the serialized envelope, so
//! every assertion here reads keys off `tools/call`'s text payload.
//! What: one per-tool omission test, one default-shape test, and the
//! before/after size measurement the issue's closure condition names.
//! Test: this module.

use serde_json::Value;

use super::compact::DROPPED_FIELDS;
use super::tests::{req, spawn_mock_daemon};
use super::McpServer;

/// The six keys compact mode keeps.
const KEPT_FIELDS: [&str; 6] = [
    "path",
    "start_line",
    "end_line",
    "compact_snippet",
    "score",
    "match_reason",
];

/// A hit carrying every field the daemon actually emits, so the omission
/// assertions are proving removal rather than absence-by-fixture.
fn full_hit(n: usize) -> Value {
    let body = format!("pub fn handler_{n}(req: Request) -> Response {{\n    route(req)\n}}");
    serde_json::json!({
        "calls": ["route", "Response::new"],
        "chunk_depth": 1,
        "chunk_type": "Function",
        "compact_snippet": body,
        "content": body,
        "end_line": 40 + n,
        "file": format!("/Users/dev/proj/crates/demo/src/handler_{n}.rs"),
        "function_name": format!("handler_{n}"),
        "id": format!("crates/demo/src/handler_{n}.rs::Function::handler_{n}::38::40"),
        "inherits_from": [],
        "language": "rust",
        "match_reason": "hybrid",
        "on_branch": false,
        "path": format!("crates/demo/src/handler_{n}.rs"),
        "score": 0.0131_f64,
        "start_line": 38 + n,
    })
}

fn search_body(hits: usize) -> Value {
    serde_json::json!({
        "results": (0..hits).map(full_hit).collect::<Vec<_>>(),
        "intent": "Definition",
        "latency_ms": 12,
        "meta": { "dropped_total": 0, "results_may_be_stale": false },
    })
}

fn ready_status() -> Value {
    serde_json::json!({
        "stages": {
            "lexical":  { "status": "ready" },
            "semantic": { "status": "ready" },
            "graph":    { "status": "ready" },
        },
        "search_capabilities": ["bm25", "literal", "exact_match", "vector", "kg"],
    })
}

/// Dispatch one tool through `tools/call` and return the parsed payload the
/// MCP client would see.
async fn call(server: &McpServer, tool: &str, arguments: Value) -> (Value, usize) {
    let resp = server
        .dispatch(req(
            "tools/call",
            serde_json::json!({ "name": tool, "arguments": arguments }),
        ))
        .await;
    let result = resp.result.expect("tools/call returns a result envelope");
    assert_eq!(result["isError"], false, "{tool} must not error");
    let text = result["content"][0]["text"]
        .as_str()
        .expect("text content node")
        .to_string();
    let parsed: Value = serde_json::from_str(&text).expect("payload is JSON");
    (parsed, text.len())
}

fn hit_keys(payload: &Value) -> Vec<String> {
    payload["results"][0]
        .as_object()
        .expect("first hit is an object")
        .keys()
        .cloned()
        .collect()
}

/// Every dropped field is ABSENT from a compact hit, every kept field is
/// present, and `meta` is untouched.
fn assert_compact_shape(payload: &Value, tool: &str) {
    let keys = hit_keys(payload);
    for dropped in DROPPED_FIELDS {
        assert!(
            !keys.contains(&dropped.to_string()),
            "{tool}: compact hit must omit `{dropped}` entirely (not null); keys = {keys:?}"
        );
    }
    for kept in KEPT_FIELDS {
        assert!(
            keys.contains(&kept.to_string()),
            "{tool}: compact hit must keep `{kept}`; keys = {keys:?}"
        );
    }
    assert_eq!(
        payload["meta"]["dropped_total"], 0,
        "{tool}: the meta block is not part of the compaction"
    );
}

/// `search` with `compact: true` returns hits carrying only the six kept keys,
/// and pins the daemon's own `compact` flag so `compact_snippet` is there to
/// replace the `content` it drops.
#[tokio::test]
async fn search_compact_hit_omits_the_dropped_keys() {
    let (base, bodies, _paths) = spawn_mock_daemon(ready_status(), search_body(1)).await;
    let server = McpServer::new(base);

    let (payload, _) = call(
        &server,
        "search",
        serde_json::json!({ "index_id": "demo", "query": "handler", "compact": true }),
    )
    .await;
    assert_compact_shape(&payload, "search");

    let bodies = bodies.lock().await;
    assert_eq!(
        bodies[0]["compact"], true,
        "the daemon body pins compact so the snippet exists"
    );
}

/// Same guarantee on the lexical lane.
#[tokio::test]
async fn search_lexical_compact_hit_omits_the_dropped_keys() {
    let (base, bodies, _paths) = spawn_mock_daemon(ready_status(), search_body(1)).await;
    let server = McpServer::new(base);

    let (payload, _) = call(
        &server,
        "search_lexical",
        serde_json::json!({ "index_id": "demo", "query": "handler_0", "compact": true }),
    )
    .await;
    assert_compact_shape(&payload, "search_lexical");

    let bodies = bodies.lock().await;
    assert_eq!(bodies[0]["compact"], true);
    assert_eq!(bodies[0]["stage"], "lexical", "the lane pin still holds");
}

/// The semantic and graph lanes route through the same `run_lane_search`, so
/// they honour `compact` too — and their descriptors advertise it.
#[tokio::test]
async fn semantic_and_kg_lanes_honour_compact() {
    for tool in ["search_semantic", "search_kg"] {
        let (base, bodies, _paths) = spawn_mock_daemon(ready_status(), search_body(1)).await;
        let server = McpServer::new(base);
        let (payload, _) = call(
            &server,
            tool,
            serde_json::json!({ "index_id": "demo", "query": "handler", "compact": true }),
        )
        .await;
        assert_compact_shape(&payload, tool);
        assert_eq!(bodies.lock().await[0]["compact"], true);
    }
}

/// Same guarantee on the cross-project fan-out, where `compact` also has to
/// win over `full_content` — the two ask for opposite things.
#[tokio::test]
async fn search_all_fanout_compact_hit_omits_the_dropped_keys() {
    let (base, bodies, paths) = spawn_mock_daemon(ready_status(), search_body(1)).await;
    let server = McpServer::new(base);

    let (payload, _) = call(
        &server,
        "search_all",
        serde_json::json!({ "query": "handler", "compact": true, "full_content": true }),
    )
    .await;
    assert_compact_shape(&payload, "search_all");

    assert_eq!(paths.lock().await[0], "/search", "no index → fan-out path");
    let bodies = bodies.lock().await;
    assert_eq!(
        bodies[0]["full_content"], false,
        "compact overrides full_content, so the fan-out still builds a snippet"
    );
}

/// A call that omits `compact` keeps the pre-#7676 hit shape on all five
/// tools — every field still present, and no `compact` key injected into the
/// daemon body.
///
/// #7493 added the byte ceiling, whose only mark on an under-cap response is
/// `meta.truncated: false`; this pins that it is the ONLY difference.
#[tokio::test]
async fn default_calls_keep_the_legacy_hit_shape() {
    let baseline: Vec<String> = full_hit(0)
        .as_object()
        .expect("fixture is an object")
        .keys()
        .cloned()
        .collect();
    let mut meta_baseline: Vec<String> = search_body(1)["meta"]
        .as_object()
        .expect("fixture meta is an object")
        .keys()
        .cloned()
        .collect();
    meta_baseline.push("truncated".to_string());
    meta_baseline.sort();

    for (tool, args) in [
        (
            "search",
            serde_json::json!({ "index_id": "demo", "query": "handler" }),
        ),
        (
            "search_lexical",
            serde_json::json!({ "index_id": "demo", "query": "handler_0" }),
        ),
        (
            "search_semantic",
            serde_json::json!({ "index_id": "demo", "query": "handler" }),
        ),
        (
            "search_kg",
            serde_json::json!({ "index_id": "demo", "query": "handler_0" }),
        ),
        ("search_all", serde_json::json!({ "query": "handler" })),
    ] {
        let (base, bodies, _paths) = spawn_mock_daemon(ready_status(), search_body(1)).await;
        let server = McpServer::new(base);
        let (payload, _) = call(&server, tool, args).await;
        assert_eq!(
            hit_keys(&payload),
            baseline,
            "{tool}: a default call must return the daemon's hit verbatim"
        );
        assert!(
            bodies.lock().await[0].get("compact").is_none(),
            "{tool}: a default call must not inject `compact` into the daemon body"
        );
        // #7493: the byte ceiling adds `meta.truncated: false` and nothing else.
        let mut meta_keys: Vec<String> = payload["meta"]
            .as_object()
            .expect("meta survives the default call")
            .keys()
            .cloned()
            .collect();
        meta_keys.sort();
        assert_eq!(
            meta_keys, meta_baseline,
            "{tool}: `meta.truncated` must be the only added key"
        );
        assert_eq!(payload["meta"]["truncated"], false, "{tool}");
    }
}

/// The issue's closure condition: a 10-hit `search` costs roughly half as many
/// bytes in compact mode.
#[tokio::test]
async fn compact_halves_the_bytes_of_a_ten_hit_search() {
    let (base, _bodies, _paths) = spawn_mock_daemon(ready_status(), search_body(10)).await;
    let server = McpServer::new(base);
    let args = serde_json::json!({ "index_id": "demo", "query": "handler" });

    let (_, full_len) = call(&server, "search", args.clone()).await;
    let mut compact_args = args;
    compact_args["compact"] = Value::Bool(true);
    let (payload, compact_len) = call(&server, "search", compact_args).await;

    assert_eq!(payload["results"].as_array().map(Vec::len), Some(10));
    println!(
        "search, 10 hits: {full_len} bytes default -> {compact_len} bytes compact \
         ({:.1}% saved)",
        100.0 - (compact_len as f64 / full_len as f64) * 100.0
    );
    assert!(
        compact_len * 2 <= full_len,
        "compact must at least halve the payload: {full_len} -> {compact_len}"
    );
}

/// Dispatch a tool expected to fail, returning the in-band error text.
async fn call_err(server: &McpServer, tool: &str, arguments: Value) -> String {
    let resp = server
        .dispatch(req(
            "tools/call",
            serde_json::json!({ "name": tool, "arguments": arguments }),
        ))
        .await;
    let result = resp.result.expect("tools/call returns a result envelope");
    assert_eq!(result["isError"], true, "{tool} was expected to error");
    result["content"][0]["text"]
        .as_str()
        .expect("text content node")
        .to_string()
}

/// A `compact` that is not a boolean is rejected on every tool that takes it,
/// exactly like `full` — it used to coerce to `false` and return full hits
/// byte-identical to a call that never asked for compaction.
#[tokio::test]
async fn a_non_boolean_compact_is_rejected() {
    for (tool, mut args) in [
        (
            "search",
            serde_json::json!({ "index_id": "demo", "query": "handler" }),
        ),
        (
            "search_lexical",
            serde_json::json!({ "index_id": "demo", "query": "handler_0" }),
        ),
        (
            "search_semantic",
            serde_json::json!({ "index_id": "demo", "query": "handler" }),
        ),
        (
            "search_kg",
            serde_json::json!({ "index_id": "demo", "query": "handler_0" }),
        ),
        ("search_all", serde_json::json!({ "query": "handler" })),
    ] {
        let (base, _bodies, _paths) = spawn_mock_daemon(ready_status(), search_body(1)).await;
        let server = McpServer::new(base);
        args["compact"] = Value::String("true".into());
        let msg = call_err(&server, tool, args).await;
        assert!(msg.contains("compact must be a boolean"), "{tool}: {msg}");
    }
}
