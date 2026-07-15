//! MCP JSON-RPC dispatch + stdio loop for `slack-mcp`.
//!
//! Why: Every trusty-* MCP server shares the same shape — `initialize`,
//! `tools/list`, `tools/call`, with notifications suppressed. Centralising it
//! here means future Slack tool handlers focus on Slack Web API specifics.
//! What: `AppState` holds an `Arc<BaseClient>`; `handle_message` is the pure
//! JSON-RPC dispatcher; `handle_tool_call` is the tool router (all nine tools
//! live, #2639 + #2640); `run_stdio` wires it into
//! `trusty_common::mcp::run_stdio_loop`.
//! Test: `handle_message_initialize_returns_server_info`,
//! `handle_message_tools_list_returns_tools`, and
//! `tools_call_search_messages_without_user_token_is_internal_error` in `mod tests`.

use std::sync::Arc;

use serde_json::{json, Value};
use thiserror::Error;

use crate::slack::api::client::BaseClient;
use crate::slack::api::error::SlackError;
use trusty_common::mcp::{error_codes, initialize_response, run_stdio_loop, Request, Response};

/// Shared state passed to every dispatcher invocation.
///
/// Why: Tool handlers will need the Slack client; nothing else is shared yet.
/// What: `Clone`-able via `Arc<BaseClient>`.
/// Test: Smoke-constructed in `mod tests`.
#[derive(Clone)]
pub struct AppState {
    pub client: Arc<BaseClient>,
}

