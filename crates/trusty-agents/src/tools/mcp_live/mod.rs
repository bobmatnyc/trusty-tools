//! Live runtime MCP-server discovery + tool registration (#3238, part of
//! epic #3052).
//!
//! Why: Before this module, agents could only call MCP tools that were
//! either hand-registered in-process or listed statically under
//! `[[mcp.services.tools]]` in `~/.trusty-agents/config.toml` — a drift-prone
//! pattern where the config had to be kept in sync by hand with whatever the
//! server actually advertised. `.mcp.json` (the standard Claude-Code file
//! declaring stdio MCP servers) was read only for memory-seeding, never for
//! runtime tool registration. This module closes that gap: given a server
//! spec (name, command, args, env), it spawns the server, runs the MCP
//! `initialize` + `tools/list` handshake, and wraps every advertised tool as
//! a `ToolExecutor` with a passthrough schema — so an agent becomes
//! connectable to *any* configured MCP server, not just the ones someone
//! remembered to hand-list.
//!
//! What: `live_mcp_tool_executors(project_dir, existing_names)` is the
//! single construction helper both assembly points call:
//! `ctrl::pm_task::dispatch::persona::run_pm_task_with_persona` (persona
//! chat) and `runtime::subagent_mode` (sub-agent tool-registry
//! construction, called right after the existing sync
//! `tool_registry::build_registry_for_agent` — that function stays
//! synchronous on purpose since ~10 existing `#[test]`s call it directly;
//! adding the async live-discovery step at the call site instead of inside
//! it avoids a disruptive signature change while still giving both assembly
//! points the same discovered tool set). Specs are gathered by
//! `spec::gather_specs` (config `discover = true` services + `.mcp.json`,
//! config wins on name collision); each surviving spec is spawned via a
//! `mcp_service_tools::ServiceClient` (same lazy-spawn-and-cache shape the
//! static path uses — one subprocess per server, reused across calls) and
//! `tools/list` is called eagerly at registry-build time (discovery
//! inherently needs the process up, unlike static-config tools which need no
//! spawn until first call). A server that fails to spawn/handshake/list is
//! warned-and-skipped — startup never aborts for one bad server. Duplicate
//! tool names (against tools the caller already registered, or across two
//! live-discovered servers) are skipped-with-warning, keeping the earlier
//! registration — matching the existing `mcp_service_tools` dedup policy
//! rather than introducing a second (prefixing) convention.
//!
//! Gating: registering a tool here does not grant access to it. Both
//! assembly points still filter the resulting registry through the agent's
//! `[tools].allow` glob (e.g. `"tm_*"` to opt into a discovered server whose
//! tools are named `tm_*`) and RBAC tier before anything reaches the LLM's
//! tool list — this module only makes the tool *exist* in the registry.
//!
//! Test: `spec::tests::*` (gathering/precedence), `executor::tests::*`
//! (schema passthrough, spawn-failure recovery), and the fake-MCP-server
//! integration tests below (discovery, execute round-trip, duplicate
//! policy, spawn-failure skip).

mod executor;
mod spec;

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use executor::{LiveMcpTool, build_tool_schema};
pub use spec::{McpServerSpec, SpecSource, gather_specs};

use crate::mcp::config::GlobalConfig;
use crate::tools::mcp_service_tools::ServiceClient;
use crate::tools::traits::ToolExecutor;

/// Build live-discovered `ToolExecutor`s for every eligible MCP server.
///
/// Why: The single entry point both assembly points call — see module docs.
/// What: Loads `GlobalConfig`, gathers specs (`spec::gather_specs`), then for
/// each spec spawns + `tools/list`s via a fresh `ServiceClient`, wrapping
/// every advertised tool as a `LiveMcpTool`. `existing_names` seeds the
/// dedup set so tools the caller already registered (from any other source)
/// are never shadowed.
/// Test: `discovery_registers_advertised_tools_as_executors` (integration,
/// below).
pub async fn live_mcp_tool_executors(
    project_dir: &Path,
    existing_names: &HashSet<String>,
) -> Vec<Arc<dyn ToolExecutor>> {
    let config = GlobalConfig::load().await;
    let specs = gather_specs(&config, project_dir).await;
    build_executors_from_specs(specs, existing_names).await
}

