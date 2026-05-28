//! Dynamic MCP service management tools (#244).
//!
//! Why: Previously the MCP service registry at `~/.open-mpm/config.toml`
//! could only be edited by hand. To let coordinating agents (ctrl, pm)
//! adapt to a user's environment in-flight — "add the github MCP", "turn
//! off slack for now" — we expose five typed tools the LLM can call:
//! `mcp_list`, `mcp_add`, `mcp_remove`, `mcp_enable`, `mcp_disable`.
//! Each tool reads, mutates, and persists `GlobalConfig` via the methods on
//! that type, then returns a short confirmation string. Because every
//! prompt build re-reads the config from disk via `GlobalConfig::load()`, a
//! mutation made in turn N is reflected in the prompt for turn N+1
//! without any caching layer.
//! What: `mcp_tool_definitions()` returns the five `ChatCompletionTool`
//! schemas; `dispatch_mcp_tool(name, args)` performs the corresponding
//! action against `GlobalConfig` and returns a string suitable for the
//! tool-result message back to the LLM.
//! Test: See unit tests at the bottom of this file.

use std::sync::Arc;

use anyhow::{Context, Result};
use async_openai::types::{ChatCompletionTool, ChatCompletionToolArgs, FunctionObjectArgs};
use async_trait::async_trait;
use serde_json::{Value, json};

use crate::mcp::{GlobalConfig, McpService, McpTool};
use crate::tools::traits::{ToolExecutor, ToolResult};

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
        url,
        transport,
        enabled,
        tools,
    })
}

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

#[cfg(test)]
mod tests {
    // Why: These tests hold `HOME_LOCK` (a `std::sync::Mutex`) across async
    // I/O to serialize global $HOME mutation between tests. See
    // `crate::test_env` for the rationale.
    #![allow(clippy::await_holding_lock)]

    use super::*;
    use crate::test_env::HOME_LOCK;
    use std::path::PathBuf;

    fn tempdir() -> PathBuf {
        let p = std::env::temp_dir().join(format!("open-mpm-mcp-tools-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn mcp_tool_definitions_returns_five_tools() {
        let tools = mcp_tool_definitions();
        assert_eq!(tools.len(), 5);
        let names: Vec<&str> = tools.iter().map(|t| t.function.name.as_str()).collect();
        assert!(names.contains(&"mcp_list"));
        assert!(names.contains(&"mcp_add"));
        assert!(names.contains(&"mcp_remove"));
        assert!(names.contains(&"mcp_enable"));
        assert!(names.contains(&"mcp_disable"));
    }

    #[tokio::test]
    async fn dispatch_mcp_list_returns_registered_services() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir();
        unsafe {
            std::env::set_var("HOME", &home);
        }
        // Seed config with one service.
        let mut cfg = GlobalConfig::default();
        cfg.add_service(McpService {
            name: "alpha".to_string(),
            description: "alpha service".to_string(),
            command: "a".to_string(),
            args: vec![],
            url: None,
            transport: "stdio".to_string(),
            enabled: true,
            tools: vec![],
        })
        .await
        .unwrap();

        let out = dispatch_mcp_tool("mcp_list", &json!({})).await;
        assert!(out.contains("alpha"));
        assert!(out.contains("Registered MCP services"));
    }

    #[tokio::test]
    async fn dispatch_mcp_add_persists_service() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir();
        unsafe {
            std::env::set_var("HOME", &home);
        }

        let args = json!({
            "name": "beta",
            "description": "beta service",
            "transport": "stdio",
            "command": "beta-cmd",
            "args": ["mcp"],
            "tools": [
                {"name": "beta_op", "description": "do beta things"}
            ]
        });
        let out = dispatch_mcp_tool("mcp_add", &args).await;
        assert!(out.contains("Added"), "got: {out}");
        assert!(out.contains("beta"));

        // Verify it persists by reloading. Note: #245 — load() now returns
        // documented defaults (gworkspace-mcp + slack-user-proxy) when the
        // file doesn't exist, so mcp_add against a missing file persists
        // those defaults plus the new "beta" service (3 total).
        let reloaded = GlobalConfig::load().await;
        let beta = reloaded
            .mcp
            .services
            .iter()
            .find(|s| s.name == "beta")
            .expect("beta service present");
        assert_eq!(beta.tools.len(), 1);
    }

    #[tokio::test]
    async fn dispatch_mcp_remove_removes_service() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir();
        unsafe {
            std::env::set_var("HOME", &home);
        }
        // Add a service first.
        dispatch_mcp_tool(
            "mcp_add",
            &json!({
                "name": "gamma",
                "description": "g",
                "transport": "stdio",
                "command": "g"
            }),
        )
        .await;

        let out = dispatch_mcp_tool("mcp_remove", &json!({"name": "gamma"})).await;
        assert!(out.contains("Removed"), "got: {out}");

        // #245: defaults remain in the registry; assert gamma is gone.
        let reloaded = GlobalConfig::load().await;
        assert!(!reloaded.mcp.services.iter().any(|s| s.name == "gamma"));

        // Removing again returns the not-found message.
        let again = dispatch_mcp_tool("mcp_remove", &json!({"name": "gamma"})).await;
        assert!(again.contains("No service"), "got: {again}");
    }

    #[tokio::test]
    async fn dispatch_mcp_enable_disable_toggles_flag() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir();
        unsafe {
            std::env::set_var("HOME", &home);
        }
        // Add a disabled service.
        dispatch_mcp_tool(
            "mcp_add",
            &json!({
                "name": "delta",
                "description": "d",
                "transport": "stdio",
                "command": "d",
                "enabled": false
            }),
        )
        .await;

        // Enable it.
        let enable_out = dispatch_mcp_tool("mcp_enable", &json!({"name": "delta"})).await;
        assert!(enable_out.contains("Enabled"), "got: {enable_out}");
        let cfg = GlobalConfig::load().await;
        let delta = cfg
            .mcp
            .services
            .iter()
            .find(|s| s.name == "delta")
            .expect("delta service present");
        assert!(delta.enabled);

        // Disable it.
        let disable_out = dispatch_mcp_tool("mcp_disable", &json!({"name": "delta"})).await;
        assert!(disable_out.contains("Disabled"), "got: {disable_out}");
        let cfg = GlobalConfig::load().await;
        let delta = cfg
            .mcp
            .services
            .iter()
            .find(|s| s.name == "delta")
            .expect("delta service present");
        assert!(!delta.enabled);

        // Unknown name returns not-found.
        let missing = dispatch_mcp_tool("mcp_enable", &json!({"name": "missing"})).await;
        assert!(missing.contains("No service"), "got: {missing}");
    }

    #[tokio::test]
    async fn dispatch_mcp_add_rejects_invalid_transport() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir();
        unsafe {
            std::env::set_var("HOME", &home);
        }
        let out = dispatch_mcp_tool(
            "mcp_add",
            &json!({
                "name": "bad",
                "description": "x",
                "transport": "bogus"
            }),
        )
        .await;
        assert!(out.contains("Invalid"), "got: {out}");
        assert!(out.contains("transport"));
    }

    #[tokio::test]
    async fn dispatch_unknown_tool_returns_error_string() {
        let out = dispatch_mcp_tool("mcp_bogus", &json!({})).await;
        assert!(out.contains("Unknown"));
    }
}
