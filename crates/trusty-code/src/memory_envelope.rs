//! Shared trusty-memory `tools/call` envelope helpers (#2424).
//!
//! Why: trusty-memory's JSON-RPC surface direct-dispatches only the methods
//! in its `TOOL_METHODS` allowlist (`crates/trusty-memory/src/transport/
//! rpc.rs`); everything else — notably every `chat_*` tool — is reachable
//! ONLY via the MCP-style `tools/call` envelope. The #2343 soak
//! (2026-07-11) found `memory_sink::write_turn`'s direct-method dispatch of
//! `chat_turn_append` failing `-32601 Method not found` on 100% of turns
//! (#2424), while #2348's `recall_session` — which already used the
//! `tools/call` envelope — worked. This module is the SINGLE shared
//! implementation of that spike-verified envelope shape (build the
//! `{"name", "arguments"}` params; unwrap the STRINGIFIED JSON blob the
//! daemon returns inside `result.content[0].text`) so the write side
//! (`session::memory_sink`) and the read side (`tools::recall_session`) can
//! never drift apart again.
//! What: [`tools_call_params`] builds the request params;
//! [`parse_tools_call_envelope`] unwraps a response envelope;
//! [`call_tool_wrapped`] composes both around
//! `trusty_common::mcp::memory_rpc::call_memory_tool_at` for callers that
//! want a one-shot "call this tool, give me its parsed result" helper.
//! Test: `memory_envelope::tests::*`.

use anyhow::{Context, anyhow};
use serde_json::{Value, json};
use trusty_common::mcp::memory_rpc::call_memory_tool_at;

/// Build the `params` object for a JSON-RPC `tools/call` request.
///
/// Why: keeps the MCP `{"name": <tool>, "arguments": <args>}` convention in
/// exactly one place — a typo'd key here (e.g. `"args"`) would silently
/// break every trusty-memory write, which is precisely the failure class
/// #2424 is about.
/// What: wraps `tool` and `arguments` into the MCP `tools/call` params
/// shape trusty-memory's dispatcher expects.
/// Test: `tests::tools_call_params_shape`.
pub fn tools_call_params(tool: &str, arguments: Value) -> Value {
    json!({
        "name": tool,
        "arguments": arguments,
    })
}

/// Extract and parse the inner tool-result JSON from a `tools/call`
/// response envelope.
///
/// Why: isolates the spike-verified unwrap (issue #2348: the daemon returns
/// the tool result as a STRINGIFIED JSON blob inside
/// `result.content[0].text`, not a nested `Value`) as one shared, testable
/// step. Previously private to `tools::recall_session`
/// (`parse_recall_envelope`); promoted here so `session::memory_sink` can
/// reuse it (#2424).
/// What: `envelope` is the raw `result` field of the JSON-RPC response
/// (i.e. `call_memory_tool_at`'s `Ok` payload for a `tools/call` request).
/// Returns `Some(parsed_body)` on a well-formed envelope, `None` otherwise
/// (never panics on an unexpected shape).
/// Test: `tests::parse_tools_call_envelope_unwraps_stringified_content`,
/// `tests::parse_tools_call_envelope_none_on_missing_content`,
/// `tests::parse_tools_call_envelope_none_on_malformed_inner_json`.
pub fn parse_tools_call_envelope(envelope: &Value) -> Option<Value> {
    let text = envelope
        .get("content")
        .and_then(|c| c.get(0))
        .and_then(|c0| c0.get("text"))
        .and_then(|t| t.as_str())?;
    serde_json::from_str(text).ok()
}

