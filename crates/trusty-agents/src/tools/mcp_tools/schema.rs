//! OpenAI-compatible schema construction for the five `mcp_*` tools. (#244)
//!
//! Why: The PM and ctrl tool registries advertise these tools to the LLM in
//! function-call form. Centralizing schema construction keeps call sites
//! minimal and the schemas consistent.
//! What: `mcp_tool_definitions()` returns the five `ChatCompletionTool`s;
//! `build_tool` is the shared builder helper.
//! Test: `super::mcp_tool_definitions_returns_five_tools`.

use anyhow::{Context, Result};
use async_openai::types::{ChatCompletionTool, ChatCompletionToolArgs, FunctionObjectArgs};
use serde_json::{Value, json};

/// Build the five `ChatCompletionTool` schemas for MCP management. (#244)
///
/// Why: The PM and ctrl tool registries need to advertise these tools to
/// the LLM in OpenAI-compatible function-call form alongside their other
/// tools (delegate_to_agent, etc.). Centralizing schema construction here
/// keeps the call sites minimal.
/// What: Returns a `Vec<ChatCompletionTool>` of length 5: `mcp_list`,
/// `mcp_add`, `mcp_remove`, `mcp_enable`, `mcp_disable`.
/// Test: `mcp_tool_definitions_returns_five_tools`.
#[allow(dead_code)]
pub fn mcp_tool_definitions() -> Vec<ChatCompletionTool> {
    let defs = [
        (
            "mcp_list",
            "List all registered MCP services with enabled/disabled status and their available tools.",
            json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        ),
        (
            "mcp_add",
            "Register a new MCP service (local stdio or remote HTTP). If a service with the same name already exists, it is replaced.",
            json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "Unique service identifier"},
                    "description": {"type": "string", "description": "Human-readable description"},
                    "transport": {"type": "string", "enum": ["stdio", "http"], "description": "Transport type"},
                    "command": {"type": "string", "description": "For stdio: command to run (e.g. 'gworkspace-mcp')"},
                    "args": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "For stdio: command arguments (e.g. ['mcp'])"
                    },
                    "url": {"type": "string", "description": "For http: service endpoint URL"},
                    "enabled": {"type": "boolean", "description": "Whether to enable immediately (default: true)"},
                    "tools": {
                        "type": "array",
                        "description": "Optional list of tools this service provides",
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
                "properties": {
                    "name": {"type": "string"}
                },
                "required": ["name"]
            }),
        ),
        (
            "mcp_enable",
            "Enable a registered MCP service.",
            json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string"}
                },
                "required": ["name"]
            }),
        ),
        (
            "mcp_disable",
            "Disable a registered MCP service without removing it.",
            json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string"}
                },
                "required": ["name"]
            }),
        ),
    ];

    let mut out = Vec::with_capacity(defs.len());
    for (name, desc, params) in defs {
        match build_tool(name, desc, params) {
            Ok(t) => out.push(t),
            Err(e) => {
                tracing::warn!(error = %e, tool = name, "failed to build mcp tool def");
            }
        }
    }
    out
}

/// Build a single `ChatCompletionTool` from a name/description/params triple.
///
/// Why: Shared by `mcp_tool_definitions` so each of the five schemas is
/// constructed identically.
/// What: Wraps `FunctionObjectArgs` + `ChatCompletionToolArgs` with context.
/// Test: Exercised via `mcp_tool_definitions_returns_five_tools`.
#[allow(dead_code)]
fn build_tool(name: &str, description: &str, params: Value) -> Result<ChatCompletionTool> {
    let func = FunctionObjectArgs::default()
        .name(name)
        .description(description)
        .parameters(params)
        .build()
        .with_context(|| format!("failed to build {name} function object"))?;
    ChatCompletionToolArgs::default()
        .function(func)
        .build()
        .with_context(|| format!("failed to build {name} ChatCompletionTool"))
}
