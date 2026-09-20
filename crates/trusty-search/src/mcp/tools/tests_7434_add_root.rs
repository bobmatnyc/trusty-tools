//! MCP surface for the multi-root index (#7434): the `add_root` tool, the
//! `roots` field on `create_index`, and the descriptor split that made room
//! for both.
//!
//! Why: the daemon has spoken `POST /indexes/:id/roots` since slice 2, but an
//! MCP client had no way to reach it — an index could only span several trees
//! if an operator called the HTTP API by hand. These tests pin the wire body
//! the tool sends, because that is the whole claim; asserting on the
//! dispatcher's inputs would prove nothing.
//! What: a mock axum daemon per test that captures the request, plus schema
//! assertions over `tool_descriptors()` covering the tools moved into
//! `descriptors_lifecycle`.
//! Test: this file.

use serde_json::Value;
use std::sync::Arc;
use tokio::sync::Mutex;

use super::tests::req;
use super::{error_codes, McpServer};

/// Spin up a mock daemon capturing one POST body, dispatch `tool` with `args`,
/// and return what the daemon received.
///
/// Why: `add_root` and `create_index` both claim to forward an array verbatim,
/// and only the daemon's view of the request can settle that.
/// What: routes `route` to a capturing handler, dispatches, asserts the
/// dispatch itself did not error, and returns the captured JSON body.
/// Test: used by every forwarding test below.
async fn captured_post_body(route: &str, tool: &str, args: Value) -> Value {
    use axum::routing::post;
    use axum::{Json, Router};

    let captured: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));

    async fn handler(
        axum::extract::State(captured): axum::extract::State<Arc<Mutex<Option<Value>>>>,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        *captured.lock().await = Some(body);
        Json(serde_json::json!({ "ok": true }))
    }

    let app = Router::new()
        .route(route, post(handler))
        .with_state(Arc::clone(&captured));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let server = McpServer::new(format!("http://{addr}"));
    let resp = server.dispatch(req(tool, args)).await;
    assert!(resp.error.is_none(), "unexpected error: {:?}", resp.error);
    let body = captured.lock().await.clone();
    body.expect("no request captured")
}

/// Find one tool's descriptor by name.
fn descriptor(name: &str) -> Value {
    super::tool_descriptors()
        .as_array()
        .expect("descriptors are an array")
        .iter()
        .find(|t| t["name"] == name)
        .unwrap_or_else(|| panic!("{name} is registered"))
        .clone()
}

/// `add_root` reaches `POST /indexes/:id/roots` with the caller's paths.
///
/// Why: the endpoint takes `{"roots": [...]}`, and the id rides in the path —
/// a tool that posted the paths under any other key, or to `/indexes`, would
/// add nothing while reporting success.
/// What: dispatches `add_root` with two paths and asserts both arrive under
/// `roots` at the index-scoped route.
/// Test: this test.
#[tokio::test]
async fn add_root_posts_the_roots_array() {
    let body = captured_post_body(
        "/indexes/{id}/roots",
        "add_root",
        serde_json::json!({
            "index_id": "idx",
            "roots": ["/projects/vendored", "/projects/docs"],
        }),
    )
    .await;
    assert_eq!(
        body.get("roots"),
        Some(&serde_json::json!(["/projects/vendored", "/projects/docs"])),
        "both roots must reach the daemon; got body: {body:?}"
    );
}

/// `add_root` without a usable `roots` array is rejected before any HTTP call.
///
/// Why: `roots` IS this call. `create_index` may drop a malformed optional
/// filter and still do what the caller asked; `add_root` cannot — posting an
/// empty list would echo back the index's unchanged root table as a success,
/// so the caller would believe a tree was added that never was.
/// What: absent, non-array, empty and mixed-type spellings each return
/// `INVALID_PARAMS` against a base URL that cannot connect, so a pass cannot
/// come from a daemon answering.
/// Test: this test.
#[tokio::test]
async fn add_root_rejects_a_missing_or_malformed_roots_array() {
    let server = McpServer::new("http://127.0.0.1:1");
    for args in [
        serde_json::json!({ "index_id": "idx" }),
        serde_json::json!({ "index_id": "idx", "roots": "/projects/vendored" }),
        serde_json::json!({ "index_id": "idx", "roots": [] }),
        serde_json::json!({ "index_id": "idx", "roots": ["/projects/a", 1] }),
    ] {
        let resp = server.dispatch(req("add_root", args.clone())).await;
        let err = resp
            .error
            .unwrap_or_else(|| panic!("{args} must be rejected"));
        assert_eq!(err.code, error_codes::INVALID_PARAMS, "for args {args}");
    }
}