/// Call one trusty-memory tool through the `tools/call` envelope and return
/// its parsed inner result.
///
/// Why: the one-shot composition the turn recorder's dual-write needs —
/// after #2424 every `memory_sink` write goes through this so the parsed
/// result it inspects (e.g. `memory_remember`'s `"status":"skipped"`
/// detection, #2363) has the SAME shape it had under the old direct
/// dispatch, keeping the caller's success/skipped logic unchanged.
/// What: builds the params via [`tools_call_params`], POSTs via
/// `call_memory_tool_at(base_url, "tools/call", ..)`, and unwraps the
/// response via [`parse_tools_call_envelope`]. A JSON-RPC error (the shape
/// trusty-memory returns when the inner tool itself fails — its dispatcher
/// converts tool errors via `JsonRpcResponse::from_anyhow`) surfaces as
/// `Err` from `call_memory_tool_at`; an unparseable envelope surfaces as
/// its own `Err` so fail-open callers can warn on it distinctly.
/// Test: `tests::call_tool_wrapped_round_trips_against_mock`,
/// `tests::call_tool_wrapped_errs_on_malformed_envelope`; exercised
/// end-to-end by `session::memory_sink::tests::*` and
/// `tools::recall_session::tests::*`.
pub async fn call_tool_wrapped(
    base_url: &str,
    tool: &str,
    arguments: Value,
) -> anyhow::Result<Value> {
    let envelope = call_memory_tool_at(base_url, "tools/call", tools_call_params(tool, arguments))
        .await
        .with_context(|| format!("tools/call '{tool}'"))?;
    parse_tools_call_envelope(&envelope).ok_or_else(|| {
        anyhow!("unexpected tools/call envelope shape from trusty-memory for '{tool}'")
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use axum::routing::post;
    use axum::{Json, Router};
    use tokio::net::TcpListener;

    use super::*;

    #[test]
    fn tools_call_params_shape() {
        let params = tools_call_params("memory_recall", json!({"palace": "p", "query": "q"}));
        assert_eq!(params["name"], "memory_recall");
        assert_eq!(params["arguments"]["palace"], "p");
        assert_eq!(params["arguments"]["query"], "q");
    }

    #[test]
    fn parse_tools_call_envelope_unwraps_stringified_content() {
        let inner = json!({"palace": "p", "results": [{"content": "hello"}]}).to_string();
        let envelope = json!({"content": [{"type": "text", "text": inner}]});
        let body = parse_tools_call_envelope(&envelope).expect("should parse");
        assert_eq!(body["palace"], "p");
        assert_eq!(body["results"][0]["content"], "hello");
    }

    #[test]
    fn parse_tools_call_envelope_none_on_missing_content() {
        assert!(parse_tools_call_envelope(&json!({"not_content": []})).is_none());
    }

    #[test]
    fn parse_tools_call_envelope_none_on_malformed_inner_json() {
        let envelope = json!({"content": [{"type": "text", "text": "not valid json"}]});
        assert!(parse_tools_call_envelope(&envelope).is_none());
    }

    /// Spin up a one-route mock `/rpc` server replying with `reply` wrapped
    /// (or not) per the closure, and return its base URL.
    async fn spawn_mock(result: Value) -> String {
        async fn handle(
            axum::extract::State(result): axum::extract::State<Value>,
            Json(_body): Json<Value>,
        ) -> Json<Value> {
            Json(json!({"jsonrpc": "2.0", "id": 1, "result": result}))
        }
        let app = Router::new().route("/rpc", post(handle)).with_state(result);
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind mock");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn call_tool_wrapped_round_trips_against_mock() {
        let inner = json!({"status": "stored", "drawer_id": "d1"}).to_string();
        let base_url = spawn_mock(json!({"content": [{"type": "text", "text": inner}]})).await;
        let result = call_tool_wrapped(&base_url, "memory_remember", json!({"palace": "p"}))
            .await
            .expect("wrapped call should succeed");
        assert_eq!(result["status"], "stored");
        assert_eq!(result["drawer_id"], "d1");
    }

    #[tokio::test]
    async fn call_tool_wrapped_errs_on_malformed_envelope() {
        let base_url = spawn_mock(json!({"content": [{"type": "text", "text": "not json"}]})).await;
        let err = call_tool_wrapped(&base_url, "memory_remember", json!({"palace": "p"}))
            .await
            .expect_err("malformed envelope must surface as Err");
        assert!(err.to_string().contains("unexpected tools/call envelope"));
    }
}
