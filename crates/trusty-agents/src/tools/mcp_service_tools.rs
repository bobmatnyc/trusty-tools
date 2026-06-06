//! Live MCP service tools — wraps tools advertised by enabled MCP services
//! in `GlobalConfig` as `ToolExecutor` instances so persona registries can
//! invoke them (e.g. `granola_*`, `gmail_*`, calendar tools).
//!
//! Why: Previously the persona registry (see `ctrl/mod.rs` ~line 1601) only
//! registered the five `mcp_*` management tools (mcp_list/add/remove/enable/
//! disable) plus git and ticketing. The actual tools exposed by running MCP
//! servers — `granola_search`, `gmail_send`, etc. listed in
//! `~/.trusty-agents/config.toml` — were never instantiated as `ToolExecutor`s,
//! so personas like Izzie could *see* `granola_*` in their `allow = [...]`
//! glob but the registry would dispatch nothing.
//!
//! What: `mcp_service_tool_executors()` reads `GlobalConfig::load()`, walks
//! each enabled service, and builds one `Arc<dyn ToolExecutor>` per
//! advertised tool. Each executor lazily spawns an `StdioMcpClient` on first
//! call (cached per-service via a `OnceCell<Arc<Mutex<StdioMcpClient>>>`),
//! then forwards `tools/call` requests. Failures (server not on PATH,
//! handshake failure, JSON-RPC error) surface as `ToolResult::Error` so the
//! LLM can recover instead of panicking the loop.
//!
//! Test: `mcp_service_tool_executors_returns_tools_for_enabled_services` and
//! `disabled_services_are_skipped` cover the static path; the live spawn
//! path is exercised opportunistically when binaries are present.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::sync::{Mutex, OnceCell};

use crate::mcp::{GlobalConfig, McpService};
use crate::plugins::stdio_mcp::StdioMcpClient;
use crate::tools::traits::{ToolExecutor, ToolResult};

/// Lazily-spawned MCP client for a single service. Multiple `McpServiceTool`
/// instances belonging to the same service share one `ServiceClient` so we
/// open exactly one subprocess per server.
///
/// Why: Spawning a new MCP child per tool call would be wasteful and would
/// break stateful servers. A `OnceCell<Mutex<StdioMcpClient>>` keyed off the
/// service name is the minimal cache that gives us "spawn on first use,
/// reuse thereafter" semantics safely across tokio tasks.
/// What: `get_or_spawn()` returns the cached client (running handshake on
/// first call); subsequent calls return the same handle. The inner mutex
/// serialises JSON-RPC requests, which `StdioMcpClient` requires.
/// Test: Indirectly via `McpServiceTool::execute` — see integration tests.
struct ServiceClient {
    name: String,
    command: String,
    args: Vec<String>,
    cell: OnceCell<Arc<Mutex<StdioMcpClient>>>,
}

impl ServiceClient {
    fn new(service: &McpService) -> Self {
        Self {
            name: service.name.clone(),
            command: service.command.clone(),
            args: service.args.clone(),
            cell: OnceCell::new(),
        }
    }

    /// Spawn the MCP server subprocess (if not already) and return the shared
    /// client. Returns `Err` if the binary isn't on PATH, the handshake
    /// fails, or any I/O error occurs — callers convert this to a
    /// `ToolResult::Error` for the LLM.
    async fn get_or_spawn(&self) -> anyhow::Result<Arc<Mutex<StdioMcpClient>>> {
        self.cell
            .get_or_try_init(|| async {
                let arg_refs: Vec<&str> = self.args.iter().map(String::as_str).collect();
                let mut client = StdioMcpClient::spawn(&self.command, &arg_refs).await?;
                client.initialize().await?;
                tracing::debug!(service = %self.name, "MCP service client spawned");
                Ok::<_, anyhow::Error>(Arc::new(Mutex::new(client)))
            })
            .await
            .cloned()
    }
}

