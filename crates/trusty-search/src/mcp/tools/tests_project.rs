//! The optional `project` argument on the bridge's read tools (#9168).
//!
//! Why: a caller that knows a project but not its index id must reach the one
//! live index the daemon maps it to, and a caller that passes `index_id` must
//! see no change at all. A miss must hand back the daemon's candidates as data.
//! What: a recording mock socket daemon answers `search.project.resolve`; the
//! tests assert the call order, the resolved id, the precedence over
//! `index_id` and the pin, the three miss shapes, and the advertised schema.
//! Test: this file.

use serde_json::{json, Value};
use trusty_common::uds::server::RpcError;

use super::project::PROJECT_TOOLS;
use super::test_daemon::{last_params, methods, recording_daemon, unreachable_server};
use super::tests::req;
use super::{error_codes, tool_descriptors, PROJECT_UNRESOLVED, PROJECT_UNRESOLVED_CODE};
use crate::service::rpc::error::{CODE_CONFLICT, CODE_NOT_FOUND};

/// Answers resolve with `trusty-tools-4e2cf878` and every query with no hits.
fn resolving(method: &str, _params: &Value) -> Result<Value, RpcError> {
    Ok(match method {
        "search.project.resolve" => json!({
            "index_id": "trusty-tools-4e2cf878",
            "root_path": "/repos/trusty-tools",
            "matched_by": "name",
            "duplicates": [],
        }),
        _ => json!({ "results": [], "matches": [], "total": 0 }),
    })
}

/// `project` is resolved by the daemon, and the resolved id is what the
/// search runs against.
#[tokio::test]
async fn project_resolves_through_the_daemon() {
    let (daemon, calls) = recording_daemon(resolving).await;
    let resp = daemon
        .server()
        .dispatch(req(
            "search",
            json!({ "project": "bobmatnyc/trusty-tools", "query": "fn main" }),
        ))
        .await;
    assert!(resp.error.is_none(), "{:?}", resp.error);
    assert_eq!(methods(&calls), ["search.project.resolve", "search.query"]);
    assert_eq!(
        last_params(&calls, "search.project.resolve").expect("resolve"),
        json!({ "project": "bobmatnyc/trusty-tools" })
    );
    assert_eq!(
        last_params(&calls, "search.query").expect("query")["index_id"],
        "trusty-tools-4e2cf878"
    );
}

/// Every tool that advertises `project` resolves it before its own call.
#[tokio::test]
async fn every_project_tool_resolves_before_its_call() {
    for tool in PROJECT_TOOLS {
        let (daemon, calls) = recording_daemon(|method, params| match method {
            "search.call_chain" => Ok(Value::String("tree".into())),
            "search.index.status" => Ok(json!({ "search_capabilities": ["vector", "kg"] })),
            _ => resolving(method, params),
        })
        .await;
        let args = json!({
            "project": "trusty-tools",
            "query": "q",
            "pattern": "q",
            "entry_point": "main",
        });
        let resp = daemon.server().dispatch(req(tool, args)).await;
        assert!(resp.error.is_none(), "{tool}: {:?}", resp.error);
        let seen = methods(&calls);
        assert_eq!(seen[0], "search.project.resolve", "{tool}: {seen:?}");
        let last = calls
            .lock()
            .expect("calls")
            .last()
            .cloned()
            .expect("a call");
        assert_eq!(
            last.1["index_id"], "trusty-tools-4e2cf878",
            "{tool} must target the resolved index: {seen:?}"
        );
    }
}

/// An explicit `index_id` wins and no resolve call is made — `index_id`
/// behaves exactly as before.
#[tokio::test]
async fn index_id_wins_over_project_without_a_daemon_call() {
    let (daemon, calls) = recording_daemon(resolving).await;
    let resp = daemon
        .server()
        .dispatch(req(
            "search",
            json!({ "index_id": "explicit", "project": "trusty-tools", "query": "q" }),
        ))
        .await;
    assert!(resp.error.is_none(), "{:?}", resp.error);
    assert_eq!(methods(&calls), ["search.query"]);
    assert_eq!(
        last_params(&calls, "search.query").expect("q")["index_id"],
        "explicit"
    );
}

