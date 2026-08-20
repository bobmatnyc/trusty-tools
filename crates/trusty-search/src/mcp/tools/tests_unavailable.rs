//! `INDEX_UNAVAILABLE` contract tests — structured daemon 503s at the MCP
//! boundary (issue #5350).
//!
//! Why: the defect is a representation one. The daemon knows which index is
//! unavailable, why, and whether retrying helps; the MCP transport formatted
//! all of it into one English sentence, so a caller could read the failure but
//! not branch on it. These tests pin the three properties that make the answer
//! usable: the verdict arrives as DATA on every verb, the `retryable` split
//! survives as a boolean rather than as prose, and a 503 is never mistaken for
//! a search that ran and found nothing.
//! What: drives `McpServer::dispatch` against a mock daemon fixed at 503 with
//! each of the availability bodies `service/server/degraded.rs` emits.
//! Test: this file.

use serde_json::{json, Value};

use super::tests::req;
use super::tests_not_ready::spawn_status_daemon;
use super::unavailable::classify_unavailable;
use super::{McpServer, INDEX_UNAVAILABLE, INDEX_UNAVAILABLE_CODE};

/// The `503 index_not_resident` body `residency_miss_response` emits for a
/// cold-parked index, copied field for field from `degraded.rs`.
fn cold_parked_body() -> Value {
    json!({
        "error": "index_not_resident",
        "index_id": "wt-1",
        "retryable": true,
        "restore_via": "POST /indexes/wt-1/search",
        "message": "index 'wt-1' is registered and built but is not currently resident \
                    (cold-parked by TRUSTY_MAX_RESIDENT_INDEXES).",
    })
}

/// Pull the `_meta` block out of a `tools/call` result, if present.
fn meta(resp: &super::Response) -> Option<Value> {
    resp.result.as_ref()?.get("_meta").cloned()
}

/// The text of a `tools/call` result's first content node.
fn text(resp: &super::Response) -> String {
    resp.result
        .as_ref()
        .and_then(|r| r["content"][0]["text"].as_str())
        .unwrap_or_default()
        .to_owned()
}

/// A `tools/call` search against a cold-parked index carries the daemon's
/// verdict as data.
///
/// Pre-fix this returned `isError: true` with the whole body stringified into
/// `POST … returned 503 Service Unavailable: {…}` and NO `_meta` at all, so
/// every assertion below on `_meta` failed.
#[tokio::test]
async fn tools_call_search_on_cold_parked_index_returns_structured_meta() {
    let base = spawn_status_daemon(503, cold_parked_body()).await;
    let server = McpServer::new(base);

    let resp = server
        .dispatch(req(
            "tools/call",
            json!({
                "name": "search",
                "arguments": { "index_id": "wt-1", "query": "residency" },
            }),
        ))
        .await;

    assert!(resp.error.is_none(), "tool failures ride in-band");
    let result = resp.result.clone().expect("tools/call returns a result");
    assert_eq!(result["isError"], Value::Bool(true));

    let m = meta(&resp).expect("_meta must carry the machine-readable verdict");
    assert_eq!(m["error_code"], INDEX_UNAVAILABLE);
    assert_eq!(
        m["error"], "index_not_resident",
        "the daemon's own code must survive, not just the MCP-level family"
    );
    assert_eq!(m["index_id"], "wt-1");
    assert_eq!(
        m["retryable"],
        Value::Bool(true),
        "whether retrying helps must be a boolean, not a sentence"
    );
    assert_eq!(m["restore_via"], "POST /indexes/wt-1/search");
    assert_eq!(m["http_status"], 503);
}

/// The bare-method form carries the same payload under `error.data`, with an
/// app-level code distinct from `INTERNAL_ERROR`.
#[tokio::test]
async fn bare_method_search_on_cold_parked_index_returns_structured_data() {
    let base = spawn_status_daemon(503, cold_parked_body()).await;
    let server = McpServer::new(base);

    let resp = server
        .dispatch(req(
            "search",
            json!({ "index_id": "wt-1", "query": "residency" }),
        ))
        .await;

    let err = resp
        .error
        .expect("bare-method failures are JSON-RPC errors");
    assert_eq!(err.code, INDEX_UNAVAILABLE_CODE);
    assert_ne!(
        err.code,
        super::error_codes::INTERNAL_ERROR,
        "an availability verdict is not an internal error"
    );
    let data = err.data.expect("structured payload rides in error.data");
    assert_eq!(data["error_code"], INDEX_UNAVAILABLE);
    assert_eq!(data["error"], "index_not_resident");
    assert_eq!(data["retryable"], Value::Bool(true));
    assert_eq!(data["restore_via"], "POST /indexes/wt-1/search");
}