/// `ToolExecutor` for one MCP tool advertised by an enabled service.
///
/// Why: Each tool the LLM might call needs its own schema and dispatch entry
/// in `ToolRegistry`; sharing a `ServiceClient` across tools belonging to
/// the same service keeps subprocess count bounded.
/// What: Holds the tool name, description, a precomputed OpenAI-shape
/// schema, and an `Arc<ServiceClient>` for dispatch. `execute()` forwards
/// args verbatim to `client.call_tool()` then renders the response (which
/// MCP defines as `{ "content": [{"type":"text","text":...}, ...] }`) into
/// a single string for the tool-result message.
/// Test: `formats_text_content`, `surfaces_call_errors`.
struct McpServiceTool {
    name: String,
    schema: Value,
    client: Arc<ServiceClient>,
}

#[async_trait]
impl ToolExecutor for McpServiceTool {
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
                // Server not running / not on PATH / handshake failed. This
                // is the graceful-degradation path — surface a recoverable
                // error so the LLM can pick a different tool or apologize.
                tracing::debug!(
                    tool = %self.name,
                    service = %self.client.name,
                    error = %e,
                    "MCP service unavailable; tool call returning error"
                );
                return ToolResult::err(format!(
                    "MCP service '{}' is not running: {}. The tool '{}' is currently unavailable.",
                    self.client.name, e, self.name
                ));
            }
        };

        let mut guard = client.lock().await;
        match guard.call_tool(&self.name, args).await {
            Ok(value) => ToolResult::ok(format_mcp_call_result(&value)),
            Err(e) => {
                tracing::warn!(
                    tool = %self.name,
                    service = %self.client.name,
                    error = %e,
                    "MCP tool call failed"
                );
                ToolResult::err(format!("MCP tool '{}' failed: {}", self.name, e))
            }
        }
    }
}

/// Render an MCP `tools/call` result into a human/LLM-readable string.
///
/// Why: MCP returns `{ "content": [{ "type": "text", "text": "..." }, ...] }`.
/// The LLM consumes our `ToolResult` as a string, so we concatenate the text
/// frames. Non-text content (resource refs, images) falls back to JSON.
/// What: If `content` is an array, joins its `text` fields with newlines;
/// otherwise returns the full JSON serialization.
/// Test: `format_mcp_call_result_*` unit tests.
fn format_mcp_call_result(value: &Value) -> String {
    if let Some(items) = value.get("content").and_then(|v| v.as_array()) {
        let mut parts: Vec<String> = Vec::with_capacity(items.len());
        for item in items {
            if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
                parts.push(text.to_string());
            } else {
                parts.push(item.to_string());
            }
        }
        if !parts.is_empty() {
            // Surface the `isError` flag inline so the model knows it failed
            // even when the server returned a 200 with an error payload.
            let is_error = value
                .get("isError")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let body = parts.join("\n");
            return if is_error {
                format!("(MCP server reported error)\n{body}")
            } else {
                body
            };
        }
    }
    value.to_string()
}

/// Build live MCP service tool executors from the global config (#447-followup).
///
/// Why: Persona registries (Izzie etc.) need to invoke `granola_*`,
/// `gmail_*`, etc. — tools advertised by enabled MCP services. This builder
/// is the single entry point: read `GlobalConfig`, walk enabled services,
/// emit one executor per advertised tool. Disabled services are skipped.
/// What: Async because `GlobalConfig::load()` is async. Per-service
/// `Arc<ServiceClient>` is shared across that service's tools so we spawn
/// at most one MCP subprocess per server, lazily on first call.
/// Test: `mcp_service_tool_executors_returns_tools_for_enabled_services`,
/// `disabled_services_are_skipped`.
pub async fn mcp_service_tool_executors() -> Vec<Arc<dyn ToolExecutor>> {
    let config = GlobalConfig::load().await;
    build_executors_from_services(&config.mcp.services)
}

