//! CTO assistant agent crate — `ToolExecutor` adapters over `tc_services::cto_db`.
//!
//! Why: Issue #484 Phase 1 consolidated the CTO DB *service adapter* (schema
//!      emission + dispatch over `trusty-cto-db`) into `tc_services::cto_db`.
//!      Phase 2 (this crate) extracts the trusty-agents-specific Anti-Corruption
//!      Layer — adapting the host-agnostic `CtoDbService` to trusty-agents's
//!      `ToolExecutor` trait — out of `trusty-agents` itself. The CTO assistant's
//!      tools (HR/budget queries) are sensitive: keeping them in a dedicated
//!      crate clarifies ownership and lets us tighten its dependency surface
//!      without touching the host. `trusty-agents` injects this crate's
//!      `AgentPlugin` at startup; the ctrl loop matches it against the
//!      active persona name rather than hard-coding a branch.
//! What: `CtoDbToolExecutor` newtype wraps a `tc_services::cto_db::CtoDbService`
//!       and implements `trusty_agents::agent_api::ToolExecutor` by delegating
//!       `name`/`schema`/`execute` to it, translating `CtoDbOutcome` into
//!       `ToolResult`. `agent_plugin()` returns an `AgentPlugin` bound to the
//!       `cto-assistant` persona name, holding one executor per published
//!       CTO DB tool.
//! SECURITY: This wrapper exposes HR/budget data. Only the `cto-assistant`
//!           persona should ever have these tools registered. The persona
//!           gate is enforced by `trusty-agents`'s ctrl loop when it matches the
//!           plugin's `persona_name` field; this crate does NOT self-restrict.
//! Test: `cto_db_tools_lists_four`, `execute_outcome_maps_to_tool_result`,
//!       `agent_plugin_targets_cto_assistant`.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tc_services::cto_db::{CtoDbOutcome, CtoDbService};

use trusty_agents_common::{AgentPlugin, ToolExecutor, ToolResult};

/// Persona name this crate's tools are scoped to.
///
/// Why: Single source of truth for the persona name string. Used by both
///      `agent_plugin()` and any future tests / wiring code so a rename
///      changes one place.
/// What: `"cto-assistant"` — matches the persona TOML name shipped under
///       `.trusty-agents/agents/cto-assistant.toml`.
/// Test: `agent_plugin_targets_cto_assistant` asserts the plugin uses this.
pub const PERSONA_NAME: &str = "cto-assistant";

/// trusty-agents `ToolExecutor` adapter over a `tc_services::cto_db::CtoDbService`.
///
/// Why: trusty-agents dispatches by tool name through `dyn ToolExecutor`. The
///      shared `CtoDbService` returns a host-agnostic `CtoDbOutcome`; this
///      newtype is the seam that translates it into trusty-agents's `ToolResult`.
/// What: Holds one `CtoDbService`. `execute()` calls `CtoDbService::execute`
///       and maps `CtoDbOutcome::Ok` → `ToolResult::ok`, `CtoDbOutcome::Err`
///       → recoverable `ToolResult::err` so a missing DB or a schema-drift
///       error doesn't tear down the LLM loop.
/// Test: See module-level tests below.
pub struct CtoDbToolExecutor {
    service: CtoDbService,
}

#[async_trait]
impl ToolExecutor for CtoDbToolExecutor {
    fn name(&self) -> &str {
        self.service.name()
    }

    fn schema(&self) -> Value {
        self.service.schema()
    }

    async fn execute(&self, args: Value) -> ToolResult {
        match self.service.execute(args).await {
            CtoDbOutcome::Ok(s) => ToolResult::ok(s),
            CtoDbOutcome::Err(msg) => ToolResult::err(msg),
        }
    }
}

/// Build the full list of CTO DB tool executors (one per query function).
///
/// Why: Centralised constructor that the `agent_plugin()` builder uses;
///      also exposed for callers that want the raw tool list without the
///      plugin wrapper.
/// What: Delegates to `tc_services::cto_db::cto_db_services()` and wraps
///       each `CtoDbService` in a `CtoDbToolExecutor` boxed as
///       `Arc<dyn ToolExecutor>`.
/// Test: `cto_db_tools_lists_four`.
pub fn cto_db_tools() -> Vec<Arc<dyn ToolExecutor>> {
    tc_services::cto_db::cto_db_services()
        .into_iter()
        .map(|service| Arc::new(CtoDbToolExecutor { service }) as Arc<dyn ToolExecutor>)
        .collect()
}

