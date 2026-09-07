//! `NO_INDEX_RESOLVED` contract tests (issue #6317).
//!
//! Why: the owner ruling of 2026-08-27 is that a read tool with no resolvable
//! index answers with an index of what exists, as a success. The properties
//! that make that answer usable rather than merely non-failing are: it is a
//! success (`isError: false`), it carries the ids a retry needs, it names how
//! to retry, and it does NOT extend to the tools that would write to a guessed
//! index. These tests pin all four, plus the schema that lets a client issue
//! the call at all.
//! What: drives `McpServer::dispatch` in the `tools/call` form against a mock
//! daemon serving `GET /indexes` and `POST /grep`.
//! Test: this file.

use serde_json::Value;

use super::index_directory::{DIRECTORY_TOOLS, NO_INDEX_RESOLVED};
use super::tests::req;
use super::McpServer;

/// Spin up a mock daemon serving the listing and fan-out routes this contract
/// touches.
///
/// Why: the directory answer is built from the daemon's own `GET /indexes`
/// body, so a test that stubs the payload elsewhere would prove nothing about
/// the round-trip. `spawn_mock_daemon` in `tests.rs` serves only the per-index
/// status and search routes.
/// What: returns the base URL of a loopback listener answering
/// `GET /indexes` with `{"indexes": indexes}` and `POST /grep` with an empty
/// match set.
/// Test: used by every test in this file.
async fn spawn_listing_daemon(indexes: Value) -> String {
    use axum::extract::State;
    use axum::routing::{get, post};
    use axum::{Json, Router};

    async fn list(State(s): State<Value>) -> Json<Value> {
        Json(serde_json::json!({ "indexes": s }))
    }
    async fn grep() -> Json<Value> {
        Json(serde_json::json!({ "matches": [], "truncated": false }))
    }

    let app = Router::new()
        .route("/indexes", get(list))
        .route("/grep", post(grep))
        .with_state(indexes);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

/// Two registered indexes, shaped like the real `?details=true` rows.
fn two_indexes() -> Value {
    serde_json::json!([
        { "id": "alpha", "root_path": "/repos/alpha", "size_bytes": 4096 },
        { "id": "beta",  "root_path": "/repos/beta",  "size_bytes": 8192 },
    ])
}

/// Pull the decoded tool payload and the `isError` flag out of a `tools/call`
/// response.
fn tool_result(resp: &super::Response) -> (Value, bool) {
    let result = resp.result.as_ref().expect("tools/call returns a result");
    let is_error = result
        .get("isError")
        .and_then(Value::as_bool)
        .expect("tools/call carries isError");
    let text = result["content"][0]["text"]
        .as_str()
        .expect("one text content node");
    let decoded = serde_json::from_str::<Value>(text).unwrap_or(Value::String(text.to_string()));
    (decoded, is_error)
}

/// Build a `tools/call` request for `tool` with no `index_id`.
fn call(tool: &str) -> super::Request {
    req(
        "tools/call",
        serde_json::json!({ "name": tool, "arguments": { "query": "anything", "pattern": "x" } }),
    )
}

/// Every read tool the ruling names answers an unresolvable index with the
/// directory, as a SUCCESS.
///
/// Why (#6317): pre-fix each of these returned `isError: true` carrying
/// `MISSING_INDEX_ID`, so both the `!is_error` assertion and the `status`
/// assertion fail against the pre-fix commit. This is the primary test.
/// What: dispatches every name in `DIRECTORY_TOOLS` against an unpinned server
/// with no `index_id`, and asserts one shape for all of them — success, the
/// `no_index_resolved` discriminator, the ids a retry needs, and a hint that
/// names the field to set.
/// Test: this test.
#[tokio::test]
async fn unresolved_read_tools_return_the_index_directory() {
    let base = spawn_listing_daemon(two_indexes()).await;
    // No pin, no explicit id, no fan-out: there is genuinely nothing to resolve.
    let server = McpServer::new(base);

    for tool in DIRECTORY_TOOLS {
        let (payload, is_error) = tool_result(&server.dispatch(call(tool)).await);
        assert!(
            !is_error,
            "{tool}: the directory answer is a success: {payload}"
        );
        assert_eq!(
            payload.get("status").and_then(Value::as_str),
            Some(NO_INDEX_RESOLVED),
            "{tool}: a caller must be able to branch on the discriminator: {payload}"
        );
        assert_eq!(
            payload.get("tool").and_then(Value::as_str),
            Some(*tool),
            "{tool}: the payload names the tool that asked: {payload}"
        );
        let ids: Vec<&str> = payload["indexes"]
            .as_array()
            .expect("indexes array")
            .iter()
            .filter_map(|e| e["id"].as_str())
            .collect();
        assert_eq!(
            ids,
            vec!["alpha", "beta"],
            "{tool}: the rows `list_indexes` serves must reach the caller: {payload}"
        );
        let hint = payload["hint"].as_str().expect("hint");
        assert!(
            hint.contains("index_id"),
            "{tool}: the hint must name the field to set: {hint}"
        );
    }
}

/// A daemon with nothing registered points at registration, not at a retry it
/// cannot satisfy.
///
/// Why: "retry with one of the ids below" is advice a caller cannot follow when
/// the list is empty, which is the circular-advice trap #3805 named. The empty
/// case has to say something different.
/// What: serves an empty listing and asserts the hint names `create_index`.
/// Test: this test.
#[tokio::test]
async fn empty_daemon_directory_points_at_create_index() {
    let base = spawn_listing_daemon(serde_json::json!([])).await;
    let server = McpServer::new(base);
    let (payload, is_error) = tool_result(&server.dispatch(call("search")).await);
    assert!(
        !is_error,
        "still a success with nothing registered: {payload}"
    );
    let hint = payload["hint"].as_str().expect("hint");
    assert!(
        hint.contains("create_index"),
        "an empty daemon must not advise picking from an empty list: {hint}"
    );
}

/// The tools that would WRITE to a guessed index still error.
///
/// Why (#6317): the ruling is about discovery, not about making every omitted
/// argument recoverable. Indexing a file into, or reindexing, an index the
/// caller never named is not undoable, so those arms keep #5213's error. This
/// is the other side of the boundary
/// `missing_index_id_error_names_list_indexes` asserts.
/// What: dispatches each mutating arm with no `index_id` against the same live
/// mock daemon the read tools succeed against, so a pass cannot come from the
/// daemon being unreachable.
/// Test: this test.
#[tokio::test]
async fn mutating_tools_still_error_when_no_index_resolves() {
    let base = spawn_listing_daemon(two_indexes()).await;
    let server = McpServer::new(base);
    for tool in ["index_file", "remove_file", "delete_index", "reindex"] {
        let (payload, is_error) = tool_result(&server.dispatch(call(tool)).await);
        assert!(
            is_error,
            "{tool}: a write to an unnamed index must still fail: {payload}"
        );
        let text = payload.as_str().unwrap_or_default();
        assert!(
            text.contains("list_indexes"),
            "{tool}: the error still points at discovery: {text}"
        );
    }
}

/// An unpinned `grep` with no `index_id` fans out instead of erroring, which is
/// how it already satisfies the ruling.
///
/// Why (#6317): the issue lists `grep` among the six tools that must stop
/// erroring, but `grep` never took that path — `resolve_index_id` returning
/// `None` sends it to the global `POST /grep` (#3805). Returning a directory
/// instead would delete that fan-out. This test states the reason `grep` is
/// absent from `DIRECTORY_TOOLS` as an assertion rather than a comment.
/// What: dispatches `grep` unpinned with no id and asserts a successful daemon
/// answer, not a directory.
/// Test: this test.
#[tokio::test]
async fn unpinned_grep_with_no_index_fans_out() {
    let base = spawn_listing_daemon(two_indexes()).await;
    let server = McpServer::new(base);
    let (payload, is_error) = tool_result(&server.dispatch(call("grep")).await);
    assert!(!is_error, "grep fan-out is a success: {payload}");
    assert!(
        payload.get("matches").is_some(),
        "grep must reach the global endpoint, not the directory: {payload}"
    );
    assert!(
        !DIRECTORY_TOOLS.contains(&"grep"),
        "grep's ruling is met by fan-out; adding it here would delete that path"
    );
}

/// Every tool that answers with a directory is classified read-only.
///
/// Why: `scopes_for_tool` is the existing authority on which tools mutate. If a
/// mutating tool were ever added to `DIRECTORY_TOOLS`, the boundary above would
/// silently move. Deriving the assertion from that authority means the two
/// cannot disagree without a failing test.
/// What: asserts every entry maps to `search.read`.
/// Test: this test.
#[test]
fn directory_tools_are_all_read_scoped() {
    for tool in DIRECTORY_TOOLS {
        assert_eq!(
            crate::mcp::openrpc::scopes_for_tool(tool),
            vec!["search.read".to_string()],
            "{tool} must be read-only to answer with a directory"
        );
    }
}

/// The advertised schema lets a client make the call the ruling permits.
///
/// Why (#6317): a tool whose `inputSchema` still lists `index_id` as `required`
/// is one a schema-obeying client will never call without one, so the new
/// behaviour would be unreachable from the surface that advertises it.
/// Pre-fix `index_id` was required on `search`, so this fails against the
/// pre-fix commit.
/// What: asserts `index_id` is absent from `required` on every directory tool
/// and that both descriptions state what an omitted id does.
/// Test: this test.
#[test]
fn directory_tools_drop_required_index_id() {
    let defs = super::tool_descriptors();
    for tool in DIRECTORY_TOOLS {
        let def = defs
            .as_array()
            .expect("descriptors are an array")
            .iter()
            .find(|t| t["name"] == *tool)
            .unwrap_or_else(|| panic!("{tool} is registered"));
        let required: Vec<&str> = def["inputSchema"]["required"]
            .as_array()
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        assert!(
            !required.contains(&"index_id"),
            "{tool}: index_id must not be required: {required:?}"
        );
        let desc = def["description"].as_str().expect("description");
        assert!(
            desc.contains("#6317"),
            "{tool}: the description must document the no-id path: {desc}"
        );
        let prop = def["inputSchema"]["properties"]["index_id"]
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("{tool}: index_id property description"));
        assert!(
            prop.contains("#6317"),
            "{tool}: the parameter description must agree: {prop}"
        );
    }
}

/// A mutating tool's schema still requires `index_id`.
///
/// Why: the annotation is keyed off `DIRECTORY_TOOLS`, and a bug that applied
/// it to every tool would advertise `reindex` as callable with no index — the
/// exact call the dispatcher rejects. Schema and dispatcher must agree on the
/// write side too.
/// What: asserts `index_id` is still `required` on the four mutating arms.
/// Test: this test.
#[test]
fn mutating_tools_keep_required_index_id() {
    let defs = super::tool_descriptors();
    for tool in ["index_file", "remove_file", "delete_index", "reindex"] {
        let def = defs
            .as_array()
            .expect("descriptors are an array")
            .iter()
            .find(|t| t["name"] == tool)
            .unwrap_or_else(|| panic!("{tool} is registered"));
        let required: Vec<&str> = def["inputSchema"]["required"]
            .as_array()
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        assert!(
            required.contains(&"index_id"),
            "{tool}: index_id stays required: {required:?}"
        );
    }
}