/// Error returned by the tool dispatcher.
///
/// Why: The dispatcher must distinguish a genuinely unknown tool from a bad
/// argument and from a live Slack failure, and surface each as a proper
/// JSON-RPC error rather than a panic (`todo!()`/`unimplemented!()` are banned
/// here).
/// What: A `thiserror` enum whose `code()` maps to a JSON-RPC error code.
/// Test: `tools_call_unknown_tool_returns_method_not_found`,
/// `tools_call_missing_required_arg_is_invalid_params`.
#[derive(Debug, Error)]
pub enum ToolCallError {
    /// A tool name that is not part of the planned surface at all.
    #[error("unknown tool: {0}")]
    UnknownTool(String),
    /// A required argument was missing or the wrong JSON type.
    #[error("invalid arguments: {0}")]
    InvalidArgs(String),
    /// The live Slack Web API call failed (auth, rate-limit, `ok:false`, …).
    #[error(transparent)]
    Slack(#[from] SlackError),
}

impl ToolCallError {
    /// JSON-RPC error code for this failure.
    ///
    /// Why: `handle_message` needs a numeric code for the wire response.
    /// What: Unknown tools map to `METHOD_NOT_FOUND`; bad arguments map to
    /// `INVALID_PARAMS`; live Slack API failures map to `INTERNAL_ERROR`.
    /// Test: Covered by the dispatcher tests and `tests/tools_http.rs`.
    pub fn code(&self) -> i32 {
        match self {
            ToolCallError::UnknownTool(_) => error_codes::METHOD_NOT_FOUND,
            ToolCallError::InvalidArgs(_) => error_codes::INVALID_PARAMS,
            ToolCallError::Slack(_) => error_codes::INTERNAL_ERROR,
        }
    }
}

/// Dispatch a single tool call by name.
///
/// Why: One routing table keeps the tool surface greppable; all nine handlers
/// (send/read/list from #2639, search + reactions from #2640) run live Slack
/// calls here.
/// What: rejects an unknown name with `UnknownTool`, then delegates every known
/// name to [`crate::slack::handlers::dispatch`].
/// Test: `tools_call_unknown_tool_returns_method_not_found`,
/// `tools_call_missing_required_arg_is_invalid_params`, `tests/tools_http.rs`.
pub async fn handle_tool_call(
    state: &AppState,
    name: &str,
    args: Value,
) -> Result<Value, ToolCallError> {
    if !crate::slack::tools::is_known_tool(name) {
        return Err(ToolCallError::UnknownTool(name.to_string()));
    }
    crate::slack::handlers::dispatch(&state.client, name, args).await
}

/// Translate a parsed JSON-RPC request into a JSON-RPC response payload.
///
/// Why: Same shape used across the trusty-* family — handle init, ping,
/// notifications, tools/list, tools/call.
/// What: Returns `Value::Null` for notifications (suppressed); returns an
/// `{"error": {code, message}}` map for tool failures and unknown methods so
/// `run_stdio` can emit a proper JSON-RPC error.
/// Test: `handle_message_initialize_returns_server_info`,
/// `tools_call_add_reaction_is_routed_not_unknown`,
/// `unknown_method_returns_error_payload`.
pub async fn handle_message(state: AppState, req: Value) -> Value {
    let method = req["method"].as_str().unwrap_or("");
    match method {
        "initialize" => initialize_response("slack-mcp", env!("CARGO_PKG_VERSION"), None),
        "notifications/initialized" | "notifications/cancelled" => Value::Null,
        "ping" => json!({}),
        "tools/list" => crate::slack::tools::tool_list_response(),
        "tools/call" => {
            let params = &req["params"];
            let name = params["name"].as_str().unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(Value::Null);
            let args = if args.is_null() { json!({}) } else { args };
            match handle_tool_call(&state, name, args).await {
                Ok(result) => {
                    let text = serde_json::to_string(&result).unwrap_or_default();
                    json!({ "content": [{ "type": "text", "text": text }] })
                }
                Err(e) => json!({
                    "error": { "code": e.code(), "message": e.to_string() }
                }),
            }
        }
        _ => json!({
            "error": {
                "code": error_codes::METHOD_NOT_FOUND,
                "message": format!("Method not found: {method}"),
            }
        }),
    }
}

/// Run the stdio MCP loop wired to this server.
///
/// Why: Single entry point for `bin/slack-mcp.rs`.
/// What: Forwards every parsed `Request` to `handle_message` and wraps the
/// JSON result back into a `Response` (or a JSON-RPC error / suppressed reply).
/// Test: Manual — driven from Claude Code; the stdio loop itself is tested in
/// `trusty-common`.
pub async fn run_stdio(state: AppState) -> anyhow::Result<()> {
    run_stdio_loop(move |req: Request| {
        let state = state.clone();
        async move {
            let id = req.id.clone();
            let raw = serde_json::to_value(&req).unwrap_or(Value::Null);
            let resp = handle_message(state, raw).await;
            if resp.is_null() {
                return Response::suppressed();
            }
            if let Some(err) = resp.get("error") {
                let code =
                    err.get("code")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(error_codes::INTERNAL_ERROR as i64) as i32;
                let message = err
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Internal error")
                    .to_string();
                return Response::err(id, code, message);
            }
            Response::ok(id, resp)
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_state() -> AppState {
        // A tokenless client pinned to an unreachable endpoint: fully hermetic
        // and independent of any `SLACK_*` env var. Any handler that would make
        // a network call fails fast with `MissingToken` before touching the
        // network, so these dispatcher tests never depend on ambient secrets.
        let client =
            BaseClient::with_endpoint("http://127.0.0.1:9", None).expect("construct base client");
        AppState {
            client: Arc::new(client),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn handle_message_initialize_returns_server_info() {
        let state = make_state();
        let req = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} });
        let resp = handle_message(state, req).await;
        assert_eq!(resp["serverInfo"]["name"], "slack-mcp");
        assert!(resp["capabilities"]["tools"].is_object());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn handle_message_tools_list_returns_tools() {
        let state = make_state();
        let req = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" });
        let resp = handle_message(state, req).await;
        let tools = resp["tools"].as_array().expect("tools array");
        assert_eq!(tools.len(), crate::slack::tools::TOOL_NAMES.len());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn tools_call_add_reaction_is_routed_not_unknown() {
        // `slack_add_reaction` is now implemented (#2640): it must route to the
        // handler, not surface as an unknown tool. With no token the handler
        // fails fast with `MissingToken` → INTERNAL_ERROR (never
        // METHOD_NOT_FOUND), proving the routing without a live call.
        let state = make_state();
        let req = json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": { "name": "slack_add_reaction", "arguments": { "channel": "C1", "timestamp": "1.2", "name": "eyes" } }
        });
        let resp = handle_message(state, req).await;
        assert_eq!(resp["error"]["code"], error_codes::INTERNAL_ERROR);
        assert_ne!(resp["error"]["code"], error_codes::METHOD_NOT_FOUND);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn tools_call_search_messages_without_user_token_is_internal_error() {
        // The tokenless client has no user token, so `slack_search_messages`
        // must surface the clear `MissingUserToken` error (INTERNAL_ERROR),
        // never a bot-token fallback and never METHOD_NOT_FOUND.
        let state = make_state();
        let req = json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/call",
            "params": { "name": "slack_search_messages", "arguments": { "query": "hello" } }
        });
        let resp = handle_message(state, req).await;
        assert_eq!(resp["error"]["code"], error_codes::INTERNAL_ERROR);
        assert!(resp["error"]["message"]
            .as_str()
            .unwrap()
            .contains("user token required for search"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn tools_call_missing_required_arg_is_invalid_params() {
        // `slack_send_message` requires `text`; omitting it must be an
        // INVALID_PARAMS error *before* any network call is attempted.
        let state = make_state();
        let req = json!({
            "jsonrpc": "2.0",
            "id": 6,
            "method": "tools/call",
            "params": { "name": "slack_send_message", "arguments": { "channel": "C1" } }
        });
        let resp = handle_message(state, req).await;
        assert_eq!(resp["error"]["code"], error_codes::INVALID_PARAMS);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn tools_call_unknown_tool_returns_method_not_found() {
        let state = make_state();
        let req = json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": { "name": "slack_not_a_tool", "arguments": {} }
        });
        let resp = handle_message(state, req).await;
        assert_eq!(resp["error"]["code"], error_codes::METHOD_NOT_FOUND);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unknown_method_returns_error_payload() {
        let state = make_state();
        let req = json!({ "jsonrpc": "2.0", "id": 5, "method": "no/such/method" });
        let resp = handle_message(state, req).await;
        assert_eq!(resp["error"]["code"], error_codes::METHOD_NOT_FOUND);
    }
}
