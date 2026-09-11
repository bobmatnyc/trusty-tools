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

use trusty_mcp::config::McpServerConfig;

use crate::mcp::extensions;
use crate::plugins::stdio_mcp::StdioMcpClient;
use crate::tools::traits::{ToolExecutor, ToolResult};

/// Lazily-spawned MCP client for a single service. Multiple `McpServiceTool`
/// instances belonging to the same service share one `ServiceClient` so we
/// open exactly one subprocess per server. Shared (as `pub(crate)`) with
/// `crate::tools::mcp_live`, which spawns the same kind of client for
/// *live-discovered* services (#3238) — the caching/spawn shape is
/// >80% identical between the static and live-discovery paths, so this is
/// the single consolidated implementation rather than two near-duplicates.
///
/// Why: Spawning a new MCP child per tool call would be wasteful and would
/// break stateful servers. A `OnceCell<Mutex<StdioMcpClient>>` keyed off the
/// service name is the minimal cache that gives us "spawn on first use,
/// reuse thereafter" semantics safely across tokio tasks.
/// What: `get_or_spawn()` returns the cached client (running handshake on
/// first call); subsequent calls return the same handle. The inner mutex
/// serialises JSON-RPC requests, which `StdioMcpClient` requires. `env` is
/// overlaid on the spawned process's environment (empty for services that
/// don't need credentials).
/// Test: Indirectly via `McpServiceTool::execute` — see integration tests;
/// `crate::tools::mcp_live` tests exercise the `list_tools()` path.
pub(crate) struct ServiceClient {
    name: String,
    command: String,
    args: Vec<String>,
    env: HashMap<String, String>,
    cell: OnceCell<Arc<Mutex<StdioMcpClient>>>,
}

impl ServiceClient {
    /// Construct from a resolved server, when its transport is stdio.
    ///
    /// Why: `#[non_exhaustive]` `McpTransport` means the caller cannot match
    /// it inline; [`extensions::stdio_parts`] owns that match, with its
    /// wildcard arm, in one place.
    /// What: `None` for any non-stdio transport — nothing here can spawn one.
    /// Test: `mcp_service_tool_executors_returns_tools_for_enabled_services`.
    fn from_server(server: &McpServerConfig) -> Option<Self> {
        let (command, args, env) = extensions::stdio_parts(server)?;
        Some(Self::with_parts(
            server.name.clone(),
            command.to_string(),
            args.to_vec(),
            env.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        ))
    }