/// A `project` in the call outranks the session pin, which is only a default.
#[tokio::test]
async fn project_outranks_the_session_pin() {
    let (daemon, calls) = recording_daemon(resolving).await;
    let resp = daemon
        .server()
        .with_pinned_index("pinned")
        .dispatch(req(
            "grep",
            json!({ "project": "trusty-tools", "pattern": "x" }),
        ))
        .await;
    assert!(resp.error.is_none(), "{:?}", resp.error);
    assert_eq!(
        last_params(&calls, "search.grep").expect("grep")["index_id"],
        "trusty-tools-4e2cf878"
    );
}

/// Each of the daemon's three misses reaches the caller as `PROJECT_UNRESOLVED`
/// carrying its candidates, in both response forms.
#[tokio::test]
async fn an_unresolved_project_surfaces_its_candidates() {
    for (error, code) in [
        ("project_not_found", CODE_NOT_FOUND),
        ("project_ambiguous", CODE_CONFLICT),
        ("no_live_index", CODE_NOT_FOUND),
    ] {
        let (daemon, calls) = recording_daemon(move |_, _| {
            Err(
                RpcError::new(code, format!("{error}: see data.candidates")).with_data(json!({
                    "error": error,
                    "project": "api",
                    "candidates": [
                        { "index_id": "api-1", "root_path": "/w/one/api" },
                        { "index_id": "api-2", "root_path": "/w/two/api" },
                    ],
                })),
            )
        })
        .await;
        let server = daemon.server();

        let resp = server
            .dispatch(req(
                "tools/call",
                json!({ "name": "search", "arguments": { "project": "api", "query": "q" } }),
            ))
            .await;
        let result = resp.result.expect("tools/call answers in band");
        assert_eq!(result["isError"], true, "{error}");
        let meta = &result["_meta"];
        assert_eq!(meta["error_code"], PROJECT_UNRESOLVED, "{error}");
        assert_eq!(meta["error"], error);
        assert_eq!(meta["candidates"][1]["index_id"], "api-2");
        let text = result["content"][0]["text"].as_str().expect("text");
        assert!(text.contains("api-1") && text.contains("api-2"), "{text}");

        let bare = server
            .dispatch(req("search", json!({ "project": "api", "query": "q" })))
            .await;
        let err = bare.error.expect("bare form is an error");
        assert_eq!(err.code, PROJECT_UNRESOLVED_CODE, "{error}");
        assert_eq!(err.data.expect("data")["error"], error);
        assert_eq!(
            methods(&calls),
            ["search.project.resolve"; 2],
            "no search ran"
        );
    }
}

/// A wrong-typed `project` is a parameter error before any daemon call.
#[tokio::test]
async fn a_non_string_project_is_invalid_params() {
    let resp = unreachable_server()
        .dispatch(req("search", json!({ "project": 7, "query": "q" })))
        .await;
    let err = resp.error.expect("refused");
    assert_eq!(err.code, error_codes::INVALID_PARAMS);
    assert!(
        err.message.contains("project must be a string"),
        "{}",
        err.message
    );
}

/// The schema advertises `project` on exactly [`PROJECT_TOOLS`], optional.
#[test]
fn project_tools_advertise_the_project_argument() {
    let defs = tool_descriptors();
    for tool in defs.as_array().expect("descriptors") {
        let name = tool["name"].as_str().expect("name");
        let schema = &tool["inputSchema"];
        let has = schema["properties"].get("project").is_some();
        assert_eq!(has, PROJECT_TOOLS.contains(&name), "{name}");
        let required = schema["required"].as_array().cloned().unwrap_or_default();
        assert!(
            !required.iter().any(|r| r == "project"),
            "{name}: project must stay optional"
        );
    }
}

/// `project` is offered only on read tools.
#[test]
fn project_tools_are_all_read_scoped() {
    for tool in PROJECT_TOOLS {
        assert_eq!(
            crate::mcp::openrpc::scopes_for_tool(tool),
            vec!["search.read".to_string()],
            "{tool}"
        );
    }
}
