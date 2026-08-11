//! `INDEX_NOT_READY` contract tests (issue #4715).
//!
//! Why: the defect being fixed is a state reported as a different state — a
//! never-indexed worktree answering `404 unknown index`, which reads as
//! permanent and induces a silent workaround. These tests pin the three
//! properties that make the new answer honest: it is structurally
//! distinguishable from an unknown index, it is structurally distinguishable
//! from a zero-hit result set, and a failure of the readiness check itself
//! degrades to an error rather than to "ready" or "empty".
//! What: drives `McpServer::dispatch` against mock daemons that 404, that
//! return an empty result set, and that return a 500.
//! Test: this file.

use serde_json::Value;

use super::tests::req;
use super::{McpServer, INDEX_NOT_READY, INDEX_NOT_READY_CODE};

/// Spin up a mock daemon that answers every route with `status` and `body`.
///
/// Why: the not-ready path is defined by the daemon's HTTP status, so each
/// test needs to fix that status precisely — `spawn_mock_daemon` in `tests.rs`
/// always answers 200 and cannot express it.
/// What: returns the base URL of a loopback listener serving `status`/`body`
/// on the search, grep, status, chunks, index-file, remove-file, and call_chain
/// routes — one per HTTP verb the transport helpers use.
/// Test: used by every test in this file and in `tests_unavailable.rs`.
pub(super) async fn spawn_status_daemon(status: u16, body: Value) -> String {
    use axum::extract::State;
    use axum::http::StatusCode;
    use axum::routing::{get, post};
    use axum::{Json, Router};

    #[derive(Clone)]
    struct S {
        status: StatusCode,
        body: Value,
    }

    async fn handler(State(s): State<S>) -> (StatusCode, Json<Value>) {
        (s.status, Json(s.body.clone()))
    }

    let state = S {
        status: StatusCode::from_u16(status).expect("valid status"),
        body,
    };
    let app = Router::new()
        .route("/indexes/{id}/search", post(handler))
        .route("/indexes/{id}/grep", post(handler))
        .route("/indexes/{id}/status", get(handler))
        .route("/indexes/{id}/chunks", get(handler))
        // #5350: the same fixed-status daemon serves the write and call-chain
        // routes, so the 503 contract can be asserted on every verb.
        .route("/indexes/{id}/index-file", post(handler))
        .route("/indexes/{id}/remove-file", post(handler))
        .route("/indexes/{id}/call_chain", get(handler))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

/// Pull the `_meta` block out of a `tools/call` result, if present.
fn meta(resp: &super::Response) -> Option<Value> {
    resp.result.as_ref()?.get("_meta").cloned()
}

/// A `tools/call` search against a never-indexed pinned index returns the
/// structured `INDEX_NOT_READY` envelope, not the daemon's raw 404 text.
#[tokio::test]
async fn tools_call_search_on_unindexed_pin_returns_structured_not_ready() {
    let base =
        spawn_status_daemon(404, serde_json::json!({ "error": "unknown index: wt-1" })).await;
    let server = McpServer::new(base).with_pinned_index("wt-1");

    let resp = server
        .dispatch(req(
            "tools/call",
            serde_json::json!({
                "name": "search",
                "arguments": { "query": "classify_index_miss" },
            }),
        ))
        .await;

    assert!(
        resp.error.is_none(),
        "tool failures ride in-band, not as JSON-RPC errors"
    );
    let result = resp
        .result
        .clone()
        .expect("tools/call always returns a result");
    assert_eq!(result["isError"], Value::Bool(true));

    let m = meta(&resp).expect("_meta must carry the machine-readable contract");
    assert_eq!(m["error_code"], INDEX_NOT_READY);
    assert_eq!(m["state"], "not_indexed");
    assert_eq!(m["index_id"], "wt-1");
    assert_eq!(m["retryable"], Value::Bool(true));
    assert_eq!(
        m["suggested_fallback"],
        serde_json::json!(["grep", "find"]),
        "the caller must learn the fallback from the payload, not out of band"
    );
}

/// The bare-method form carries the same payload under `error.data` with the
/// app-level code, so a caller never has to parse the message string.
#[tokio::test]
async fn bare_method_search_on_unindexed_pin_returns_index_not_ready_code() {
    let base =
        spawn_status_daemon(404, serde_json::json!({ "error": "unknown index: wt-1" })).await;
    let server = McpServer::new(base).with_pinned_index("wt-1");

    let resp = server
        .dispatch(req("search", serde_json::json!({ "query": "anything" })))
        .await;

    let err = resp
        .error
        .expect("bare-method failures are JSON-RPC errors");
    assert_eq!(err.code, INDEX_NOT_READY_CODE);
    assert_ne!(
        err.code,
        super::error_codes::METHOD_NOT_FOUND,
        "not-ready must not share a code with any 'does not exist' condition"
    );
    let data = err.data.expect("structured payload rides in error.data");
    assert_eq!(data["error_code"], INDEX_NOT_READY);
    assert_eq!(data["state"], "not_indexed");
    assert_eq!(data["retryable"], Value::Bool(true));
}

/// Fail-open guard: a not-ready answer must never look like a search that ran
/// and found nothing.
///
/// Why: "zero hits" and "not indexed" are different answers. If they were
/// indistinguishable, the fix would reproduce the very bug it exists to
/// correct — a state misreported as another state.
/// What: runs the same call against a daemon that returns an empty result set
/// and against one that 404s, then asserts the two responses differ in the two
/// fields a caller branches on (`isError` and `_meta.error_code`).
/// Test: this is the test.
#[tokio::test]
async fn not_ready_is_distinguishable_from_an_empty_result_set() {
    let empty_base = spawn_status_daemon(
        200,
        serde_json::json!({ "results": [], "intent": "Definition", "latency_ms": 3 }),
    )
    .await;
    let empty = McpServer::new(empty_base).with_pinned_index("wt-1");
    let empty_resp = empty
        .dispatch(req(
            "tools/call",
            serde_json::json!({ "name": "search", "arguments": { "query": "q" } }),
        ))
        .await;

    let empty_result = empty_resp.result.clone().expect("result envelope");
    assert_eq!(
        empty_result["isError"],
        Value::Bool(false),
        "an empty result set is a success"
    );
    assert!(
        meta(&empty_resp).is_none(),
        "an empty result set must carry no not-ready marker"
    );

    let missing_base =
        spawn_status_daemon(404, serde_json::json!({ "error": "unknown index: wt-1" })).await;
    let missing = McpServer::new(missing_base).with_pinned_index("wt-1");
    let missing_resp = missing
        .dispatch(req(
            "tools/call",
            serde_json::json!({ "name": "search", "arguments": { "query": "q" } }),
        ))
        .await;

    let missing_result = missing_resp.result.clone().expect("result envelope");
    assert_eq!(missing_result["isError"], Value::Bool(true));
    assert_eq!(
        meta(&missing_resp).expect("not-ready marker")["error_code"],
        INDEX_NOT_READY
    );
}

/// Fail-open guard: when the daemon fails for any reason other than "this
/// index does not exist", the caller still gets an error — never "ready", and
/// never an empty result set.
#[tokio::test]
async fn readiness_check_failure_does_not_degrade_to_ready_or_empty() {
    // Server-side failure.
    let base = spawn_status_daemon(500, serde_json::json!({ "error": "boom" })).await;
    let server = McpServer::new(base).with_pinned_index("wt-1");
    let resp = server
        .dispatch(req("search", serde_json::json!({ "query": "q" })))
        .await;
    let err = resp.error.expect("a 500 is still an error");
    assert_ne!(
        err.code, INDEX_NOT_READY_CODE,
        "a server fault must not be reported as a missing index"
    );
    assert!(resp.result.is_none(), "no result body on failure");

    // Transport failure: nothing is listening on port 1.
    let dead = McpServer::new("http://127.0.0.1:1").with_pinned_index("wt-1");
    let dead_resp = dead
        .dispatch(req("search", serde_json::json!({ "query": "q" })))
        .await;
    assert!(
        dead_resp.error.is_some(),
        "an unreachable daemon is an error, not an empty answer"
    );
    assert!(dead_resp.result.is_none());
}

/// A 404 on an id the session never advertised stays a genuine unknown index.
///
/// Why: the whole distinction collapses if every 404 becomes "not ready". Only
/// the id the daemon told the caller to use earns the retryable answer.
/// What: asserts the pinned id classifies, a foreign id does not, and an
/// unpinned session never classifies.
/// Test: this is the test.
#[test]
fn classify_index_miss_only_fires_for_the_pinned_index() {
    let pinned = McpServer::new("http://127.0.0.1:1").with_pinned_index("wt-1");
    assert!(pinned.classify_index_miss(Some("wt-1")).is_some());
    assert!(
        pinned
            .classify_index_miss(Some("some-other-index"))
            .is_none(),
        "an id the caller invented is genuinely unknown"
    );
    assert!(pinned.classify_index_miss(None).is_none());

    let unpinned = McpServer::new("http://127.0.0.1:1");
    assert!(unpinned.classify_index_miss(Some("wt-1")).is_none());
}

/// `index_status` on the advertised default reports not-ready, matching
/// `search` — issue #4715 observed both 404ing on the same id.
#[tokio::test]
async fn index_status_on_unindexed_pin_returns_index_not_ready() {
    let base =
        spawn_status_daemon(404, serde_json::json!({ "error": "unknown index: wt-1" })).await;
    let server = McpServer::new(base).with_pinned_index("wt-1");
    let resp = server
        .dispatch(req("index_status", serde_json::json!({})))
        .await;
    let err = resp.error.expect("index_status must fail loudly");
    assert_eq!(err.code, INDEX_NOT_READY_CODE);
}

/// `list_chunks` reports the same state as its `index_status` neighbour.
///
/// Why: routing it is only safe because `get_index_chunks_handler` now rules
/// out the cold store before it 404s — see `service::server::tests_4715`.
#[tokio::test]
async fn list_chunks_on_unindexed_pin_returns_index_not_ready() {
    let base =
        spawn_status_daemon(404, serde_json::json!({ "error": "unknown index: wt-1" })).await;
    let server = McpServer::new(base).with_pinned_index("wt-1");
    let resp = server
        .dispatch(req("list_chunks", serde_json::json!({})))
        .await;
    let err = resp.error.expect("list_chunks must fail loudly");
    assert_eq!(err.code, INDEX_NOT_READY_CODE);
    assert_eq!(err.data.expect("payload")["error_code"], INDEX_NOT_READY);
}

/// A 503 from a routed endpoint — the daemon's "registered but not resident"
/// answer — must NOT be classified as never-indexed.
///
/// Why: this is the misclassification pointing the other way. A cold-parked
/// index was built; saying it never was is the same defect this PR removes.
#[tokio::test]
async fn non_resident_503_is_not_classified_as_never_indexed() {
    let base = spawn_status_daemon(
        503,
        serde_json::json!({ "error": "index_not_resident", "message": "restoring" }),
    )
    .await;
    let server = McpServer::new(base).with_pinned_index("wt-1");
    let resp = server
        .dispatch(req("index_status", serde_json::json!({})))
        .await;
    let err = resp.error.expect("503 is still an error");
    assert_ne!(
        err.code, INDEX_NOT_READY_CODE,
        "a built-but-not-resident index must not be reported as never built"
    );
}

/// The pinned `grep` tool is index-backed, so it reports the same state rather
/// than a bare 404 — otherwise the fallback advice would loop back into a tool
/// that cannot answer either.
#[tokio::test]
async fn grep_on_unindexed_pin_returns_index_not_ready() {
    let base =
        spawn_status_daemon(404, serde_json::json!({ "error": "unknown index: wt-1" })).await;
    let server = McpServer::new(base).with_pinned_index("wt-1");
    let resp = server
        .dispatch(req("grep", serde_json::json!({ "pattern": "fn main" })))
        .await;
    let err = resp.error.expect("grep must fail loudly");
    assert_eq!(err.code, INDEX_NOT_READY_CODE);
    let data = err.data.expect("structured payload");
    assert_eq!(data["error_code"], INDEX_NOT_READY);
}

/// The payload names exactly the three things the ruling requires: the state,
/// the reason, and the suggested fallback.
#[tokio::test]
async fn not_ready_payload_carries_state_reason_and_fallback() {
    let base =
        spawn_status_daemon(404, serde_json::json!({ "error": "unknown index: wt-1" })).await;
    let server = McpServer::new(base).with_pinned_index("wt-1");
    let resp = server
        .dispatch(req("search", serde_json::json!({ "query": "q" })))
        .await;
    let data = resp.error.expect("error").data.expect("payload");

    assert_eq!(data["state"], super::not_ready::STATE_NOT_INDEXED);
    let reason = data["reason"].as_str().expect("reason is a string");
    assert!(
        !reason.trim().is_empty(),
        "reason must explain why, not be an empty slot"
    );
    let fallback = data["suggested_fallback"]
        .as_array()
        .expect("suggested_fallback is an array");
    assert!(fallback.iter().any(|v| v == "grep"));
    assert!(fallback.iter().any(|v| v == "find"));
}

/// The prose message tells the model it may retry and what to use meanwhile —
/// a model that ignores `_meta` still gets the right instruction.
#[tokio::test]
async fn not_ready_message_says_retryable_and_names_the_fallback() {
    let base =
        spawn_status_daemon(404, serde_json::json!({ "error": "unknown index: wt-1" })).await;
    let server = McpServer::new(base).with_pinned_index("wt-1");
    let resp = server
        .dispatch(req("search", serde_json::json!({ "query": "q" })))
        .await;
    let msg = resp.error.expect("error").message;

    assert!(msg.contains("wt-1"), "must name the index: {msg}");
    assert!(msg.contains("Retry"), "must say it is retryable: {msg}");
    assert!(msg.contains("grep"), "must name the fallback: {msg}");
    assert!(msg.contains("find"), "must name the fallback: {msg}");
    assert!(
        !msg.starts_with("POST "),
        "must not leak the raw HTTP transport error: {msg}"
    );
}

/// The not-ready payload points at `list_indexes`, not only at a fallback.
///
/// Why (#5213): `suggested_fallback: ["grep", "find"]` was the only actionable
/// field, an agent read "grep" as trusty-search's `grep` TOOL, and that tool is
/// index-backed under the same pin — so it reported the same failure and the
/// loss of search capability round-tripped into advice that could not work. The
/// prose warned about it; the machine-readable field, which is what an agent
/// branches on, did not. The owner's ruling is that an omitted `index_id` must
/// error loudly AND point at discovery. Pre-fix the payload had no `next_steps`
/// key at all, so both assertions below fail against the pre-fix commit.
/// Test: this test.
#[tokio::test]
async fn not_ready_payload_points_at_list_indexes_not_only_a_fallback() {
    let base =
        spawn_status_daemon(404, serde_json::json!({ "error": "unknown index: wt-1" })).await;
    let server = McpServer::new(base).with_pinned_index("wt-1");
    let resp = server
        .dispatch(req("search", serde_json::json!({ "query": "q" })))
        .await;
    let data = resp.error.expect("error").data.expect("payload");

    let discover = data["next_steps"]["discover"]
        .as_str()
        .expect("next_steps.discover must be present");
    assert!(
        discover.contains("list_indexes"),
        "the caller must be told how to reach an index that DOES exist, got: {discover}"
    );
    let scope = data["fallback_scope"]
        .as_str()
        .expect("fallback_scope must disambiguate whose grep is meant");
    assert!(
        scope.contains("grep"),
        "the circular-advice trap must be named in data, not only in prose: {scope}"
    );
}

/// An unresolvable `index_id` names `list_indexes` in the error itself.
///
/// Why (#5213): "missing required string field: index_id" is true and
/// unactionable. A caller that does not know a valid id guesses one — which is
/// the wrong-index failure #1373 pinned the session to avoid, arriving by a
/// different route. Pre-fix the message was the bare field name, so the
/// `contains("list_indexes")` assertion fails against the pre-fix commit.
/// Test: this test.
#[tokio::test]
async fn missing_index_id_error_names_list_indexes() {
    // No pin, no explicit id: there is genuinely nothing to resolve.
    let server = McpServer::new("http://127.0.0.1:1");
    for tool in ["search", "search_semantic", "index_status"] {
        let resp = server
            .dispatch(req(tool, serde_json::json!({ "query": "q" })))
            .await;
        let msg = resp.error.expect("error").message;
        assert!(
            msg.contains("list_indexes"),
            "{tool}: an omitted index_id must point at discovery, got: {msg}"
        );
    }
}

/// `search_all`'s tool description and its `index_id` parameter agree.
///
/// Why (#5213): the tool description said omitting `index_id` "falls back to
/// legacy cross-project fan-out behaviour (issue #10)" while the parameter
/// description said it defaults to the session pin. Both cannot hold —
/// empirically the pin wins, so the documented fan-out path is unreachable in a
/// pinned session and the contract contradicted itself. `issue #10` was also a
/// stale reference (an unrelated closed `tga` ticket). Pre-fix the description
/// contained "issue #10" and did not mention the pin, so both assertions fail
/// against the pre-fix commit.
/// Test: this test.
#[test]
fn search_all_description_matches_actual_index_precedence() {
    let defs = super::tool_descriptors();
    let all = defs
        .as_array()
        .expect("descriptors are an array")
        .iter()
        .find(|t| t["name"] == "search_all")
        .expect("search_all is registered");
    let desc = all["description"].as_str().expect("description");

    assert!(
        !desc.contains("issue #10"),
        "the stale #10 reference must be gone: {desc}"
    );
    assert!(
        desc.contains("pinned"),
        "the description must state that a session pin pre-empts fan-out: {desc}"
    );
    let param = all["inputSchema"]["properties"]["index_id"]["description"]
        .as_str()
        .expect("index_id description");
    assert!(
        param.contains("pinned"),
        "the parameter description must agree with the tool description: {param}"
    );
}
