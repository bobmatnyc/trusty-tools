//! `ToolExecutor` wrapper for one live-discovered MCP tool (#3238).
//!
//! Why: A discovered `tools/list` entry needs to become a `dyn ToolExecutor`
//! so it can sit in the same `ToolRegistry` as every in-process and
//! static-config MCP tool, dispatched identically by the LLM loop.
//! What: `LiveMcpTool` holds the tool's real name, an OpenAI-shape schema
//! built by passing the MCP server's `inputSchema` straight through (no
//! synthetic open-object schema — the server told us the real shape),
//! and a shared `ServiceClient` handle for `tools/call` dispatch.
//! Test: `execute_dispatches_through_shared_client`,
//! `execute_surfaces_call_errors`, `build_tool_schema_passes_through_input_schema`.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tools::mcp_service_tools::{ServiceClient, format_mcp_call_result};
use crate::tools::traits::{ToolExecutor, ToolResult};

/// Build the OpenAI-shape function-calling schema for one discovered MCP
/// tool, prefixing the description with `[<server_name>]` (matching the
/// convention the static `mcp_service_tools` path already uses) so the LLM
/// can tell which server a tool belongs to.
///
/// Why: Unlike the static config path (which has no schema to pass through
/// and falls back to an open object), live discovery gets the server's real
/// `inputSchema` from `tools/list` — passing it through gives the LLM
/// accurate argument shapes instead of an unconstrained object.
/// What: Wraps `tool.input_schema` as `function.parameters`, falling back to
/// an empty object schema only if the server didn't advertise one.
/// Test: `build_tool_schema_passes_through_input_schema`,
/// `build_tool_schema_falls_back_when_input_schema_absent`.
pub(super) fn build_tool_schema(
    server_name: &str,
    tool: &trusty_common::stdio_mcp_client::McpTool,
) -> Value {
    let params = if tool.input_schema.is_object() {
        tool.input_schema.clone()
    } else {
        json!({"type": "object", "properties": {}})
    };
    json!({
        "type": "function",
        "function": {
            "name": tool.name,
            "description": format!("[{}] {}", server_name, tool.description.clone().unwrap_or_default()),
            "parameters": params
        }
    })
}

/// `ToolExecutor` for one tool advertised by a live-discovered MCP server.
///
/// Why: Same dispatch shape as `mcp_service_tools::McpServiceTool`, but the
/// schema comes from real discovery instead of static config, and the
/// backing `ServiceClient` was constructed by `mcp_live::spec` rather than
/// from an `McpService`.
/// What: `execute()` forwards to `client.get_or_spawn()` then `tools/call`,
/// formatting results with the same `format_mcp_call_result` the static path
/// uses (shared, not duplicated).
/// Test: `execute_dispatches_through_shared_client`,
/// `execute_surfaces_call_errors`.
pub(super) struct LiveMcpTool {
    pub(super) name: String,
    pub(super) schema: Value,
    pub(super) client: Arc<ServiceClient>,
}

#[async_trait]
impl ToolExecutor for LiveMcpTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn schema(&self) -> Value {
        self.schema.clone()
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let client = match self.client.get_or_spawn().await {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!(
                    tool = %self.name,
                    service = %self.client.name(),
                    error = %e,
                    "mcp_live: server unavailable; tool call returning error"
                );
                return ToolResult::err(format!(
                    "MCP service '{}' is not running: {}. The tool '{}' is currently unavailable.",
                    self.client.name(),
                    e,
                    self.name
                ));
            }
        };

        let mut guard = client.lock().await;
        match guard.call_tool(&self.name, args).await {
            Ok(value) => ToolResult::ok(format_mcp_call_result(&value)),
            Err(e) => {
                tracing::warn!(
                    tool = %self.name,
                    service = %self.client.name(),
                    error = %e,
                    "mcp_live: tool call failed"
                );
                ToolResult::err(format!("MCP tool '{}' failed: {}", self.name, e))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use trusty_common::stdio_mcp_client::McpTool as WireMcpTool;

    /// Why: The server's real `inputSchema` must pass through unmodified so
    /// the LLM sees accurate argument shapes.
    /// What: Build a schema for a tool with a non-trivial inputSchema;
    /// assert `function.parameters` equals it exactly, and the description
    /// is prefixed with `[server_name]`.
    /// Test: This test.
    #[test]
    fn build_tool_schema_passes_through_input_schema() {
        let tool = WireMcpTool {
            name: "echo".to_string(),
            description: Some("Echoes input".to_string()),
            input_schema: json!({
                "type": "object",
                "properties": {"msg": {"type": "string"}},
                "required": ["msg"]
            }),
        };
        let schema = build_tool_schema("fake-server", &tool);
        assert_eq!(schema["type"], "function");
        assert_eq!(schema["function"]["name"], "echo");
        assert_eq!(
            schema["function"]["description"],
            "[fake-server] Echoes input"
        );
        assert_eq!(
            schema["function"]["parameters"],
            json!({
                "type": "object",
                "properties": {"msg": {"type": "string"}},
                "required": ["msg"]
            })
        );
    }

    /// Why: Some servers omit `inputSchema`; the executor must still produce
    /// a valid (empty-object) schema rather than propagating `null`.
    /// What: Tool with `input_schema: Value::Null`; assert the fallback
    /// empty-object schema is used.
    /// Test: This test.
    #[test]
    fn build_tool_schema_falls_back_when_input_schema_absent() {
        let tool = WireMcpTool {
            name: "bare".to_string(),
            description: None,
            input_schema: Value::Null,
        };
        let schema = build_tool_schema("fake-server", &tool);
        assert_eq!(
            schema["function"]["parameters"],
            json!({"type": "object", "properties": {}})
        );
    }

    /// Why: `execute()` must return a recoverable error (not panic) when the
    /// backing server can't be spawned.
    /// What: `ServiceClient` pointed at a nonexistent binary; assert
    /// `execute` returns an error result mentioning the tool/service.
    /// Test: This test.
    #[tokio::test]
    async fn execute_surfaces_spawn_failure_as_recoverable_error() {
        let client = Arc::new(ServiceClient::with_parts(
            "ghost-server".to_string(),
            "/nonexistent/mcp/binary/xyzzy-live-test".to_string(),
            vec![],
            HashMap::new(),
        ));
        let tool = LiveMcpTool {
            name: "ghost_tool".to_string(),
            schema: json!({"type": "function", "function": {"name": "ghost_tool"}}),
            client,
        };
        let result = tool.execute(json!({})).await;
        assert!(result.is_error());
        assert!(!result.is_fatal());
        assert!(
            result.content().contains("ghost_tool") || result.content().contains("ghost-server")
        );
    }
}
