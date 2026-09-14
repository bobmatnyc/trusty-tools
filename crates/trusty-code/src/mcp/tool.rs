//! One configured MCP server's tool, as a `ToolExecutor` (#5428).
//!
//! Why: a tool discovered over `tools/list` has to sit in the same
//! [`crate::tools::ToolRegistry`] as every in-process tool so the agent loop
//! dispatches it identically — the seam `trusty_agents::tools::mcp_live` uses
//! for the same job.
//! What: [`McpBridgeTool`] holds the composed registry name, the tool's REAL
//! name on the server, and a shared client handle. `execute` forwards to
//! `tools/call` and renders the MCP content frames as text. Every failure is a
//! recoverable `ToolResult::err`, never fatal: one broken server must not end
//! a task run.
//! Test: `super::tests::tool_tests`, `mcp_loader_e2e::end_to_end_dispatch_echoes_through_the_registry`.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::sync::Mutex;
use trusty_common::stdio_mcp_client::{McpTool, StdioMcpClient};

use crate::tools::traits::{ToolExecutor, ToolResult};

/// Upper bound on a passed-through `inputSchema`, in bytes.
///
/// Why: the advertised schema is cloned into this tool and re-serialised on
/// every `registry.schemas()` call, so a server advertising a huge one is an
/// unbounded per-tool memory and CPU cost (#3266, the same bound
/// `trusty_agents::tools::mcp_live` applies). 64 KiB is far above any
/// legitimate schema observed in this workspace.
/// Test: `super::tests::tool_tests::an_oversized_input_schema_is_rejected`.
pub const MAX_INPUT_SCHEMA_BYTES: usize = 64 * 1024;

/// Compose the registry name for one server's tool.
///
/// Why: `mcp__<server>__<tool>` is Claude Code's own convention, so a model
/// already trained on that surface reads a `tcode` tool list the same way, and
/// the `mcp__` prefix keeps a server's tool from ever colliding with a built-in.
/// Test: `super::tests::tool_tests::composed_names_follow_the_claude_code_convention`.
pub fn composed_name(server: &str, tool: &str) -> String {
    format!("mcp__{server}__{tool}")
}

/// Build the OpenAI-shape function descriptor for one discovered tool.
///
/// Why: the server told us the real argument shape, so passing its
/// `inputSchema` through gives the model accurate arguments instead of an
/// unconstrained object.
/// What: `Err` with a reason when the schema is over [`MAX_INPUT_SCHEMA_BYTES`]
/// — the caller turns that into an issue and SKIPS the tool, per the #5428
/// ruling. A server that advertises no object schema at all gets an
/// empty-object one, which is a missing shape rather than an abusive one.
/// Test: `super::tests::tool_tests::an_oversized_input_schema_is_rejected`,
/// `super::tests::tool_tests::an_absent_input_schema_falls_back_to_an_open_object`.
pub fn build_schema(server: &str, tool: &McpTool) -> Result<Value, String> {
    let params = if tool.input_schema.is_object() {
        let bytes = serde_json::to_vec(&tool.input_schema)
            .map_err(|e| format!("its inputSchema could not be serialised: {e}"))?;
        if bytes.len() > MAX_INPUT_SCHEMA_BYTES {
            return Err(format!(
                "its inputSchema is {} bytes, over the {MAX_INPUT_SCHEMA_BYTES}-byte cap",
                bytes.len()
            ));
        }
        tool.input_schema.clone()
    } else {
        json!({"type": "object", "properties": {}})
    };
    Ok(json!({
        "type": "function",
        "function": {
            "name": composed_name(server, &tool.name),
            "description": format!(
                "[{server}] {}",
                tool.description.clone().unwrap_or_default()
            ),
            "parameters": params,
        }
    }))
}

