//! The MCP JSON-RPC message router for the stdio and socket paths.
//!
//! Why (#9269): moved verbatim out of `lib.rs`, which the `server` feature
//! gates pushed over the 500-SLOC cap. `crate::handle_message` re-exports it,
//! so every caller's path is unchanged.
//! What: [`handle_message`].
//! Test: `lib_tests` (initialize, tools/list, tools/call, notifications).

use serde_json::{json, Value};
use trusty_mcp::initialize_response;

use crate::{openrpc, tools, AppState};

/// Handle a single MCP JSON-RPC message and produce its response.
///
/// Why: Pulled out of the stdio loop so unit tests can drive every method
/// without touching real stdin/stdout.
/// What: Routes `initialize`, `tools/list`, `tools/call`, `ping`, and the
/// `notifications/initialized` notification (which returns `Value::Null`).
/// Test: See unit tests below — initialize/list/call all return expected
/// JSON-RPC envelopes; notifications return `Null` (no response written).
pub async fn handle_message(state: &AppState, msg: Value) -> Value {
    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");

    match method {
        "initialize" => {
            let extra = state
                .default_palace
                .as_ref()
                .map(|dp| json!({ "default_palace": dp }));
            let result = initialize_response("trusty-memory", &state.version, extra);
            // Why (issue #42): prompt-facts now flow through the
            // per-message `get_prompt_context` tool rather than MCP
            // prompts, so we no longer advertise the `prompts` capability.
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": result,
            })
        }
        // Notifications must NOT receive a response.
        "notifications/initialized" | "notifications/cancelled" => Value::Null,
        "tools/list" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": tools::tool_definitions_with(state.default_palace.is_some())
        }),
        // OpenRPC 1.3.2 discovery — see `openrpc.rs`. Returns the full
        // service description so orchestrators (trusty-agents, etc.) can
        // introspect every tool and its required `memory.read`/`memory.write`
        // scope without bespoke per-server adapters.
        "rpc.discover" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": openrpc::build_discover_response(
                &state.version,
                state.default_palace.is_some(),
            ),
        }),
        "tools/call" => {
            let params = msg.get("params").cloned().unwrap_or_default();
            let tool_name = params
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();
            let args = params.get("arguments").cloned().unwrap_or_default();
            match tools::dispatch_tool(state, &tool_name, args).await {
                Ok(content) => {
                    // Why: tools that return a bare JSON string (e.g.
                    // `get_prompt_context` returning the formatted
                    // Markdown block) should surface as plain text in the
                    // MCP `content[0].text` field — wrapping in
                    // `Value::to_string()` would re-quote the payload and
                    // force every caller to strip outer quotes.
                    let text = match &content {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "content": [{"type": "text", "text": text}]
                        }
                    })
                }
                Err(e) => json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    // Why: anyhow's `{:#}` alternate format walks the full
                    // `Caused by:` chain so MCP clients see actionable
                    // detail (e.g. "PalaceHandle::remember_with_options:
                    // filter rejected: too short") instead of just the
                    // outermost context label.
                    "error": {"code": -32603, "message": format!("{e:#}")}
                }),
            }
        }
        "ping" => json!({"jsonrpc": "2.0", "id": id, "result": {}}),
        _ => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": -32601,
                "message": format!("Method not found: {method}")
            }
        }),
    }
}
