//! `ToolExecutor` wrapper for one live-discovered MCP tool (#3238).
//!
//! Why: A discovered `tools/list` entry needs to become a `dyn ToolExecutor`
//! so it can sit in the same `ToolRegistry` as every in-process and
//! static-config MCP tool, dispatched identically by the LLM loop.
//! What: `LiveMcpTool` holds the tool's real name, an OpenAI-shape schema
//! built by passing the MCP server's `inputSchema` straight through (no
//! synthetic open-object schema — the server told us the real shape),
//! and a shared `ServiceClient` handle for `tools/call` dispatch.
//! Test: `execute_surfaces_spawn_failure_as_recoverable_error`,
//! `build_tool_schema_passes_through_input_schema`.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tools::mcp_service_tools::{ServiceClient, format_mcp_call_result};
use crate::tools::traits::{ToolExecutor, ToolResult};

/// Upper bound on a passed-through `inputSchema`'s serialized size, in
/// bytes. A malicious or buggy MCP server could otherwise advertise an
/// arbitrarily large `inputSchema` (deeply nested, huge enum lists, etc.)
/// that gets cloned into every `LiveMcpTool`'s schema and re-serialized on
/// every `registry.schemas()` call — unbounded memory/CPU per discovered
/// tool. 64 KiB comfortably covers legitimate real-world schemas (the
/// largest observed in this codebase's fixtures is a few hundred bytes)
/// while bounding the worst case.
const MAX_INPUT_SCHEMA_BYTES: usize = 64 * 1024;

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
/// an empty object schema if the server didn't advertise one OR if the
/// advertised schema exceeds `MAX_INPUT_SCHEMA_BYTES` (#3266 memory-bound
/// fix) — an oversized schema is treated the same as a missing one rather
/// than rejecting the whole tool, so the tool still registers (with a less
/// precise, open-object parameter shape) instead of vanishing.
/// Test: `build_tool_schema_passes_through_input_schema`,
/// `build_tool_schema_falls_back_when_input_schema_absent`,
/// `build_tool_schema_falls_back_when_input_schema_exceeds_size_cap`.
pub(super) fn build_tool_schema(
    server_name: &str,
    tool: &trusty_common::stdio_mcp_client::McpTool,
) -> Value {
    let fallback = || json!({"type": "object", "properties": {}});
    let params = if !tool.input_schema.is_object() {
        fallback()
    } else {
        match serde_json::to_vec(&tool.input_schema) {
            Ok(bytes) if bytes.len() <= MAX_INPUT_SCHEMA_BYTES => tool.input_schema.clone(),
            Ok(bytes) => {
                tracing::warn!(
                    server = %server_name,
                    tool = %tool.name,
                    schema_bytes = bytes.len(),
                    cap_bytes = MAX_INPUT_SCHEMA_BYTES,
                    "mcp_live: inputSchema exceeds size cap; falling back to an open-object schema"
                );
                fallback()
            }
            Err(_) => fallback(),
        }
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
/// Test: `execute_surfaces_spawn_failure_as_recoverable_error`.
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

    /// Why: SECURITY/DoS (#3266) — a malicious or buggy MCP server could
    /// advertise an oversized `inputSchema` (e.g. a huge enum or deeply
    /// nested object). Without a cap, that gets cloned into the schema of
    /// every registered `LiveMcpTool` and re-serialized on every registry
    /// listing, an unbounded per-tool memory/CPU cost.
    /// What: Build an `inputSchema` whose serialized size exceeds
    /// `MAX_INPUT_SCHEMA_BYTES`; assert the resulting `function.parameters`
    /// falls back to the empty-object schema (tool still registers, just
    /// with a less precise parameter shape) rather than passing the huge
    /// schema through.
    /// Test: This test.
    #[test]
    fn build_tool_schema_falls_back_when_input_schema_exceeds_size_cap() {
        let huge_enum: Vec<String> = (0..20_000).map(|i| format!("value-{i}")).collect();
        let tool = WireMcpTool {
            name: "oversized".to_string(),
            description: Some("Has a huge inputSchema".to_string()),
            input_schema: json!({
                "type": "object",
                "properties": {"choice": {"type": "string", "enum": huge_enum}}
            }),
        };
        let schema = build_tool_schema("fake-server", &tool);
        assert_eq!(
            schema["function"]["parameters"],
            json!({"type": "object", "properties": {}}),
            "oversized inputSchema must fall back, not pass through"
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