/// Error-arm guard: the NON-retryable 503 must reach the caller as
/// `retryable: false`, not as a string a caller has to read.
///
/// Why this arm specifically: flattening turned both halves of the split into
/// the same kind of object — a prose message. A caller that could not tell
/// `vector_unavailable/skipped_by_config` (never clears without a config
/// change) from `vector_unavailable/stage_not_ready` (clears on its own) either
/// polls forever or gives up on a state that was about to resolve.
#[tokio::test]
async fn non_retryable_vector_verdict_survives_as_a_boolean() {
    let base = spawn_status_daemon(
        503,
        json!({
            "error": "vector_unavailable",
            "reason": "skipped_by_config",
            "index_id": "wt-1",
            "retryable": false,
            "message": "index 'wt-1' cannot serve a semantic search.",
        }),
    )
    .await;
    let server = McpServer::new(base);

    let resp = server
        .dispatch(req(
            "tools/call",
            json!({
                "name": "search_semantic",
                "arguments": { "index_id": "wt-1", "query": "meaning" },
            }),
        ))
        .await;

    let m = meta(&resp).expect("_meta must carry the verdict");
    assert_eq!(m["error"], "vector_unavailable");
    assert_eq!(m["reason"], "skipped_by_config");
    assert_eq!(
        m["retryable"],
        Value::Bool(false),
        "a permanent verdict must be distinguishable from a transient one"
    );
}

/// The GET verbs get the same treatment as the POST verbs.
#[tokio::test]
async fn index_status_503_is_structured_not_prose() {
    let base = spawn_status_daemon(503, cold_parked_body()).await;
    let server = McpServer::new(base);

    let resp = server
        .dispatch(req(
            "tools/call",
            json!({ "name": "index_status", "arguments": { "index_id": "wt-1" } }),
        ))
        .await;

    let m = meta(&resp).expect("_meta must carry the verdict on GET too");
    assert_eq!(m["error"], "index_not_resident");
    assert_eq!(m["retryable"], Value::Bool(true));
}

/// The write verbs too (#5349 adjacency — this asserts the SHAPE of the 503
/// those endpoints return, not that they should return one).
#[tokio::test]
async fn index_file_503_is_structured_not_prose() {
    let base = spawn_status_daemon(503, cold_parked_body()).await;
    let server = McpServer::new(base);

    let resp = server
        .dispatch(req(
            "tools/call",
            json!({
                "name": "index_file",
                "arguments": { "index_id": "wt-1", "path": "src/a.rs", "content": "fn a() {}" },
            }),
        ))
        .await;

    let m = meta(&resp).expect("_meta must carry the verdict on a write");
    assert_eq!(m["error"], "index_not_resident");
    assert_eq!(
        m["restore_via"], "POST /indexes/wt-1/search",
        "the hint that un-sticks a write is the whole reason the body has one"
    );
}

/// A `text/plain` verb (`get_call_chain`) with a body carrying no `message`
/// still produces a usable one, built from the fields that ARE present.
#[tokio::test]
async fn call_chain_503_without_a_message_field_still_reads_usefully() {
    let base = spawn_status_daemon(
        503,
        json!({
            "error": "kg_unavailable",
            "reason": "skipped_by_config",
            "index": "wt-1",
        }),
    )
    .await;
    let server = McpServer::new(base);

    let resp = server
        .dispatch(req(
            "tools/call",
            json!({
                "name": "get_call_chain",
                "arguments": { "index_id": "wt-1", "entry_point": "main" },
            }),
        ))
        .await;

    let m = meta(&resp).expect("_meta must carry the verdict on a text endpoint");
    assert_eq!(m["error"], "kg_unavailable");
    let t = text(&resp);
    assert!(t.contains("kg_unavailable"), "message names the state: {t}");
    assert!(t.contains("wt-1"), "message names the index: {t}");
}

/// The `restore_via` hint is surfaced in the prose too, because the model reads
/// `content[]` before it reads `_meta`.
#[tokio::test]
async fn unavailable_message_carries_the_restore_hint() {
    let base = spawn_status_daemon(503, cold_parked_body()).await;
    let server = McpServer::new(base);

    let resp = server
        .dispatch(req(
            "tools/call",
            json!({
                "name": "search",
                "arguments": { "index_id": "wt-1", "query": "q" },
            }),
        ))
        .await;

    let m = meta(&resp).expect("_meta must carry the verdict");
    assert_eq!(m["restore_via"], "POST /indexes/wt-1/search");
    let t = text(&resp);
    assert!(
        t.contains("POST /indexes/wt-1/search"),
        "the endpoint that clears the state must be legible in the prose: {t}"
    );
    assert!(
        !t.contains("returned 503 Service Unavailable"),
        "the prose must be the daemon's message, not a stringified HTTP failure: {t}"
    );
}

