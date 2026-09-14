//! Serialized-response byte ceiling tests for the MCP result tools (#7493).
//!
//! Why: the guarantee is about the bytes a caller receives and about what the
//! response says is missing from them, so every assertion reads the parsed
//! `tools/call` payload rather than any internal state. The fold has to drop
//! WHOLE hits, keep paging honest, and never answer a non-empty result set
//! with an empty one.
//! What: a mock daemon serving oversized `search`, `list_chunks`, `grep`, and
//! `get_call_chain` bodies, plus the descriptor and measurement-failure arms.
//! Test: this module.

use serde_json::Value;

use super::byte_cap::{measure, DEFAULT_MAX_BYTES, HARD_MAX_BYTES};
use super::tests::req;
use super::{tool_descriptors, McpServer};

/// Ceiling that keeps exactly four of [`fixture_hit`]'s ten hits.
///
/// The fixture's hits are uniform and roughly 1.1 KB each once pretty-printed,
/// so four fit under this and five do not. Change the fixture and this
/// constant moves with it — `over_cap_search_returns_whole_hits_and_a_withheld_count`
/// prints the sizes it measured on failure.
const FOUR_HIT_CEILING: usize = 5_000;

/// A search hit large enough that ten of them clear any sane ceiling.
fn fixture_hit(n: usize) -> Value {
    let body = format!(
        "pub fn handler_{n}(req: Request) -> Response {{\n{}\n    route(req)\n}}",
        "    // a chunk body wide enough to matter to a byte ceiling\n".repeat(12)
    );
    serde_json::json!({
        "id": format!("crates/demo/src/handler_{n}.rs::Function::handler_{n}::38::40"),
        "file": format!("/Users/dev/proj/crates/demo/src/handler_{n}.rs"),
        "path": format!("crates/demo/src/handler_{n}.rs"),
        "start_line": 38 + n,
        "end_line": 40 + n,
        "content": body,
        "function_name": format!("handler_{n}"),
        "score": 0.0131_f64,
        "match_reason": "hybrid",
    })
}

fn search_body(hits: usize) -> Value {
    serde_json::json!({
        "results": (0..hits).map(fixture_hit).collect::<Vec<_>>(),
        "intent": "Definition",
        "latency_ms": 12,
        "meta": { "dropped_total": 0 },
    })
}

/// A `list_chunks` page whose `next_cursor` names the LAST FETCHED chunk —
/// the cursor the fold has to rewrite once it drops the tail.
fn chunks_body(count: usize) -> Value {
    let chunks: Vec<Value> = (0..count).map(fixture_hit).collect();
    let last_id = chunks
        .last()
        .and_then(|c| c.get("id"))
        .cloned()
        .unwrap_or(Value::Null);
    serde_json::json!({
        "index_id": "demo",
        "total": 400,
        "offset": 0,
        "limit": count,
        "chunks": chunks,
        "next_cursor": last_id,
    })
}

fn grep_body(count: usize) -> Value {
    serde_json::json!({
        "matches": (0..count).map(fixture_hit).collect::<Vec<_>>(),
        "total": count,
        "truncated": false,
    })
}