/// Construct the `AgentPlugin` trusty-agents injects at startup.
///
/// Why: Replaces the hard-coded `if persona_name == "cto-assistant" { ... }`
///      branch in `trusty-agents`'s ctrl loop. `main.rs` calls this once and
///      threads the result into the ctrl session; the ctrl loop then
///      registers these tools whenever the cto-assistant persona is active.
/// What: Returns an `AgentPlugin` bound to `PERSONA_NAME` containing every
///       executor from `cto_db_tools()`.
/// Test: `agent_plugin_targets_cto_assistant`.
pub fn agent_plugin() -> AgentPlugin {
    AgentPlugin::new(PERSONA_NAME, cto_db_tools())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The wrapper must surface one executor per published CTO DB tool.
    ///
    /// Why: Verifies the `CtoDbService` → `CtoDbToolExecutor` adaptation
    ///      preserves the four-tool surface after the cto-assistant extraction.
    /// What: Asserts `cto_db_tools()` returns four executors whose names
    ///       match `tc_services::cto_db::CTO_DB_TOOL_NAMES`.
    /// Test: `cargo test -p cto-assistant cto_db_tools_lists_four`.
    #[test]
    fn cto_db_tools_lists_four() {
        let tools = cto_db_tools();
        assert_eq!(tools.len(), 4, "expected one executor per CTO DB tool");
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        for expected in tc_services::cto_db::CTO_DB_TOOL_NAMES {
            assert!(
                names.contains(expected),
                "missing tool {expected} in {names:?}"
            );
        }
    }

    /// A `CtoDbOutcome::Err` from the service must become a recoverable
    /// `ToolResult::Error` — never fatal, never a panic.
    ///
    /// Why: The harness must keep running when `cto.db` is unavailable; the
    ///      adapter is the layer that guarantees the error stays recoverable.
    /// What: Points `CTO_DB_PATH` at a non-existent file, runs a tool, and
    ///       asserts the `ToolResult` is a non-fatal error. Restores the env var.
    /// Test: `cargo test -p cto-assistant execute_outcome_maps_to_tool_result`.
    #[tokio::test]
    async fn execute_outcome_maps_to_tool_result() {
        let prev = std::env::var(trusty_cto_db::ENV_CTO_DB_PATH).ok();
        // SAFETY: single-threaded test scope; env var restored below.
        unsafe {
            std::env::set_var(
                trusty_cto_db::ENV_CTO_DB_PATH,
                "/tmp/definitely-not-a-real-cto-db-path-cto-assistant.sqlite",
            );
        }

        let tools = cto_db_tools();
        let tool = tools
            .iter()
            .find(|t| t.name() == "query_headcount")
            .expect("query_headcount executor present");
        let result = tool.execute(json!({})).await;
        assert!(result.is_error(), "missing DB must yield an error result");
        assert!(!result.is_fatal(), "DB errors must be recoverable");

        // SAFETY: same single-threaded test scope.
        unsafe {
            match prev {
                Some(v) => std::env::set_var(trusty_cto_db::ENV_CTO_DB_PATH, v),
                None => std::env::remove_var(trusty_cto_db::ENV_CTO_DB_PATH),
            }
        }
    }

    /// The plugin returned by `agent_plugin()` must target the cto-assistant
    /// persona and expose the full CTO DB tool surface.
    ///
    /// Why: Guards against drift between `PERSONA_NAME` and the persona TOML
    ///      name — a mismatch would silently disable the tools.
    /// What: Asserts `persona_name == PERSONA_NAME` and tool count is four.
    /// Test: `cargo test -p cto-assistant agent_plugin_targets_cto_assistant`.
    #[test]
    fn agent_plugin_targets_cto_assistant() {
        let plugin = agent_plugin();
        assert_eq!(plugin.persona_name, PERSONA_NAME);
        assert_eq!(plugin.tools.len(), 4);
    }
}
