//! Core dispatch tests for the MCP tool dispatcher.
//!
//! Why: validates the JSON-RPC protocol layer (version check, notification
//! suppression, `tools/call` vs bare-method dispatch, error code mapping)
//! and the cross-cutting tool characteristics (all tools appear in
//! `tools/list`, schema requires the right fields, `grep`/`search_all`
//! param validation).
//! What: unit tests that either do not need a live daemon (a socket path
//! nothing serves) or answer through a mock daemon on a scratch socket
//! (`test_daemon.rs`, #9168).
//! Test: this file.

use serde_json::Value;

use super::test_daemon::{last_params, methods, recording_daemon, unreachable_server, MockDaemon};
use super::{error_codes, Request};

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
    let server = unreachable_server();
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
    let server = unreachable_server();
    let resp = server.dispatch(req("not_a_tool", Value::Null)).await;
    let err = resp.error.expect("expected error");
    assert_eq!(err.code, error_codes::METHOD_NOT_FOUND);
}

#[tokio::test]
async fn missing_params_returns_invalid_params() {
    let server = unreachable_server();
    let resp = server
        .dispatch(req("index_file", serde_json::json!({})))
        .await;
    let err = resp.error.expect("expected error");
    assert_eq!(err.code, error_codes::INVALID_PARAMS);
}