/// Mock daemon answering the four capped endpoints this module exercises.
async fn spawn_mock(search: Value, chunks: Value, grep: Value, call_chain: String) -> String {
    use axum::extract::State;
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use std::sync::Arc;

    #[derive(Clone)]
    struct Mock {
        search: Arc<Value>,
        chunks: Arc<Value>,
        grep: Arc<Value>,
        call_chain: Arc<String>,
    }

    async fn search_route(State(s): State<Mock>, Json(_b): Json<Value>) -> Json<Value> {
        Json((*s.search).clone())
    }
    async fn chunks_route(State(s): State<Mock>) -> Json<Value> {
        Json((*s.chunks).clone())
    }
    async fn grep_route(State(s): State<Mock>, Json(_b): Json<Value>) -> Json<Value> {
        Json((*s.grep).clone())
    }
    async fn call_chain_route(State(s): State<Mock>) -> String {
        (*s.call_chain).clone()
    }

    let app = Router::new()
        .route("/indexes/{id}/search", post(search_route))
        .route("/indexes/{id}/chunks", get(chunks_route))
        .route("/indexes/{id}/grep", post(grep_route))
        .route("/indexes/{id}/call_chain", get(call_chain_route))
        .with_state(Mock {
            search: Arc::new(search),
            chunks: Arc::new(chunks),
            grep: Arc::new(grep),
            call_chain: Arc::new(call_chain),
        });

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

/// Spawn a daemon serving only the given search body.
async fn search_daemon(hits: usize) -> McpServer {
    let base = spawn_mock(search_body(hits), Value::Null, Value::Null, String::new()).await;
    McpServer::new(base)
}

/// Dispatch a tool through `tools/call` and return its parsed payload plus
/// the serialized length the MCP client would be charged for.
async fn call(server: &McpServer, tool: &str, arguments: Value) -> (Value, usize) {
    let resp = server
        .dispatch(req(
            "tools/call",
            serde_json::json!({ "name": tool, "arguments": arguments }),
        ))
        .await;
    let result = resp.result.expect("tools/call returns a result envelope");
    assert_eq!(
        result["isError"], false,
        "{tool} must not error: {result:?}"
    );
    let text = result["content"][0]["text"]
        .as_str()
        .expect("text content node")
        .to_string();
    let parsed: Value = serde_json::from_str(&text).expect("payload is JSON");
    (parsed, text.len())
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

/// An over-cap search returns whole hits only, and says how many it withheld.
#[tokio::test]
async fn over_cap_search_returns_whole_hits_and_a_withheld_count() {
    let server = search_daemon(10).await;
    let (payload, len) = call(
        &server,
        "search",
        serde_json::json!({
            "index_id": "demo",
            "query": "handler",
            "max_bytes": FOUR_HIT_CEILING,
        }),
    )
    .await;

    let results = payload["results"].as_array().expect("results array");
    assert_eq!(
        results.len(),
        4,
        "expected four whole hits under a {FOUR_HIT_CEILING}-byte ceiling; \
         got {} at {len} serialized bytes",
        results.len()
    );
    // Whole hits, not a hit cut mid-object.
    for (i, hit) in results.iter().enumerate() {
        assert_eq!(
            hit,
            &fixture_hit(i),
            "hit {i} must be returned verbatim, never partially"
        );
    }
    assert_eq!(payload["meta"]["truncated"], true);
    assert_eq!(payload["meta"]["returned"], 4);
    assert_eq!(payload["meta"]["withheld"], 6);
    let notice = payload["meta"]["truncation_notice"]
        .as_str()
        .expect("a truncation notice");
    assert!(
        notice.contains('6'),
        "the notice counts the withheld: {notice}"
    );
    for knob in ["compact: true", "top_k", "path_prefix", "full: true"] {
        assert!(notice.contains(knob), "the notice names {knob}: {notice}");
    }
}

/// `full: true` disables the ceiling for that call and reports nothing missing.
#[tokio::test]
async fn full_true_returns_every_hit_with_truncated_false() {
    let server = search_daemon(10).await;
    let (payload, _) = call(
        &server,
        "search",
        serde_json::json!({
            "index_id": "demo",
            "query": "handler",
            "max_bytes": FOUR_HIT_CEILING,
            "full": true,
        }),
    )
    .await;

    assert_eq!(payload["results"].as_array().map(Vec::len), Some(10));
    assert_eq!(payload["meta"]["truncated"], false);
    assert!(payload["meta"].get("withheld").is_none());
    assert!(payload["meta"].get("truncation_notice").is_none());
}

/// A `max_bytes` above the hard bound is clamped, and the clamp is reported.
#[tokio::test]
async fn max_bytes_above_the_hard_ceiling_is_clamped_and_reported() {
    let server = search_daemon(10).await;
    let (payload, len) = call(
        &server,
        "search",
        serde_json::json!({
            "index_id": "demo",
            "query": "handler",
            "max_bytes": 4_000_000,
        }),
    )
    .await;

    assert_eq!(payload["meta"]["max_bytes_clamped"], true);
    assert_eq!(payload["meta"]["max_bytes"], HARD_MAX_BYTES);
    // The ten-hit fixture still fits under the clamped ceiling.
    assert!(len < HARD_MAX_BYTES);
    assert_eq!(payload["results"].as_array().map(Vec::len), Some(10));
    assert_eq!(payload["meta"]["truncated"], false);
}

/// A `max_bytes` that is not a byte count is rejected, never silently ignored.
#[tokio::test]
async fn a_malformed_max_bytes_is_rejected() {
    let server = search_daemon(10).await;
    let msg = call_err(
        &server,
        "search",
        serde_json::json!({ "index_id": "demo", "query": "handler", "max_bytes": "lots" }),
    )
    .await;
    assert!(msg.contains("max_bytes"), "{msg}");
}

/// A single result larger than the whole ceiling comes back alone and flagged
/// — a non-empty match set never folds to an empty one.
#[tokio::test]
async fn a_single_result_larger_than_the_cap_is_returned_alone() {
    let server = search_daemon(1).await;
    let (payload, _) = call(
        &server,
        "search",
        serde_json::json!({ "index_id": "demo", "query": "handler", "max_bytes": 64 }),
    )
    .await;

    assert_eq!(payload["results"].as_array().map(Vec::len), Some(1));
    assert_eq!(payload["results"][0], fixture_hit(0));
    assert_eq!(payload["meta"]["truncated"], true);
    assert_eq!(payload["meta"]["withheld"], 0);
    assert!(payload["meta"]["truncation_notice"]
        .as_str()
        .is_some_and(|n| n.contains("empty result")));
}

/// After a capped `list_chunks` page, the cursor names the last RETURNED
/// chunk, so the next page starts at the first withheld one.
#[tokio::test]
async fn list_chunks_cursor_points_at_the_last_returned_chunk() {
    let base = spawn_mock(Value::Null, chunks_body(10), Value::Null, String::new()).await;
    let server = McpServer::new(base);
    let (payload, _) = call(
        &server,
        "list_chunks",
        serde_json::json!({
            "index_id": "demo",
            "after": "crates/demo/src/handler_0.rs::Function::handler_0::38::40",
            "max_bytes": FOUR_HIT_CEILING,
        }),
    )
    .await;

    let returned = payload["chunks"].as_array().expect("chunks array");
    assert!(
        !returned.is_empty() && returned.len() < 10,
        "the page folded"
    );
    let last = returned.last().expect("a last chunk");
    assert_eq!(
        payload["next_cursor"], last["id"],
        "the cursor must name the last chunk this response returned"
    );
    assert_eq!(payload["meta"]["truncated"], true);
}

/// Offset-mode paging cannot use an id cursor, so the fold reports the page
/// size it actually returned instead.
#[tokio::test]
async fn list_chunks_offset_paging_resumes_at_the_first_withheld_chunk() {
    let base = spawn_mock(Value::Null, chunks_body(10), Value::Null, String::new()).await;
    let server = McpServer::new(base);
    let (payload, _) = call(
        &server,
        "list_chunks",
        serde_json::json!({
            "index_id": "demo",
            "limit": 10,
            "max_bytes": FOUR_HIT_CEILING,
        }),
    )
    .await;

    let returned = payload["chunks"].as_array().expect("chunks array").len();
    assert!(returned < 10, "the page folded");
    assert_eq!(
        payload["limit"], returned,
        "offset + limit must land on the first withheld chunk"
    );
}

/// `grep` folds its `matches` array the same way, without disturbing the
/// daemon's own top-level `truncated` flag.
#[tokio::test]
async fn grep_matches_fold_to_the_ceiling() {
    let base = spawn_mock(Value::Null, Value::Null, grep_body(10), String::new()).await;
    let server = McpServer::new(base);
    let (payload, _) = call(
        &server,
        "grep",
        serde_json::json!({
            "index_id": "demo",
            "pattern": "handler",
            "max_bytes": FOUR_HIT_CEILING,
        }),
    )
    .await;

    let matches = payload["matches"].as_array().expect("matches array").len();
    assert!(matches > 0 && matches < 10, "got {matches} matches");
    assert_eq!(payload["meta"]["truncated"], true);
    assert_eq!(payload["meta"]["withheld"], 10 - matches);
    assert_eq!(
        payload["truncated"], false,
        "the daemon's own grep flag is untouched"
    );
}

/// `get_call_chain` returns one indivisible prose tree: over the ceiling it is
/// still returned, flagged, with the knobs that would shrink it.
#[tokio::test]
async fn get_call_chain_over_the_cap_is_flagged_not_dropped() {
    let tree = "fn handler(req: Request) -> Response\n".repeat(400);
    let base = spawn_mock(Value::Null, Value::Null, Value::Null, tree.clone()).await;
    let server = McpServer::new(base);
    let (payload, _) = call(
        &server,
        "get_call_chain",
        serde_json::json!({ "index_id": "demo", "entry_point": "handler", "max_bytes": 512 }),
    )
    .await;

    assert_eq!(payload["text"], tree, "the tree is not cut mid-body");
    assert_eq!(payload["meta"]["truncated"], true);
    assert_eq!(payload["meta"]["withheld"], 0);
    assert!(payload["meta"]["truncation_notice"]
        .as_str()
        .is_some_and(|n| n.contains("max_depth")));
}

/// A response under the ceiling keeps its shape; only `meta.truncated` is new.
#[tokio::test]
async fn an_under_cap_response_is_untouched_apart_from_the_flag() {
    let server = search_daemon(2).await;
    let (payload, len) = call(
        &server,
        "search",
        serde_json::json!({ "index_id": "demo", "query": "handler" }),
    )
    .await;

    assert!(len < DEFAULT_MAX_BYTES, "fixture must fit the default cap");
    assert_eq!(payload["results"], search_body(2)["results"]);
    assert_eq!(payload["intent"], "Definition");
    assert_eq!(payload["meta"]["dropped_total"], 0);
    assert_eq!(payload["meta"]["truncated"], false);
    assert!(payload["meta"].get("returned").is_none());
}

/// Every capped tool advertises the two knobs it honours.
#[test]
fn every_capped_tool_advertises_max_bytes_and_full() {
    let defs = tool_descriptors();
    let tools = defs.as_array().expect("descriptor array");
    for name in [
        "search",
        "search_lexical",
        "search_semantic",
        "search_kg",
        "search_all",
        "list_chunks",
        "grep",
        "get_call_chain",
    ] {
        let tool = tools
            .iter()
            .find(|t| t["name"] == name)
            .unwrap_or_else(|| panic!("{name} is missing from tools/list"));
        let props = &tool["inputSchema"]["properties"];
        assert!(
            props["max_bytes"]["type"] == "integer",
            "{name} must advertise max_bytes"
        );
        assert_eq!(props["max_bytes"]["default"], DEFAULT_MAX_BYTES);
        assert!(
            props["full"]["type"] == "boolean",
            "{name} must advertise full"
        );
    }
    // An uncapped tool gains neither knob.
    let health = tools
        .iter()
        .find(|t| t["name"] == "search_health")
        .expect("search_health");
    assert!(health["inputSchema"]["properties"]
        .get("max_bytes")
        .is_none());
}

/// A measurement that cannot be taken is an error, never a licence to return
/// the body unmeasured.
///
/// `serde_json::Value` itself always serializes, so the reachable failure
/// point is the measurement helper the fold calls with `?`; this pins its
/// error arm on a value serde_json genuinely rejects — a map keyed by a tuple,
/// which has no JSON object-key form (an integer key would be stringified).
#[test]
fn a_measurement_failure_returns_an_error_not_an_unmeasured_body() {
    use std::collections::HashMap;

    let unserializable: HashMap<(u8, u8), &str> = HashMap::from([((1u8, 2u8), "hit")]);
    let err = measure(&unserializable).expect_err("a tuple map key cannot serialize");
    let msg = format!("{err:?}");
    assert!(msg.contains("measurement failed"), "{msg}");
    assert!(msg.contains("unmeasured"), "{msg}");
}