/// Fail-open guard: an availability verdict must never look like a search that
/// ran and returned nothing.
#[tokio::test]
async fn structured_503_is_never_mistaken_for_an_empty_result_set() {
    let base = spawn_status_daemon(503, cold_parked_body()).await;
    let server = McpServer::new(base);

    let resp = server
        .dispatch(req(
            "tools/call",
            json!({
                "name": "search",
                "arguments": { "index_id": "wt-1", "query": "q" },
            }),
        ))
        .await;

    let result = resp.result.expect("tools/call returns a result");
    assert_eq!(result["isError"], Value::Bool(true));
    assert!(
        result.get("results").is_none(),
        "a 503 is a failure, never a zero-hit success body"
    );
}

/// The classifier fires ONLY on a 503 carrying a structured body — everything
/// else falls through to the pre-existing `Transport` error unchanged.
///
/// Why this matters: the fix must not swallow a status it does not understand.
/// A 500, a bodyless 503, or a 503 from a proxy that answers HTML all keep the
/// behaviour they had before #5350.
#[test]
fn classify_unavailable_ignores_non_503_and_unstructured_bodies() {
    use reqwest::StatusCode;

    let structured = cold_parked_body();
    assert!(
        classify_unavailable(StatusCode::INTERNAL_SERVER_ERROR, &structured).is_none(),
        "only 503 is an availability verdict"
    );
    assert!(
        classify_unavailable(StatusCode::NOT_FOUND, &structured).is_none(),
        "404 belongs to the #4715 contract, not this one"
    );
    assert!(
        classify_unavailable(StatusCode::SERVICE_UNAVAILABLE, &json!([1, 2, 3])).is_none(),
        "a non-object body is not a verdict"
    );
    assert!(
        classify_unavailable(
            StatusCode::SERVICE_UNAVAILABLE,
            &json!({ "detail": "nope" })
        )
        .is_none(),
        "a body without an `error` code is not a verdict"
    );
    assert!(
        classify_unavailable(StatusCode::SERVICE_UNAVAILABLE, &json!({ "error": 503 })).is_none(),
        "a non-string `error` is not a code"
    );
    assert!(
        classify_unavailable(StatusCode::SERVICE_UNAVAILABLE, &structured).is_some(),
        "the one shape that IS a verdict"
    );
}

/// An unstructured 503 keeps reaching the caller as the prose transport error
/// it always was — the fallback is intact, not replaced.
#[tokio::test]
async fn unstructured_503_still_falls_through_to_the_transport_error() {
    let base = spawn_status_daemon(503, json!({ "detail": "gateway said no" })).await;
    let server = McpServer::new(base);

    let resp = server
        .dispatch(req("search", json!({ "index_id": "wt-1", "query": "q" })))
        .await;

    let err = resp.error.expect("still an error");
    assert_eq!(err.code, super::error_codes::INTERNAL_ERROR);
    assert!(err.data.is_none(), "nothing structured to carry");
}

/// The `503 index_corpus_unavailable` read-failure body reaches an MCP caller
/// as data, on the grep twin as much as on search (#5917).
///
/// Why: `grep`, `get_call_chain`, and `search` all refuse an unreadable corpus
/// over HTTP now, and every one of them relays through this module. A verdict
/// that arrived as prose on the grep tool would leave the MCP surface reporting
/// "no matches" for the state the HTTP surface calls an outage.
/// What: answers a `grep` tool call with the exact body
/// `degraded::corpus_read_failure_response` emits and asserts each field
/// survives into `_meta`, including the two #5917 unified.
#[tokio::test]
async fn tools_call_grep_on_an_unreadable_corpus_returns_structured_meta() {
    let body = json!({
        "error": "index_corpus_unavailable",
        "index_id": "wt-1",
        "failure_kind": "read_failed",
        "transient": true,
        "retryable": true,
        "message": "index 'wt-1': the durable corpus could not be read (#5917)",
    });
    let base = spawn_status_daemon(503, body).await;
    let server = McpServer::new(base);

    let resp = server
        .dispatch(req(
            "tools/call",
            json!({
                "name": "grep",
                "arguments": { "index_id": "wt-1", "pattern": "authenticate_user" },
            }),
        ))
        .await;

    let m = meta(&resp).expect("_meta must carry the machine-readable verdict");
    assert_eq!(m["error_code"], INDEX_UNAVAILABLE);
    assert_eq!(m["error"], "index_corpus_unavailable");
    assert_eq!(m["index_id"], "wt-1");
    assert_eq!(
        m["retryable"],
        Value::Bool(true),
        "#5917 unified the field set: both producers of this code send `retryable`"
    );
    assert_eq!(m["failure_kind"], "read_failed");
    assert_eq!(m["http_status"], 503);
    assert!(
        text(&resp).contains("#5917"),
        "the prose the model reads names the fault too"
    );
}