#[tokio::test]
async fn tools_list_returns_all_tools() {
    let server = unreachable_server();
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
    let server = unreachable_server();
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
    let server = unreachable_server();
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
    let server = unreachable_server();
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
    let server = unreachable_server();
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
    let server = unreachable_server();
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
/// before any daemon round-trip.
#[tokio::test]
async fn search_all_missing_query_returns_invalid_params() {
    let server = unreachable_server();
    let resp = server
        .dispatch(req("search_all", serde_json::json!({})))
        .await;
    let err = resp.error.expect("expected error");
    assert_eq!(err.code, error_codes::INVALID_PARAMS);
}

#[tokio::test]
async fn tools_call_without_name_returns_invalid_params() {
    let server = unreachable_server();
    let resp = server
        .dispatch(req("tools/call", serde_json::json!({})))
        .await;
    let err = resp.error.expect("expected error");
    assert_eq!(err.code, error_codes::INVALID_PARAMS);
}

/// `grep` is listed and missing-pattern fast-fails before any daemon hop.
#[tokio::test]
async fn grep_missing_pattern_returns_invalid_params() {
    let server = unreachable_server();
    let resp = server.dispatch(req("grep", serde_json::json!({}))).await;
    let err = resp.error.expect("expected error");
    assert_eq!(err.code, error_codes::INVALID_PARAMS);
}

/// Issue #447 — `max_count` is forwarded as `max_results` to the daemon.
///
/// Why: when the MCP client passes `max_count` (ripgrep's `--max-count`
/// flag name) the dispatcher must translate it to `max_results` before
/// calling the daemon. Without the alias the parameter was silently
/// dropped and the daemon applied its default cap of 100 regardless.
/// What: asserts that a `grep` call with `max_count=5` (and no
/// `max_results`) forwards `max_results: 5` in the daemon request body.
/// Test: a mock socket daemon records the `search.grep` params, then the
/// forwarded body is checked for `max_results == 5`.
#[tokio::test]
async fn grep_max_count_alias_forwarded_as_max_results() {
    let (daemon, calls) = recording_daemon(|_, _| {
        Ok(serde_json::json!({ "matches": [], "total": 0, "truncated": false }))
    })
    .await;

    let server = daemon.server();
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
    let params = last_params(&calls, "search.grep").expect("no request captured");
    assert_eq!(params["index_id"], "idx");
    let body = &params["body"];
    assert_eq!(
        body.get("max_results").and_then(Value::as_u64),
        Some(5),
        "max_count must be forwarded as max_results; got body: {body:?}"
    );
}

/// `grep` appears in `tools/list` with a `pattern`-required schema.
#[tokio::test]
async fn grep_listed_in_tools_with_required_pattern() {
    let server = unreachable_server();
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

/// Captured values from a mock daemon, read after the dispatch returns.
pub(super) type Captured<T> = std::sync::Arc<std::sync::Mutex<Vec<T>>>;

/// A mock socket daemon answering the status and both search methods.
///
/// Why: the per-lane tool tests in `tests_lane.rs` all need a controllable
/// daemon — this helper lets each test specify exactly what
/// `search.index.status` and `search.query` return, and captures the inbound
/// request bodies so tests can assert the correct `SearchQuery` shape was
/// dispatched.
/// What: returns `(daemon, captured_search_bodies, captured_search_routes)`.
/// A route reads `search.query <index_id>` or `search.query.all`; a body is
/// the `SearchQuery` (per-index) or the global request (fan-out). #9168: a
/// scratch socket replaced the loopback axum listener.
/// Test: used by every test in `tests_lane.rs`.
pub(super) async fn spawn_mock_daemon(
    status_response: Value,
    search_response: Value,
) -> (MockDaemon, Captured<Value>, Captured<String>) {
    let bodies: Captured<Value> = Default::default();
    let routes: Captured<String> = Default::default();
    let (b, r) = (bodies.clone(), routes.clone());
    let (daemon, _calls) = recording_daemon(move |method, params| match method {
        "search.index.status" => {
            // Inject the index_id so the payload looks like a real status.
            let mut v = status_response.clone();
            if v.is_object() {
                v["index_id"] = params["index_id"].clone();
            }
            Ok(v)
        }
        "search.query" => {
            let id = params["index_id"].as_str().unwrap_or_default();
            r.lock().expect("routes").push(format!("search.query {id}"));
            b.lock().expect("bodies").push(params["body"].clone());
            Ok(search_response.clone())
        }
        // #7676: the global fan-out answers with the same body, so a
        // `search_all` call with no index can be exercised through this harness.
        "search.query.all" => {
            r.lock().expect("routes").push(method.to_string());
            b.lock().expect("bodies").push(params.clone());
            Ok(search_response.clone())
        }
        other => Err(trusty_common::uds::server::RpcError::new(
            trusty_common::uds::server::CODE_METHOD_NOT_FOUND,
            format!("mock: {other}"),
        )),
    })
    .await;
    (daemon, bodies, routes)
}

// Issue #1373 — pinned-index resolution + serve-time pin
// ----------------------------------------------------------------

/// `resolve_index_id` precedence: explicit arg wins, else pinned, else None.
///
/// Why: the whole #1373 fix hinges on this precedence — a pinned session must
/// default an omitted `index_id` to the pin, still honour an explicit id, and
/// (without a pin) fall through to `None` so behaviour is unchanged.
/// What: exercises all four combinations on the bare helper.
/// Test: this is the test.
#[test]
fn resolve_index_id_prefers_explicit_then_pinned() {
    let pinned = unreachable_server().with_pinned_index("my-project");
    // Omitted → pinned.
    assert_eq!(
        pinned.resolve_index_id(&serde_json::json!({})),
        Some("my-project".to_string())
    );
    // Explicit wins over the pin.
    assert_eq!(
        pinned.resolve_index_id(&serde_json::json!({ "index_id": "other" })),
        Some("other".to_string())
    );
    // Empty explicit id is ignored → falls back to the pin.
    assert_eq!(
        pinned.resolve_index_id(&serde_json::json!({ "index_id": "" })),
        Some("my-project".to_string())
    );

    // No pin: omitted → None (unchanged legacy behaviour).
    let unpinned = unreachable_server();
    assert_eq!(unpinned.resolve_index_id(&serde_json::json!({})), None);
    assert_eq!(
        unpinned.resolve_index_id(&serde_json::json!({ "index_id": "x" })),
        Some("x".to_string())
    );
}

/// A blank `--index ""` pin is treated as "no pin" so a degenerate flag can't
/// wedge every call onto an empty-string index.
///
/// Why: defensive — an operator (or a buggy launcher) passing `--index ""`
/// must not silently pin to `""`.
/// What: asserts `with_pinned_index("")` leaves `pinned_index` unset.
/// Test: this is the test.
#[test]
fn blank_pin_is_treated_as_no_pin() {
    let s = unreachable_server().with_pinned_index("   ");
    assert_eq!(s.resolve_index_id(&serde_json::json!({})), None);
}

/// Without a pin, `search` with no `index_id` goes looking for the index
/// directory instead of fast-failing (#6317).
///
/// Why: this test asserted INVALID_PARAMS, which was the #1373 contract until
/// the owner ruling of 2026-08-27 replaced the error with a listing of what
/// exists. The behaviour it guards now is that the arm consults the daemon:
/// against an unreachable daemon that surfaces as a transport failure, never as
/// a parameter error. The success shape is asserted in
/// `tests_index_directory::unresolved_read_tools_return_the_index_directory`,
/// which drives a live mock.
/// What: dispatches `search` (no index_id, no pin) at an absent socket and
/// asserts the error is INTERNAL_ERROR, not INVALID_PARAMS.
/// Test: this is the test.
#[tokio::test]
async fn search_without_pin_consults_the_index_directory() {
    let server = unreachable_server();
    let resp = server
        .dispatch(req("search", serde_json::json!({ "query": "fn main" })))
        .await;
    let err = resp.error.expect("expected error");
    assert_eq!(
        err.code,
        error_codes::INTERNAL_ERROR,
        "an omitted index_id must reach the daemon listing, not fail on params: {}",
        err.message
    );
}

/// With a pin, `search` with no `index_id` targets the pinned index endpoint.
///
/// Why: the core acceptance criterion — a pinned session must resolve a bare
/// `search` to its own project index, never sweeping or guessing.
/// What: spins up the mock daemon, dispatches `search` (query only) against a
/// server pinned to `pinned-proj`, and asserts the daemon saw `search.query`
/// for `pinned-proj`.
/// Test: this is the test.
#[tokio::test]
async fn pinned_search_defaults_index_id_to_pin() {
    let (daemon, _bodies, paths) = spawn_mock_daemon(
        serde_json::json!({ "search_capabilities": ["vector", "kg"] }),
        serde_json::json!({ "results": [], "intent": "Definition", "latency_ms": 1 }),
    )
    .await;
    let server = daemon.server().with_pinned_index("pinned-proj");

    let resp = server
        .dispatch(req("search", serde_json::json!({ "query": "fn main" })))
        .await;
    assert!(
        resp.error.is_none(),
        "pinned search should succeed: {resp:?}"
    );

    let seen = paths.lock().expect("routes");
    assert_eq!(seen.len(), 1, "exactly one daemon search call: {seen:?}");
    assert_eq!(seen[0], "search.query pinned-proj");
}

/// With a pin, `grep` with no `index_id` scopes to the pinned index
/// (`search.grep`) instead of the global fan-out (`search.grep.all`).
///
/// Why: #1373 requires fan-out tools to scope to the pin so a project session
/// never sweeps every registered index.
/// What: mock daemon captures the grep path; asserts the pinned per-index
/// endpoint was hit.
/// Test: this is the test.
#[tokio::test]
async fn pinned_grep_scopes_to_pinned_index() {
    let (daemon, calls) = recording_daemon(|_, _| Ok(serde_json::json!({ "matches": [] }))).await;

    let server = daemon.server().with_pinned_index("pinned-proj");
    let resp = server
        .dispatch(req("grep", serde_json::json!({ "pattern": "fn foo" })))
        .await;
    assert!(resp.error.is_none(), "pinned grep should succeed: {resp:?}");

    assert_eq!(methods(&calls), ["search.grep"]);
    let params = last_params(&calls, "search.grep").expect("a grep call");
    assert_eq!(params["index_id"], "pinned-proj");
}

/// Dispatch `create_index` with `args` against a mock socket daemon and
/// return the `search.index.create` params it received.
///
/// Why: the #4356 forwarding claim is about the WIRE body, so asserting on the
/// dispatcher's inputs would prove nothing.
/// What: mirrors `grep_max_count_alias_forwarded_as_max_results`'s harness.
async fn captured_create_index_body(args: Value) -> Value {
    let (daemon, calls) =
        recording_daemon(|_, _| Ok(serde_json::json!({ "id": "idx", "created": true }))).await;
    let resp = daemon.server().dispatch(req("create_index", args)).await;
    assert!(resp.error.is_none(), "unexpected error: {:?}", resp.error);
    last_params(&calls, "search.index.create").expect("no request captured")
}

/// #4356: `create_index` forwards `exclude_globs` to `search.index.create`.
///
/// Why: the daemon has accepted `exclude_globs` on this endpoint since the
/// repo-config work, but the MCP tool never sent it — so an MCP caller
/// registering a large tree had no way to narrow it, which is half of what
/// makes an oversized index a hazard.
/// What: dispatches `create_index` with two globs and asserts both reach the
/// wire body.
/// Test: this test. It fails against the pre-fix commit, where the body carries
/// only `id` and `root_path`.
#[tokio::test]
async fn create_index_forwards_exclude_globs() {
    let body = captured_create_index_body(serde_json::json!({
        "id": "idx",
        "root_path": "/projects/idx",
        "exclude_globs": ["**/fixtures/**", "**/*.generated.ts"],
    }))
    .await;
    assert_eq!(
        body.get("exclude_globs"),
        Some(&serde_json::json!(["**/fixtures/**", "**/*.generated.ts"])),
        "both globs must reach the daemon; got body: {body:?}"
    );
}

/// #4356: a malformed `exclude_globs` is omitted rather than forwarded.
///
/// Why: the daemon deserialises the field as `Option<Vec<String>>`, so sending
/// `[1, 2]` would 422 the whole registration — an unrelated failure for a
/// caller who merely mistyped an optional filter.
/// What: a non-array value, an array with no strings, and a MIXED array all
/// leave the field off the body entirely. The mixed case used to forward just
/// the string entries, so `["**/a/**", 1]` registered an index filtered by one
/// glob where the caller wrote two — the same typo `[1, 2]` already rejected
/// whole, but silent. All-or-nothing means the two spellings agree.
/// Test: this test.
#[tokio::test]
async fn create_index_omits_malformed_exclude_globs() {
    let not_an_array = captured_create_index_body(serde_json::json!({
        "id": "idx",
        "root_path": "/projects/idx",
        "exclude_globs": "**/fixtures/**",
    }))
    .await;
    assert!(
        not_an_array.get("exclude_globs").is_none(),
        "a bare string must not be forwarded: {not_an_array:?}"
    );

    let no_strings = captured_create_index_body(serde_json::json!({
        "id": "idx",
        "root_path": "/projects/idx",
        "exclude_globs": [1, 2],
    }))
    .await;
    assert!(
        no_strings.get("exclude_globs").is_none(),
        "an array with no strings must not be forwarded: {no_strings:?}"
    );

    let mixed = captured_create_index_body(serde_json::json!({
        "id": "idx",
        "root_path": "/projects/idx",
        "exclude_globs": ["**/fixtures/**", 1, "**/*.generated.ts"],
    }))
    .await;
    assert!(
        mixed.get("exclude_globs").is_none(),
        "one non-string entry must reject the whole array, not silently drop \
         that entry and narrow the index by less than the caller asked: {mixed:?}"
    );
}

// ----------------------------------------------------------------
// Issue #6422 — delete_index purges on-disk data by default
// ----------------------------------------------------------------

/// Dispatch `delete_index` against a mock daemon and return the params it sent.
///
/// Why: the whole #6422 change on this surface is which `delete_data` leaves
/// the tool, and only the daemon's view of the request can prove it. The mock
/// answers the shape `search.index.delete` really answers so the dispatcher
/// takes its success path.
/// What: returns the `search.index.delete` params the daemon received.
/// Test: used by the `delete_index_*` tests below.
async fn captured_delete_index_params(args: Value) -> Value {
    let (daemon, calls) = recording_daemon(|_, _| {
        Ok(serde_json::json!({
            "id": "idx", "ok": true, "removed": true, "data_deleted": true
        }))
    })
    .await;
    let resp = daemon.server().dispatch(req("delete_index", args)).await;
    assert!(resp.error.is_none(), "unexpected error: {:?}", resp.error);
    last_params(&calls, "search.index.delete").expect("no delete captured")
}

/// Why (#6422, closure condition 1): the owner ruling, on the MCP surface. A
/// caller that names only the index gets the destructive default, and the flag
/// is sent EXPLICITLY rather than left to the daemon's own default, which is
/// still the opposite (#4123).
/// Test: this is the test.
#[tokio::test]
async fn delete_index_purges_data_by_default() {
    let params = captured_delete_index_params(serde_json::json!({ "index_id": "idx" })).await;
    assert_eq!(
        params,
        serde_json::json!({ "index_id": "idx", "delete_data": true }),
        "an argument-free delete_index must ask for the on-disk data too"
    );
}

/// Why (#6422, closure condition 2): deregister-only survives as the explicit
/// opt-out. Against the pre-fix code this argument did not exist at all and the
/// tool sent `delete_data=true` regardless, so a caller asking to keep the
/// corpus destroyed it — this assertion fails there.
/// Test: this is the test.
#[tokio::test]
async fn delete_index_honours_the_deregister_only_opt_out() {
    let params = captured_delete_index_params(
        serde_json::json!({ "index_id": "idx", "delete_data": false }),
    )
    .await;
    assert_eq!(params["delete_data"], serde_json::json!(false));

    let explicit_true =
        captured_delete_index_params(serde_json::json!({ "index_id": "idx", "delete_data": true }))
            .await;
    assert_eq!(explicit_true["delete_data"], serde_json::json!(true));
}

/// Why (#4123's rule, carried into #6422): a destructive toggle is never
/// guessed at. Coercing `"false"` to the default would destroy the corpus the
/// caller asked to keep, and dropping it silently would do the same.
/// Test: this is the test — no daemon is needed, the error precedes the call.
#[tokio::test]
async fn delete_index_rejects_a_non_boolean_delete_data() {
    let server = unreachable_server();
    for bad in [
        serde_json::json!("false"),
        serde_json::json!(0),
        serde_json::json!([]),
    ] {
        let resp = server
            .dispatch(req(
                "delete_index",
                serde_json::json!({ "index_id": "idx", "delete_data": bad }),
            ))
            .await;
        let err = resp
            .error
            .expect("a non-boolean delete_data must be refused");
        assert_eq!(err.code, error_codes::INVALID_PARAMS, "{bad}");
        assert!(
            err.message.contains("delete_data"),
            "the refusal must name the field: {}",
            err.message
        );
    }
}

/// Why (#6422): the schema is what a remote caller reads to learn the opt-out
/// exists. A default flipped in the dispatcher and not advertised here is a
/// change no caller can see.
/// Test: this is the test.
#[tokio::test]
async fn delete_index_schema_advertises_the_opt_out() {
    let server = unreachable_server();
    let resp = server.dispatch(req("tools/list", Value::Null)).await;
    let result = resp.result.expect("expected result");
    let tools = result
        .get("tools")
        .and_then(Value::as_array)
        .expect("array");
    let tool = tools
        .iter()
        .find(|t| t.get("name").and_then(Value::as_str) == Some("delete_index"))
        .expect("delete_index missing from tools/list");
    assert_eq!(
        tool["inputSchema"]["properties"]["delete_data"]["default"],
        serde_json::json!(true),
        "the schema must say the data goes by default: {tool}"
    );
    assert!(
        tool["description"]
            .as_str()
            .unwrap_or_default()
            .contains("delete_data: false"),
        "the description must name the opt-out: {tool}"
    );
}
