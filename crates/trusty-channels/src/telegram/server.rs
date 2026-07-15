//! MCP JSON-RPC dispatch + stdio loop for `telegram-mcp`.
//!
//! Why: Every trusty-* MCP server shares the same shape — `initialize`,
//! `tools/list`, `tools/call`, with notifications suppressed. Centralising it
//! here means future Telegram tool handlers focus on Bot API specifics.
//! What: `AppState` holds an `Arc<BaseClient>`; `handle_message` is the pure
//! JSON-RPC dispatcher; `handle_tool_call` is the (currently all-stub) tool
//! router; `run_stdio` wires it into `trusty_common::mcp::run_stdio_loop`.
//! Test: `handle_message_initialize_returns_server_info`,
//! `handle_message_tools_list_returns_tools`, and
//! `tools_call_returns_not_implemented` in `mod tests`.

use std::sync::Arc;

use serde_json::{json, Value};
use thiserror::Error;

use crate::telegram::api::client::BaseClient;
use trusty_common::mcp::{error_codes, initialize_response, run_stdio_loop, Request, Response};

/// Shared state passed to every dispatcher invocation.
///
/// Why: Tool handlers will need the Telegram client; nothing else is shared yet.
/// What: `Clone`-able via `Arc<BaseClient>`.
/// Test: Smoke-constructed in `mod tests`.
#[derive(Clone)]
pub struct AppState {
    pub client: Arc<BaseClient>,
}

/// Error returned by the tool dispatcher.
///
/// Why: The scaffold must distinguish a known-but-unimplemented tool from a
/// genuinely unknown one, and surface each as a proper JSON-RPC error rather
/// than a panic (`todo!()`/`unimplemented!()` are banned here).
/// What: A `thiserror` enum whose `code()` maps to a JSON-RPC error code.
/// Test: `tools_call_returns_not_implemented` and
/// `tools_call_unknown_tool_returns_method_not_found`.
#[derive(Debug, Error)]
pub enum ToolCallError {
    /// A planned tool whose live handler has not been implemented yet.
    #[error(
        "tool '{0}' is not yet implemented: live Telegram Bot API calls are deferred (see issue #2641)"
    )]
    NotImplemented(String),
    /// A tool name that is not part of the planned surface at all.
    #[error("unknown tool: {0}")]
    UnknownTool(String),
}

impl ToolCallError {
    /// JSON-RPC error code for this failure.
    ///
    /// Why: `handle_message` needs a numeric code for the wire response.
    /// What: Unknown tools map to `METHOD_NOT_FOUND`; unimplemented (but
    /// planned) tools map to `INTERNAL_ERROR`.
    /// Test: Covered by the dispatcher tests.
    pub fn code(&self) -> i32 {
        match self {
            ToolCallError::NotImplemented(_) => error_codes::INTERNAL_ERROR,
            ToolCallError::UnknownTool(_) => error_codes::METHOD_NOT_FOUND,
        }
    }
}

/// Dispatch a single tool call by name.
///
/// Why: One routing table keeps the tool surface greppable; live handlers land
/// on the matching arm here as each Bot API method is implemented.
/// What: Every planned tool currently routes to a `NotImplemented` stub;
/// anything else is `UnknownTool`. The `_state`/`_args` are accepted now so the
/// signature is stable when real handlers replace the stubs.
/// Test: `tools_call_returns_not_implemented`.
pub async fn handle_tool_call(
    _state: &AppState,
    name: &str,
    _args: Value,
) -> Result<Value, ToolCallError> {
    if crate::telegram::tools::is_known_tool(name) {
        // Stub: schema is authoritative, live call deferred (see issue #2641).
        return Err(ToolCallError::NotImplemented(name.to_string()));
    }
    Err(ToolCallError::UnknownTool(name.to_string()))
}

/// Translate a parsed JSON-RPC request into a JSON-RPC response payload.
///
/// Why: Same shape used across the trusty-* family — handle init, ping,
/// notifications, tools/list, tools/call.
/// What: Returns `Value::Null` for notifications (suppressed); returns an
/// `{"error": {code, message}}` map for tool failures and unknown methods so
/// `run_stdio` can emit a proper JSON-RPC error.
/// Test: `handle_message_initialize_returns_server_info`,
/// `tools_call_returns_not_implemented`, `unknown_method_returns_error_payload`.
pub async fn handle_message(state: AppState, req: Value) -> Value {
    let method = req["method"].as_str().unwrap_or("");
    match method {
        "initialize" => initialize_response("telegram-mcp", env!("CARGO_PKG_VERSION"), None),
        "notifications/initialized" | "notifications/cancelled" => Value::Null,
        "ping" => json!({}),
        "tools/list" => crate::telegram::tools::tool_list_response(),
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
/// Why: Single entry point for `bin/telegram-mcp.rs`.
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
        // BaseClient::new() reads no required env vars and returns Ok, so this
        // works in CI without any Telegram credentials.
        let client = BaseClient::new().expect("construct base client");
        AppState {
            client: Arc::new(client),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn handle_message_initialize_returns_server_info() {
        let state = make_state();
        let req = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} });
        let resp = handle_message(state, req).await;
        assert_eq!(resp["serverInfo"]["name"], "telegram-mcp");
        assert!(resp["capabilities"]["tools"].is_object());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn handle_message_tools_list_returns_tools() {
        let state = make_state();
        let req = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" });
        let resp = handle_message(state, req).await;
        let tools = resp["tools"].as_array().expect("tools array");
        assert_eq!(tools.len(), crate::telegram::tools::TOOL_NAMES.len());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn tools_call_returns_not_implemented() {
        let state = make_state();
        let req = json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": { "name": "telegram_send_message", "arguments": { "chat_id": "123", "text": "hi" } }
        });
        let resp = handle_message(state, req).await;
        assert_eq!(resp["error"]["code"], error_codes::INTERNAL_ERROR);
        assert!(resp["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not yet implemented"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn tools_call_unknown_tool_returns_method_not_found() {
        let state = make_state();
        let req = json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": { "name": "telegram_not_a_tool", "arguments": {} }
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
