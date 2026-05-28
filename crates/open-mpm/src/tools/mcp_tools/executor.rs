//! `ToolExecutor` adapter + factory for the MCP management tools. (#244)
//!
//! Why: The CTRL chat turn and PM loop both dispatch through `ToolRegistry`.
//! Wrapping the typed dispatch in a thin adapter lets us register the MCP
//! tools with one call without duplicating schema or dispatch logic.
//! What: `McpManagementTool` forwards `execute()` to `dispatch_mcp_tool`;
//! `mcp_tool_executors()` builds the five tools as `Arc<dyn ToolExecutor>`.
//! Test: Indirectly via the `dispatch_mcp_*` tests.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use super::dispatch::dispatch_mcp_tool;
use crate::tools::traits::{ToolExecutor, ToolResult};

/// `ToolExecutor` wrapper around the five `mcp_*` tools so they can be
/// registered in a `ToolRegistry` alongside other tools.
///
/// Why: The CTRL chat turn and PM loop both dispatch through `ToolRegistry`.
/// Wrapping the typed dispatch in this thin adapter lets us register the
/// MCP tools with one call (`with_mcp_tools`) without duplicating schema or
/// dispatch logic at each call site.
/// What: Holds the static name + schema; `execute()` forwards to
/// `dispatch_mcp_tool`.
/// Test: Indirectly via the `dispatch_mcp_*` tests below.
struct McpManagementTool {
    name: &'static str,
    schema: Value,
}

#[async_trait]
impl ToolExecutor for McpManagementTool {
    fn name(&self) -> &str {
        self.name
    }
    fn schema(&self) -> Value {
        self.schema.clone()
    }
    async fn execute(&self, args: Value) -> ToolResult {
        let result = dispatch_mcp_tool(self.name, &args).await;
        ToolResult::ok(result)
    }
}

/// Build the five MCP management tools as `Arc<dyn ToolExecutor>` so they
/// can be registered into a `ToolRegistry`. (#244)
pub fn mcp_tool_executors() -> Vec<Arc<dyn ToolExecutor>> {
    let definitions = [
        (
            "mcp_list",
            "List all registered MCP services with enabled/disabled status and their available tools.",
            json!({"type":"object","properties":{},"required":[]}),
        ),
        (
            "mcp_add",
            "Register a new MCP service (local stdio or remote HTTP). If a service with the same name already exists, it is replaced.",
            json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string"},
                    "description": {"type": "string"},
                    "transport": {"type": "string", "enum": ["stdio", "http"]},
                    "command": {"type": "string"},
                    "args": {"type": "array", "items": {"type": "string"}},
                    "url": {"type": "string"},
                    "enabled": {"type": "boolean"},
                    "tools": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": {"type": "string"},
                                "description": {"type": "string"}
                            },
                            "required": ["name", "description"]
                        }
                    }
                },
                "required": ["name", "description", "transport"]
            }),
        ),
        (
            "mcp_remove",
            "Remove a registered MCP service by name.",
            json!({
                "type": "object",
                "properties": {"name": {"type": "string"}},
                "required": ["name"]
            }),
        ),
        (
            "mcp_enable",
            "Enable a registered MCP service.",
            json!({
                "type": "object",
                "properties": {"name": {"type": "string"}},
                "required": ["name"]
            }),
        ),
        (
            "mcp_disable",
            "Disable a registered MCP service without removing it.",
            json!({
                "type": "object",
                "properties": {"name": {"type": "string"}},
                "required": ["name"]
            }),
        ),
    ];

    definitions
        .into_iter()
        .map(|(name, desc, params)| {
            let schema = json!({
                "type": "function",
                "function": {
                    "name": name,
                    "description": desc,
                    "parameters": params
                }
            });
            let exec: Arc<dyn ToolExecutor> = Arc::new(McpManagementTool { name, schema });
            exec
        })
        .collect()
}