/// Pure(ish) helper split out so tests can feed synthetic specs without
/// touching `~/.trusty-agents/config.toml` or a real project directory.
async fn build_executors_from_specs(
    specs: Vec<McpServerSpec>,
    existing_names: &HashSet<String>,
) -> Vec<Arc<dyn ToolExecutor>> {
    let mut executors: Vec<Arc<dyn ToolExecutor>> = Vec::new();
    let mut seen: HashSet<String> = existing_names.clone();

    for spec in specs {
        let client = Arc::new(ServiceClient::with_parts(
            spec.name.clone(),
            spec.command.clone(),
            spec.args.clone(),
            spec.env.clone(),
        ));
        let tools = match client.list_tools().await {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(
                    server = %spec.name,
                    source = ?spec.source,
                    error = %e,
                    "mcp_live: discovery failed (spawn/initialize/tools-list); skipping server"
                );
                continue;
            }
        };

        let mut registered_for_server = 0usize;
        for tool in tools {
            if seen.contains(&tool.name) {
                tracing::warn!(
                    tool = %tool.name,
                    server = %spec.name,
                    "mcp_live: duplicate tool name; keeping earlier registration"
                );
                continue;
            }
            seen.insert(tool.name.clone());
            let schema = build_tool_schema(&spec.name, &tool);
            executors.push(Arc::new(LiveMcpTool {
                name: tool.name,
                schema,
                client: client.clone(),
            }) as Arc<dyn ToolExecutor>);
            registered_for_server += 1;
        }
        tracing::info!(
            server = %spec.name,
            count = registered_for_server,
            "mcp_live: registered live-discovered tools"
        );
    }

    executors
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A minimal fake MCP server: a POSIX shell one-liner that matches on
    /// substrings of each JSON-RPC request line and prints a canned
    /// response. Mirrors the same technique
    /// `trusty_common::stdio_mcp_client`'s own tests use (see
    /// `read_line_skips_non_json_prefix_lines`) — no external binary or
    /// process-spawning test harness needed. Ids are hardcoded to the
    /// sequence a single fresh `StdioMcpClient` actually allocates:
    /// `initialize`=1, `tools/list`=2, `tools/call`=3 (the `initialize`d
    /// notification carries no id and gets no response).
    fn fake_mcp_server_script(tool_name: &str, tool_desc: &str, call_result_text: &str) -> String {
        format!(
            r#"while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      printf '{{"jsonrpc":"2.0","id":1,"result":{{"protocolVersion":"2024-11-05","serverInfo":{{"name":"fake","version":"0.1.0"}}}}}}\n'
      ;;
    *'"method":"tools/list"'*)
      printf '{{"jsonrpc":"2.0","id":2,"result":{{"tools":[{{"name":"{tool_name}","description":"{tool_desc}","inputSchema":{{"type":"object","properties":{{}}}}}}]}}}}\n'
      ;;
    *'"method":"tools/call"'*)
      printf '{{"jsonrpc":"2.0","id":3,"result":{{"content":[{{"type":"text","text":"{call_result_text}"}}]}}}}\n'
      ;;
  esac
