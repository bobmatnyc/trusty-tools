//! Boolean-flag typing tests for the MCP tool dispatcher (#7927).
//!
//! Why: a flag read with `as_bool().unwrap_or(false)` maps `"true"` — the
//! spelling a hand-written client or an LLM sends most often — onto the
//! default, so the call succeeds and returns exactly what a call that never
//! set the flag would return. Live verification on 2026-09-14 saw
//! `compact: "true"` come back as full hits with `isError: false`. The
//! guarantee is therefore about the ERROR a wrong-typed flag produces and
//! about the default an ABSENT flag keeps, which are only observable on the
//! dispatched response and on the daemon body the dispatcher forwards.
//! What: one table test over every boolean flag the MCP layer reads, and one
//! forwarding test proving an absent flag still means the documented default
//! while a present one is honoured.
//! Test: this module.

use serde_json::Value;

use super::misc::GREP_BOOL_FLAGS;
use super::test_daemon::{recording_daemon, unreachable_server, Calls};
use super::tests::req;
use super::{error_codes, McpServer};

/// Every boolean flag a caller can send, with a tool call that reaches it.
///
/// The flag is injected as the STRING `"true"` by the test below — the exact
/// value the issue's live verification sent. Rows cover the flags this branch
/// made strict plus the two already-strict ones that are read before any
/// daemon call (`compact`, `delete_data`), so the shared rule cannot regress
/// on them either. `full` and `max_bytes` are read on the response path and
/// keep their own coverage in `tests_byte_cap.rs`.
fn flag_rows() -> Vec<(&'static str, &'static str, Value)> {
    let mut rows: Vec<(&'static str, &'static str, Value)> = vec![
        (
            "search",
            "exclude_archived",
            serde_json::json!({ "index_id": "demo", "query": "handler" }),
        ),
        (
            "search_lexical",
            "exclude_archived",
            serde_json::json!({ "index_id": "demo", "query": "handler" }),
        ),
        (
            "search",
            "compact",
            serde_json::json!({ "index_id": "demo", "query": "handler" }),
        ),
        (
            "search_all",
            "serial",
            serde_json::json!({ "query": "handler" }),
        ),
        (
            "search_all",
            "full_content",
            serde_json::json!({ "query": "handler" }),
        ),
        (
            "get_call_chain",
            "include_source",
            serde_json::json!({ "index_id": "demo", "entry_point": "handler" }),
        ),
        (
            "create_index",
            "follow_links",
            serde_json::json!({ "id": "demo", "root_path": "/tmp/demo" }),
        ),
        (
            "delete_index",
            "delete_data",
            serde_json::json!({ "index_id": "demo" }),
        ),
    ];
    for (key, _) in GREP_BOOL_FLAGS {
        rows.push((
            "grep",
            key,
            serde_json::json!({ "index_id": "demo", "pattern": "fn handler" }),
        ));
    }
    rows
}

/// A wrong-typed boolean flag is an `INVALID_PARAMS` error naming the
/// parameter and its expected type — never a silently-defaulted call.
///
/// Every row here reaches its flag before any daemon call, so the
/// unreachable socket proves the rejection happens on the arguments rather
/// than on a daemon answer.
#[tokio::test]
async fn every_boolean_flag_rejects_a_wrong_typed_value() {
    for (tool, flag, mut args) in flag_rows() {
        let server = unreachable_server();
        args[flag] = Value::String("true".into());
        let resp = server.dispatch(req(tool, args)).await;
        let err = resp
            .error
            .unwrap_or_else(|| panic!("{tool}: a string {flag} must not be accepted"));
        assert_eq!(
            err.code,
            error_codes::INVALID_PARAMS,
            "{tool}/{flag}: {}",
            err.message
        );
        assert!(
            err.message.contains(&format!("{flag} must be a boolean")),
            "{tool}/{flag}: the error must name the parameter and the expected \
             type; got {}",
            err.message
        );
    }
}

/// Dispatch `tool` and return the request body the mock daemon received.
///
/// #9168: `search.grep` nests the body under `body`; `search.query.all` takes
/// it as the params themselves.
fn forwarded(calls: &Calls) -> Value {
    let (method, params) = calls
        .lock()
        .expect("the call log")
        .last()
        .cloned()
        .expect("no call forwarded");
    match method.as_str() {
        "search.grep" => params["body"].clone(),
        _ => params,
    }
}

/// Dispatch `tool` and return the body the mock daemon received.
async fn forwarded_body(server: &McpServer, calls: &Calls, tool: &str, args: Value) -> Value {
    let resp = server.dispatch(req(tool, args)).await;
    assert!(resp.error.is_none(), "{tool}: {:?}", resp.error);
    forwarded(calls)
}

/// An absent flag keeps the documented default, and a present one is still
/// honoured — rejecting wrong types must not turn into rejecting omissions.
#[tokio::test]
async fn an_absent_boolean_flag_keeps_the_documented_default() {
    let (daemon, captured) = recording_daemon(|_, _| {
        Ok(serde_json::json!({ "results": [], "matches": [], "total": 0 }))
    })
    .await;
    let server = daemon.server();

    // grep: an omitted switch is not forwarded at all, so the daemon applies
    // its own `false` default.
    let body = forwarded_body(
        &server,
        &captured,
        "grep",
        serde_json::json!({ "index_id": "demo", "pattern": "fn handler" }),
    )
    .await;
    for (key, _) in GREP_BOOL_FLAGS {
        assert!(
            body.get(key).is_none(),
            "an omitted {key} must not be forwarded; got body {body}"
        );
    }

    // …and a real boolean still reaches the daemon unchanged.
    let body = forwarded_body(
        &server,
        &captured,
        "grep",
        serde_json::json!({
            "index_id": "demo",
            "pattern": "fn handler",
            "case_insensitive": true,
            "word_regexp": false,
        }),
    )
    .await;
    assert_eq!(body["case_insensitive"], Value::Bool(true), "body {body}");
    assert_eq!(body["word_regexp"], Value::Bool(false), "body {body}");

    // search_all: `full_content` defaults to false and `serial` is omitted.
    let body = forwarded_body(
        &server,
        &captured,
        "search_all",
        serde_json::json!({ "query": "handler" }),
    )
    .await;
    assert_eq!(body["full_content"], Value::Bool(false), "body {body}");
    assert!(body.get("serial").is_none(), "body {body}");
}