/// Pure helper that turns a service list into executors. Split out so tests
/// can feed synthetic configs without touching `~/.trusty-agents/config.toml`.
fn build_executors_from_services(services: &[McpService]) -> Vec<Arc<dyn ToolExecutor>> {
    let mut executors: Vec<Arc<dyn ToolExecutor>> = Vec::new();
    // Used to dedupe across services in case two servers advertise the same
    // tool name — first declaration wins.
    let mut seen: HashMap<String, String> = HashMap::new();

    for svc in services {
        if !svc.enabled {
            continue;
        }
        if svc.tools.is_empty() {
            tracing::debug!(
                service = %svc.name,
                "MCP service has no static tools listed in config; skipping (live tools/list discovery would require spawning the server at registry build time)"
            );
            continue;
        }
        // Stdio-only for now: HTTP MCP transport isn't wired into
        // StdioMcpClient. Skip rather than fail.
        if svc.transport != "stdio" {
            tracing::debug!(
                service = %svc.name,
                transport = %svc.transport,
                "skipping non-stdio MCP service (only stdio transport supported)"
            );
            continue;
        }
        if svc.command.is_empty() {
            tracing::debug!(
                service = %svc.name,
                "skipping MCP service with empty command"
            );
            continue;
        }
        let shared_client = Arc::new(ServiceClient::new(svc));
        for tool in &svc.tools {
            if let Some(existing_service) = seen.get(&tool.name) {
                tracing::debug!(
                    tool = %tool.name,
                    new_service = %svc.name,
                    existing_service = %existing_service,
                    "duplicate MCP tool name; keeping first registration"
                );
                continue;
            }
            seen.insert(tool.name.clone(), svc.name.clone());

            // Synthesise an OpenAI-shape schema. The config doesn't store an
            // input schema, so accept an open object — the MCP server will
            // validate args server-side and we surface any error to the LLM.
            let schema = json!({
                "type": "function",
                "function": {
                    "name": tool.name,
                    "description": format!("[{}] {}", svc.name, tool.description),
                    "parameters": {
                        "type": "object",
                        "properties": {},
                        "additionalProperties": true
                    }
                }
            });
            let exec: Arc<dyn ToolExecutor> = Arc::new(McpServiceTool {
                name: tool.name.clone(),
                schema,
                client: shared_client.clone(),
            });
            executors.push(exec);
        }
    }
    tracing::info!(
        count = executors.len(),
        "built live MCP service tool executors"
    );
    executors
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::{McpService, McpTool};

    fn svc(name: &str, enabled: bool, tools: &[&str]) -> McpService {
        McpService {
            name: name.to_string(),
            description: format!("{name} service"),
            command: format!("{name}-bin"),
            args: vec!["mcp".to_string()],
            url: None,
            transport: "stdio".to_string(),
            enabled,
            tools: tools
                .iter()
                .map(|t| McpTool {
                    name: (*t).to_string(),
                    description: format!("{t} description"),
                })
                .collect(),
        }
    }

    /// Why: Enabled services with non-empty tool lists must produce one
    /// executor per advertised tool, with schemas the LLM can call.
    /// What: Build a fake service with three tools, assert all three show up
    /// with correct names and an OpenAI-shaped schema.
    /// Test: This test.
    #[test]
    fn build_executors_emits_one_per_tool() {
        let services = vec![svc(
            "granola-mcp",
            true,
            &["granola_search", "granola_get", "granola_list"],
        )];
        let execs = build_executors_from_services(&services);
        assert_eq!(execs.len(), 3);
        let names: Vec<&str> = execs.iter().map(|e| e.name()).collect();
        assert!(names.contains(&"granola_search"));
        assert!(names.contains(&"granola_get"));
        assert!(names.contains(&"granola_list"));

        // Schema sanity: must be the OpenAI function-calling shape.
        let schema = execs[0].schema();
        assert_eq!(schema["type"], "function");
        assert!(
            schema["function"]["name"]
                .as_str()
                .is_some_and(|s| s.starts_with("granola_"))
        );
        assert_eq!(schema["function"]["parameters"]["type"], "object");
    }

    /// Why: Disabled services must not contribute tools — that's the whole
    /// point of the `enabled` flag.
    /// What: Disable a service, verify zero executors come back.
    /// Test: This test.
    #[test]
    fn disabled_services_are_skipped() {
        let services = vec![svc("granola-mcp", false, &["granola_search"])];
        assert!(build_executors_from_services(&services).is_empty());
    }

    /// Why: Services without a static tool list (or empty list) get skipped
    /// rather than producing zero-tool clutter. Live `tools/list` discovery
    /// is deliberately not done at registry-build time to avoid blocking
    /// persona startup on slow/dead servers.
    /// What: Enabled service with no tools yields zero executors.
    /// Test: This test.
    #[test]
    fn services_with_no_tools_are_skipped() {
        let services = vec![svc("empty-svc", true, &[])];
        assert!(build_executors_from_services(&services).is_empty());
    }

    /// Why: Non-stdio transports aren't wired into StdioMcpClient yet;
    /// silently skipping is the documented graceful-degradation behaviour.
    /// What: HTTP-transport service is filtered out.
    /// Test: This test.
    #[test]
    fn non_stdio_services_are_skipped() {
        let mut s = svc("http-svc", true, &["http_op"]);
        s.transport = "http".to_string();
        s.url = Some("https://example.com".to_string());
        assert!(build_executors_from_services(&[s]).is_empty());
    }

    /// Why: Duplicate tool names across services would create ambiguous
    /// dispatch; first registration wins so behaviour is deterministic.
    /// What: Two services both advertise `shared_tool`; only one executor
    /// appears.
    /// Test: This test.
    #[test]
    fn duplicate_tool_names_keep_first() {
        let services = vec![
            svc("a", true, &["shared_tool"]),
            svc("b", true, &["shared_tool", "b_only"]),
        ];
        let execs = build_executors_from_services(&services);
        assert_eq!(execs.len(), 2);
        let names: Vec<&str> = execs.iter().map(|e| e.name()).collect();
        assert!(names.contains(&"shared_tool"));
        assert!(names.contains(&"b_only"));
    }

    /// Why: MCP `content` text frames are the primary payload format;
    /// joining their text fields is what the LLM expects to see.
    /// What: Synthetic call_tool result with two text frames; assert the
    /// formatted string has both, newline-joined.
    /// Test: This test.
    #[test]
    fn format_mcp_call_result_joins_text_frames() {
        let resp = json!({
            "content": [
                {"type": "text", "text": "first line"},
                {"type": "text", "text": "second line"}
            ]
        });
        let out = format_mcp_call_result(&resp);
        assert_eq!(out, "first line\nsecond line");
    }

    /// Why: When MCP reports `isError: true`, the prefix must surface so the
    /// LLM can react to the failure rather than treat it as success.
    /// What: Synthetic error result; assert prefix is present.
    /// Test: This test.
    #[test]
    fn format_mcp_call_result_marks_errors() {
        let resp = json!({
            "isError": true,
            "content": [{"type": "text", "text": "auth failed"}]
        });
        let out = format_mcp_call_result(&resp);
        assert!(out.contains("MCP server reported error"));
        assert!(out.contains("auth failed"));
    }

    /// Why: Non-text content (e.g. resource references) shouldn't be
    /// silently dropped — fall back to the JSON serialisation so the LLM
    /// at least sees something structured.
    /// What: Frame with no `text` field falls through to JSON dump.
    /// Test: This test.
    #[test]
    fn format_mcp_call_result_falls_back_to_json() {
        let resp = json!({
            "content": [{"type": "resource", "uri": "file:///x"}]
        });
        let out = format_mcp_call_result(&resp);
        assert!(out.contains("resource"));
        assert!(out.contains("file:///x"));
    }

    /// Why: When the binary doesn't exist, `execute()` must return a
    /// recoverable error string (not panic, not hang waiting on a timeout).
    /// What: Build an executor pointed at a non-existent binary, call it,
    /// assert recoverable error.
    /// Test: This test.
    #[tokio::test]
    async fn execute_returns_error_when_binary_missing() {
        let services = vec![McpService {
            name: "nonexistent".to_string(),
            description: "missing".to_string(),
            command: "/nonexistent/mcp/binary/xyzzy-tool-test".to_string(),
            args: vec![],
            url: None,
            transport: "stdio".to_string(),
            enabled: true,
            tools: vec![McpTool {
                name: "ghost_tool".to_string(),
                description: "won't run".to_string(),
            }],
        }];
        let execs = build_executors_from_services(&services);
        assert_eq!(execs.len(), 1);
        let result = execs[0].execute(json!({})).await;
        assert!(result.is_error(), "should error when binary missing");
        // Recoverable error — loop should continue.
        assert!(!result.is_fatal());
    }
}