/// Render an MCP `tools/call` result as the text the model sees.
///
/// Why: MCP returns an array of content frames; the agent loop takes a string.
/// What: joins every `text` frame with a newline. A result carrying no text
/// frame renders as its own JSON, so a structured-only reply is still visible
/// rather than reported as empty.
/// Test: `super::tests::tool_tests::text_frames_are_joined`,
/// `super::tests::tool_tests::a_frameless_result_renders_as_json`.
pub fn render_result(result: &Value) -> String {
    let frames: Vec<&str> = result
        .get("content")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("text").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    if frames.is_empty() {
        return result.to_string();
    }
    frames.join("\n")
}

/// Whether an MCP result carries the in-band `isError` flag.
///
/// Why: a server reports a tool-level failure as `isError: true` content, not
/// as a JSON-RPC error, so `call_tool` returns `Ok` and the flag is the only
/// signal the call actually failed.
/// Test: `super::tests::tool_tests::the_in_band_error_flag_is_detected`.
pub fn is_mcp_error(result: &Value) -> bool {
    result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// A `ToolExecutor` bridging one tool on one configured MCP server.
///
/// Why: the registry dispatches by name and knows nothing about MCP; this is
/// the adapter that keeps it that way.
/// What: `name` is the composed `mcp__<server>__<tool>` key the registry and
/// the advertised schema both use; `remote` is the bare name the server itself
/// answers to. The client is shared (`Arc<Mutex<_>>`) because one stdio pair
/// carries one in-flight request at a time, and because every tool on a server
/// must keep THAT server's child alive — dropping the last handle kills it.
/// Test: `super::tests::tool_tests::execute_surfaces_a_broken_server_recoverably`,
/// `mcp_loader_e2e::end_to_end_dispatch_echoes_through_the_registry`.
pub struct McpBridgeTool {
    name: String,
    remote: String,
    server: String,
    schema: Value,
    client: Arc<Mutex<StdioMcpClient>>,
}

impl McpBridgeTool {
    /// Build one bridge tool, or say why the tool cannot be exposed.
    ///
    /// What: `Err` carries the reason [`build_schema`] refused, for the caller
    /// to record as an issue.
    /// Test: `super::tests::tool_tests::an_oversized_input_schema_is_rejected`.
    pub fn new(
        server: &str,
        tool: &McpTool,
        client: Arc<Mutex<StdioMcpClient>>,
    ) -> Result<Self, String> {
        Ok(Self {
            name: composed_name(server, &tool.name),
            remote: tool.name.clone(),
            server: server.to_string(),
            schema: build_schema(server, tool)?,
            client,
        })
    }

    /// The server this tool belongs to.
    pub fn server(&self) -> &str {
        &self.server
    }
}

#[async_trait]
impl ToolExecutor for McpBridgeTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn schema(&self) -> Value {
        self.schema.clone()
    }

    /// Forward the call to the server, recoverably.
    ///
    /// Why: an MCP server can die, hang or reject at any point in a run, and
    /// none of those may end the agent loop — the model has to be able to read
    /// the failure and choose something else.
    /// What: a transport failure and an in-band `isError` both become
    /// `ToolResult::err` naming the server and the tool, so the model can tell
    /// "this connector is broken" from "my arguments were wrong".
    /// Test: `super::tests::tool_tests::execute_surfaces_a_broken_server_recoverably`,
    /// `mcp_loader_e2e::end_to_end_dispatch_echoes_through_the_registry`.
    async fn execute(&self, args: Value) -> ToolResult {
        let mut client = self.client.lock().await;
        match client.call_tool(&self.remote, args).await {
            Ok(result) if is_mcp_error(&result) => {
                let detail = render_result(&result);
                tracing::warn!(
                    server = %self.server,
                    tool = %self.remote,
                    "mcp: tool reported an error",
                );
                ToolResult::err(format!(
                    "MCP tool '{}' on server '{}' reported an error: {detail}",
                    self.remote, self.server
                ))
            }
            Ok(result) => ToolResult::ok(render_result(&result)),
            Err(e) => {
                tracing::warn!(
                    server = %self.server,
                    tool = %self.remote,
                    error = %e,
                    "mcp: tool call failed",
                );
                ToolResult::err(format!(
                    "MCP tool '{}' on server '{}' failed: {e}",
                    self.remote, self.server
                ))
            }
        }
    }
}