done"#
        )
    }

    fn fake_spec(
        name: &str,
        tool_name: &str,
        tool_desc: &str,
        call_result_text: &str,
    ) -> McpServerSpec {
        McpServerSpec {
            name: name.to_string(),
            command: "sh".to_string(),
            args: vec![
                "-c".to_string(),
                fake_mcp_server_script(tool_name, tool_desc, call_result_text),
            ],
            env: HashMap::new(),
            source: SpecSource::ConfigService,
        }
    }

    /// Why: The core promise of #3238 — a live server's advertised tools
    /// must show up as real `ToolExecutor`s with the right name/schema,
    /// without any static config listing them.
    /// What: One fake server advertising `fake_echo`; assert exactly one
    /// executor comes back named `fake_echo` with a function-shape schema.
    /// Test: This test.
    #[tokio::test]
    #[cfg(unix)]
    async fn discovery_registers_advertised_tools_as_executors() {
        let specs = vec![fake_spec("fake-server", "fake_echo", "Echoes input", "ok")];
        let execs = build_executors_from_specs(specs, &HashSet::new()).await;
        assert_eq!(execs.len(), 1);
        assert_eq!(execs[0].name(), "fake_echo");
        let schema = execs[0].schema();
        assert_eq!(schema["type"], "function");
        assert_eq!(schema["function"]["name"], "fake_echo");
    }

    /// Why: A discovered tool must actually dispatch `tools/call` through
    /// to the server and surface the text content — not just exist in the
    /// registry.
    /// What: Build the one executor from `discovery_registers_...`, call
    /// `execute`, assert the round-tripped content matches the fake
    /// server's canned `tools/call` response.
    /// Test: This test.
    #[tokio::test]
    #[cfg(unix)]
    async fn execute_round_trips_a_tools_call() {
        let specs = vec![fake_spec(
            "fake-server",
            "fake_echo",
            "Echoes input",
            "hello from fake server",
        )];
        let execs = build_executors_from_specs(specs, &HashSet::new()).await;
        assert_eq!(execs.len(), 1);
        let result = execs[0].execute(serde_json::json!({})).await;
        assert!(!result.is_error(), "unexpected error: {}", result.content());
        assert_eq!(result.content(), "hello from fake server");
    }

    /// Why: One bad server (missing binary) must never take down discovery
    /// for the rest — this is the "warn-and-skip, never abort" contract
    /// #3238 requires.
    /// What: Two specs: one pointing at a nonexistent binary, one a working
    /// fake server. Assert the working server's tool still registers and
    /// the total count is exactly one (the failed server contributes zero,
    /// not an error/panic).
    /// Test: This test.
    #[tokio::test]
    #[cfg(unix)]
    async fn server_spawn_failure_warns_and_skips_that_server_only() {
        let broken = McpServerSpec {
            name: "broken-server".to_string(),
            command: "/nonexistent/mcp/binary/xyzzy-live-mod-test".to_string(),
            args: vec![],
            env: HashMap::new(),
            source: SpecSource::ConfigService,
        };
        let working = fake_spec("working-server", "works_fine", "desc", "ok");
        let execs = build_executors_from_specs(vec![broken, working], &HashSet::new()).await;
        assert_eq!(execs.len(), 1);
        assert_eq!(execs[0].name(), "works_fine");
    }

    /// Why: Two servers advertising the same tool name must not both
    /// register — that would panic the `debug_assert!` in
    /// `ToolRegistry::register` once these executors are handed off. The
    /// documented policy is skip-with-warning, keeping the earlier
    /// registration (matches `mcp_service_tools`'s static-path policy).
    /// What: Two fake servers both advertising `dup_tool`; assert exactly
    /// one executor comes back, from the first server processed.
    /// Test: This test.
    #[tokio::test]
    #[cfg(unix)]
    async fn duplicate_tool_names_across_servers_keep_first() {
        let first = fake_spec("server-a", "dup_tool", "from a", "a-result");
        let second = fake_spec("server-b", "dup_tool", "from b", "b-result");
        let execs = build_executors_from_specs(vec![first, second], &HashSet::new()).await;
        assert_eq!(execs.len(), 1);
        let result = execs[0].execute(serde_json::json!({})).await;
        assert_eq!(result.content(), "a-result");
    }

    /// Why: A tool already registered by another source (static
    /// `mcp_service_tools`, git tools, ticketing, …) must not be shadowed by
    /// a live-discovered tool of the same name — `existing_names` is the
    /// seam that lets callers seed the dedup set from their own registry.
    /// What: Seed `existing_names` with the fake server's tool name; assert
    /// zero executors come back even though the server is healthy and
    /// advertises that tool.
    /// Test: This test.
    #[tokio::test]
    #[cfg(unix)]
    async fn existing_names_prevent_shadowing_already_registered_tools() {
        let specs = vec![fake_spec("fake-server", "already_registered", "d", "r")];
        let mut existing = HashSet::new();
        existing.insert("already_registered".to_string());
        let execs = build_executors_from_specs(specs, &existing).await;
        assert!(execs.is_empty());
    }
}