    /// Construct from primitive parts rather than a resolved server — used
    /// by `mcp_live`, whose server specs carry the same three fields already
    /// flattened.
    pub(crate) fn with_parts(
        name: String,
        command: String,
        args: Vec<String>,
        env: HashMap<String, String>,
    ) -> Self {
        Self {
            name,
            command,
            args,
            env,
            cell: OnceCell::new(),
        }
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// Spawn the MCP server subprocess (if not already) and return the shared
    /// client. Returns `Err` if the binary isn't on PATH, the handshake
    /// fails, or any I/O error occurs — callers convert this to a
    /// `ToolResult::Error` for the LLM (or, for `mcp_live` discovery, a
    /// warn-and-skip for that server).
    pub(crate) async fn get_or_spawn(&self) -> anyhow::Result<Arc<Mutex<StdioMcpClient>>> {
        self.cell
            .get_or_try_init(|| async {
                let arg_refs: Vec<&str> = self.args.iter().map(String::as_str).collect();
                let mut client = StdioMcpClient::spawn_with_env(
                    &self.command,
                    &arg_refs,
                    "trusty-agents",
                    &self.env,
                )
                .await?;
                client.initialize().await?;
                tracing::debug!(service = %self.name, "MCP service client spawned");
                Ok::<_, anyhow::Error>(Arc::new(Mutex::new(client)))
            })
            .await
            .cloned()
    }

    /// Spawn (if needed) and return the tools the server advertises via
    /// `tools/list`. Used only by the live-discovery path (`mcp_live`) —
    /// the static path in this module gets its tool list from config.
    pub(crate) async fn list_tools(
        &self,
    ) -> anyhow::Result<Vec<trusty_common::stdio_mcp_client::McpTool>> {
        let client = self.get_or_spawn().await?;
        let mut guard = client.lock().await;
        guard.list_tools().await
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
struct StaticMcpTool {
    name: String,
    schema: Value,
    client: Arc<ServiceClient>,
}

#[async_trait]
impl ToolExecutor for StaticMcpTool {
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
/// Shared (as `pub(crate)`) with `crate::tools::mcp_live`, which formats
/// `tools/call` results identically for live-discovered tools.
/// What: If `content` is an array, joins its `text` fields with newlines;
/// otherwise returns the full JSON serialization.
/// Test: `format_mcp_call_result_*` unit tests.
pub(crate) fn format_mcp_call_result(value: &Value) -> String {
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
pub async fn mcp_service_tool_executors(
    assistant: Option<&str>,
    project_dir: &std::path::Path,
) -> Vec<Arc<dyn ToolExecutor>> {
    let resolved = crate::mcp::resolve_for_assistant(assistant, project_dir).await;
    build_executors_from_servers(&resolved.usable())
}

/// Pure helper that turns a resolved server list into executors. Split out so
/// tests can feed synthetic servers without touching any config file.
///
/// Why: the caller passes `ResolvedMcp::usable()`, so a server this assistant
/// disabled, or one whose credential does not resolve, never reaches here —
/// the per-assistant tier lands on the registry the model actually sees.
/// Test: `mcp_service_tool_executors_returns_tools_for_enabled_services`,
/// `disabled_services_are_skipped`.
fn build_executors_from_servers(servers: &[&McpServerConfig]) -> Vec<Arc<dyn ToolExecutor>> {
    let mut executors: Vec<Arc<dyn ToolExecutor>> = Vec::new();
    // Used to dedupe across servers in case two advertise the same tool name —
    // first declaration wins.
    let mut seen: HashMap<String, String> = HashMap::new();

    for svc in servers {
        if !svc.enabled {
            continue;
        }
        if extensions::discover(svc) {
            tracing::debug!(
                service = %svc.name,
                "MCP service has discover=true; skipping static tool list (crate::tools::mcp_live handles this service via live tools/list discovery)"
            );
            continue;
        }
        let tools = extensions::tools(svc);
        if tools.is_empty() {
            tracing::debug!(
                service = %svc.name,
                "MCP service has no static tools listed in config; skipping (live tools/list discovery would require spawning the server at registry build time)"
            );
            continue;
        }
        // Stdio-only for now: remote MCP transports aren't wired into
        // StdioMcpClient. Skip rather than fail.
        let Some(shared_client) = ServiceClient::from_server(svc).map(Arc::new) else {
            tracing::debug!(
                service = %svc.name,
                transport = extensions::transport_label(svc),
                "skipping non-stdio MCP service (only stdio transport supported)"
            );
            continue;
        };
        if shared_client.command.is_empty() {
            tracing::debug!(
                service = %svc.name,
                "skipping MCP service with empty command"
            );
            continue;
        }
        for tool in &tools {
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
            let exec: Arc<dyn ToolExecutor> = Arc::new(StaticMcpTool {
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
    use crate::mcp::extensions::ToolDescriptor;
    use trusty_mcp::config::McpTransport;

    /// One stdio server with a static tool list, the shape #7454 replaced
    /// `McpService` with.
    fn svc(name: &str, enabled: bool, tools: &[&str]) -> McpServerConfig {
        let mut server = McpServerConfig::new(
            name,
            McpTransport::Stdio {
                command: format!("{name}-bin"),
                args: vec!["mcp".to_string()],
                env: Default::default(),
            },
        );
        server.enabled = enabled;
        extensions::set(
            &mut server.extensions,
            extensions::DESCRIPTION,
            &format!("{name} service"),
        );
        let declared: Vec<ToolDescriptor> = tools
            .iter()
            .map(|t| ToolDescriptor {
                name: (*t).to_string(),
                description: format!("{t} description"),
            })
            .collect();
        extensions::set(&mut server.extensions, extensions::TOOLS, &declared);
        server
    }

    /// `build_executors_from_servers` takes the borrowed form
    /// `ResolvedMcp::usable()` returns; the tests own their servers.
    fn build(servers: &[McpServerConfig]) -> Vec<Arc<dyn ToolExecutor>> {
        let refs: Vec<&McpServerConfig> = servers.iter().collect();
        build_executors_from_servers(&refs)
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
        let execs = build(&services);
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
        assert!(build(&services).is_empty());
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
        assert!(build(&services).is_empty());
    }

    /// Why: Non-stdio transports aren't wired into StdioMcpClient yet;
    /// silently skipping is the documented graceful-degradation behaviour.
    /// What: HTTP-transport service is filtered out.
    /// Test: This test.
    #[test]
    fn non_stdio_services_are_skipped() {
        let mut s = svc("http-svc", true, &["http_op"]);
        s.transport = McpTransport::Http {
            url: "https://example.com".to_string(),
            headers: Default::default(),
        };
        assert!(build(&[s]).is_empty());
    }

    /// Why: #3238 — a service with `discover = true` opts entirely into the
    /// live-discovery path (`crate::tools::mcp_live`); the static path here
    /// must not ALSO register its `tools` list, or the same tool name would
    /// be double-registered (triggering the debug-assert duplicate panic in
    /// `ToolRegistry::register`).
    /// What: Service has a non-empty static `tools` list AND `discover =
    /// true`; assert the static builder contributes zero executors for it.
    /// Test: This test.
    #[test]
    fn discover_services_skip_static_tool_list() {
        let mut s = svc("discover-svc", true, &["static_tool"]);
        extensions::set(&mut s.extensions, extensions::DISCOVER, &true);
        assert!(build(&[s]).is_empty());
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
        let execs = build(&services);
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
        let mut server = McpServerConfig::new(
            "nonexistent",
            McpTransport::Stdio {
                command: "/nonexistent/mcp/binary/xyzzy-tool-test".to_string(),
                args: vec![],
                env: Default::default(),
            },
        );
        extensions::set(
            &mut server.extensions,
            extensions::TOOLS,
            &vec![ToolDescriptor {
                name: "ghost_tool".to_string(),
                description: "won't run".to_string(),
            }],
        );
        let execs = build(&[server]);
        assert_eq!(execs.len(), 1);
        let result = execs[0].execute(json!({})).await;
        assert!(result.is_error(), "should error when binary missing");
        // Recoverable error — loop should continue.
        assert!(!result.is_fatal());
    }
}
