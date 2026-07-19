//! Dispatch + argument-parsing for the MCP management tools. (#244)
//!
//! Why: A single dispatch point keeps the PM/ctrl tool-call glue short — they
//! just match the five names and forward here. Each turn re-reads
//! `GlobalConfig` from disk so a mutation in turn N is reflected in turn N+1.
//! What: `dispatch_mcp_tool(name, args)` applies the requested mutation and
//! returns a user-readable string; `parse_service_from_args` builds an
//! `McpService` from the `mcp_add` argument object.
//! Test: `super::dispatch_mcp_*` cases.

use anyhow::{Context, Result};
use serde_json::Value;

use crate::mcp::{GlobalConfig, McpService, McpTool};

/// Dispatch an MCP management tool by name. (#244)
///
/// Why: Single dispatch point keeps the PM/ctrl tool-call dispatch glue
/// short — they just match the five names and forward to this function.
/// What: Loads the global `GlobalConfig`, applies the requested mutation,
/// persists, and returns a string for the tool-result message. Errors
/// (bad args, file write failures) are converted to user-readable strings
/// so the LLM can recover in the next turn.
/// Test: `dispatch_mcp_*` cases below.
pub async fn dispatch_mcp_tool(name: &str, args: &Value) -> String {
    match name {
        "mcp_list" => GlobalConfig::load().await.render_list(),
        "mcp_add" => match parse_service_from_args(args) {
            Ok(svc) => {
                let mut cfg = GlobalConfig::load().await;
                let svc_name = svc.name.clone();
                match cfg.add_service(svc).await {
                    Ok(()) => format!("Added MCP service '{svc_name}'."),
                    Err(e) => format!("Failed to add MCP service: {e:#}"),
                }
            }
            Err(e) => format!("Invalid mcp_add arguments: {e:#}"),
        },
        "mcp_remove" => match args.get("name").and_then(Value::as_str) {
            Some(n) => {
                let mut cfg = GlobalConfig::load().await;
                match cfg.remove_service(n).await {
                    Ok(true) => format!("Removed MCP service '{n}'."),
                    Ok(false) => format!("No service named '{n}' found."),
                    Err(e) => format!("Failed to remove MCP service: {e:#}"),
                }
            }
            None => "Invalid mcp_remove arguments: 'name' is required".to_string(),
        },
        "mcp_enable" => match args.get("name").and_then(Value::as_str) {
            Some(n) => {
                let mut cfg = GlobalConfig::load().await;
                match cfg.enable_service(n).await {
                    Ok(true) => format!("Enabled MCP service '{n}'."),
                    Ok(false) => format!("No service named '{n}' found."),
                    Err(e) => format!("Failed to enable MCP service: {e:#}"),
                }
            }
            None => "Invalid mcp_enable arguments: 'name' is required".to_string(),
        },
        "mcp_disable" => match args.get("name").and_then(Value::as_str) {
            Some(n) => {
                let mut cfg = GlobalConfig::load().await;
                match cfg.disable_service(n).await {
                    Ok(true) => format!("Disabled MCP service '{n}'."),
                    Ok(false) => format!("No service named '{n}' found."),
                    Err(e) => format!("Failed to disable MCP service: {e:#}"),
                }
            }
            None => "Invalid mcp_disable arguments: 'name' is required".to_string(),
        },
        other => format!("Unknown MCP tool '{other}'"),
    }
}

/// Parse the `mcp_add` argument object into an `McpService`.
fn parse_service_from_args(args: &Value) -> Result<McpService> {
    let name = args
        .get("name")
        .and_then(Value::as_str)
        .context("'name' is required")?
        .to_string();
    let description = args
        .get("description")
        .and_then(Value::as_str)
        .context("'description' is required")?
        .to_string();
    let transport = args
        .get("transport")
        .and_then(Value::as_str)
        .context("'transport' is required")?
        .to_string();
    if transport != "stdio" && transport != "http" {
        anyhow::bail!("'transport' must be 'stdio' or 'http' (got '{transport}')");
    }
    let command = args
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let args_vec = args
        .get("args")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let url = args.get("url").and_then(Value::as_str).map(str::to_string);
    let enabled = args.get("enabled").and_then(Value::as_bool).unwrap_or(true);
    // SECURITY (#3266 follow-up): `discover` is a declared `mcp_add` schema
    // field, but it is intentionally forced to `false` here regardless of
    // what an LLM-originated tool call requests. `gather_specs`
    // (`tools/mcp_live/spec.rs`) auto-spawns ANY `enabled && discover &&
    // stdio` service with no trust gate of its own — the `.mcp.json` trust
    // gate only covers the `.mcp.json` ingestion path. Without this strip, a
    // prompt-injected or hallucinated `mcp_add` call could set
    // `discover: true` with an attacker-chosen `command`, `add_service`
    // would persist it, and the very next persona turn's `gather_specs` call
    // would auto-spawn that binary — a full trust-gate bypass. Live
    // discovery remains available, but only via a human hand-editing
    // `~/.trusty-agents/config.toml` directly, same as the documented `env`
    // path above.
    let discover = false;
    if args.get("discover").and_then(Value::as_bool) == Some(true) {
        tracing::warn!(
            "mcp_add: rejected 'discover: true' from LLM-originated tool call; service added with discover=false"
        );
    }
    // SECURITY (#3266): `env` is intentionally NOT read from `args` here.
    // `mcp_add` is dispatched from LLM-originated tool calls (the persona's
    // model decides the arguments); `env` is not declared in this tool's
    // schema (`mcp_tool_definitions` in `schema.rs`), so any `env` object an
    // LLM call includes is undeclared input. Silently honoring it would let
    // a prompt-injected or hallucinated tool call inject arbitrary
    // environment variables (credentials, `LD_PRELOAD`-style hijacks, proxy
    // overrides) into a spawned MCP server's process environment. Services
    // that genuinely need env vars are configured by a human editing
    // `~/.trusty-agents/config.toml` directly, not via this LLM-facing tool.
    let env: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    if args.get("env").is_some() {
        tracing::warn!(
            "mcp_add: rejected undeclared 'env' field from LLM-originated tool call; service added with no env vars"
        );
    }
    let tools = args
        .get("tools")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|t| {
                    let n = t.get("name").and_then(Value::as_str)?;
                    let d = t.get("description").and_then(Value::as_str)?;
                    Some(McpTool {
                        name: n.to_string(),
                        description: d.to_string(),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Ok(McpService {
        name,
        description,
        command,
        args: args_vec,
        env,
        url,
        transport,
        enabled,
        tools,
        discover,
    })
}