/// #7434: `create_index` forwards `roots` to `POST /indexes`.
///
/// Why: the daemon has run create-time roots through the same gate as
/// `add_root` since slice 2, but an MCP caller could not use it — they had to
/// create, then add, and wait out two walks instead of one.
/// What: dispatches `create_index` with one additional root and asserts it
/// reaches the wire body alongside `id` and `root_path`.
/// Test: this test. It fails against the pre-#7434 commit, where the body
/// carries no `roots` key at all.
#[tokio::test]
async fn create_index_forwards_roots() {
    let body = captured_post_body(
        "/indexes",
        "create_index",
        serde_json::json!({
            "id": "idx",
            "root_path": "/projects/idx",
            "roots": ["/projects/vendored"],
        }),
    )
    .await;
    assert_eq!(body.get("id"), Some(&serde_json::json!("idx")));
    assert_eq!(
        body.get("roots"),
        Some(&serde_json::json!(["/projects/vendored"])),
        "the additional root must reach the daemon; got body: {body:?}"
    );
}

/// A malformed `roots` is omitted from `create_index` rather than forwarded.
///
/// Why: same reasoning as #4356's `exclude_globs` — the daemon deserialises
/// the field as `Vec<PathBuf>`, so sending `[1, 2]` would 422 the whole
/// registration over an optional field. All-or-nothing also means a mixed
/// array cannot quietly register an index covering fewer trees than asked.
/// What: a bare string, an array of non-strings, and a mixed array each leave
/// the key off the body, and the registration still happens.
/// Test: this test.
#[tokio::test]
async fn create_index_omits_malformed_roots() {
    for roots in [
        serde_json::json!("/projects/vendored"),
        serde_json::json!([1, 2]),
        serde_json::json!(["/projects/vendored", 1]),
    ] {
        let body = captured_post_body(
            "/indexes",
            "create_index",
            serde_json::json!({
                "id": "idx",
                "root_path": "/projects/idx",
                "roots": roots,
            }),
        )
        .await;
        assert!(
            body.get("roots").is_none(),
            "{roots} must not be forwarded; got body: {body:?}"
        );
        assert_eq!(
            body.get("root_path"),
            Some(&serde_json::json!("/projects/idx")),
            "the registration itself still goes through"
        );
    }
}

/// `add_root`'s advertised schema matches what the dispatcher enforces.
///
/// Why: the LLM calls what the schema describes. A schema that left `roots`
/// optional would invite exactly the call
/// `add_root_rejects_a_missing_or_malformed_roots_array` pins as an error.
/// What: `index_id` and `roots` are both `required`, and `roots` is an array
/// of strings.
/// Test: this test.
#[test]
fn add_root_is_advertised_with_a_required_roots_array() {
    let def = descriptor("add_root");
    let required: Vec<&str> = def["inputSchema"]["required"]
        .as_array()
        .expect("required array")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(required.contains(&"index_id"), "got {required:?}");
    assert!(required.contains(&"roots"), "got {required:?}");
    assert_eq!(def["inputSchema"]["properties"]["roots"]["type"], "array");
    assert_eq!(
        def["inputSchema"]["properties"]["roots"]["items"]["type"],
        "string"
    );
}

/// `create_index` advertises the `roots` array the dispatcher forwards.
#[test]
fn create_index_advertises_roots() {
    let def = descriptor("create_index");
    assert_eq!(def["inputSchema"]["properties"]["roots"]["type"], "array");
    // Optional: an ordinary single-tree registration must stay a two-field call.
    let required: Vec<&str> = def["inputSchema"]["required"]
        .as_array()
        .expect("required array")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(!required.contains(&"roots"), "got {required:?}");
}

/// Every lifecycle tool survived the move into `descriptors_lifecycle`, once.
///
/// Why (#7434): `descriptors.rs` was at 467 of its 500 SLOC, so the four
/// index-lifecycle schemas moved to their own file. A tool dropped by that
/// move disappears from `tools/list` silently — the dispatcher still answers
/// it, so nothing else fails — and a tool left in BOTH files is advertised
/// twice with schemas that can drift apart.
/// What: asserts each of the four names appears exactly once in
/// `tool_descriptors()`, and that each carries an `inputSchema`.
/// Test: this test.
#[test]
fn every_lifecycle_tool_is_advertised_exactly_once() {
    let defs = super::tool_descriptors();
    let names: Vec<&str> = defs
        .as_array()
        .expect("array")
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    for tool in ["create_index", "add_root", "delete_index", "reindex"] {
        assert_eq!(
            names.iter().filter(|n| **n == tool).count(),
            1,
            "{tool} must be advertised exactly once; got {names:?}"
        );
        assert!(
            descriptor(tool).get("inputSchema").is_some(),
            "{tool} must carry an inputSchema"
        );
    }
}
