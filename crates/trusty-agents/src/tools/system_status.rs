//! `system_status` — on-demand trusty-* subsystem health check (epic #3052).
//!
//! Why: the owner asked that the assistant "have (or know where to find)
//! system details, including a subsystems status check." Subsystem probing
//! is I/O (four daemon HTTP round-trips, an MCP-server reachability check),
//! so it is a tool the agent calls ON DEMAND — never injected into every
//! turn's context, which would bloat/slow every request for a question most
//! turns never ask.
//!
//! This tool is also the direct fix for a real incident: an agent running on
//! OpenRouter once told its owner "I'm running through Anthropic's API" —
//! confidently wrong. The rejected fix was injecting the provider name as a
//! system-prompt literal (just another plausible-sounding string that can
//! drift from reality). This tool instead reads the agent's ACTUAL resolved
//! model/runner/credential live, from the same values that literally drive
//! the current dispatch.
//! What: `SystemStatusTool` wraps `crate::system_status::gather` /
//! `gather_with_resolved_endpoint` (the testable core shared with `tagent
//! system status`) and renders the result as the same human-readable text
//! `format::render_text` produces for the CLI, so the LLM and a human
//! operator read an identical summary. Never reports a credential VALUE —
//! only provider names and tiers (see `crate::system_status::credentials`).
//! Two constructors: [`SystemStatusTool::new`] (re-derives model/runner from
//! the on-disk agent TOML — fine for callers with no live session, e.g.
//! ctrl's own registry) and [`SystemStatusTool::with_resolved_endpoint`]
//! (REQUIRED for any call site that may have applied a session-scoped
//! `/model`/`/provider` override — e.g. `run_pm_task_with_persona` — so the
//! report never silently falls back to a stale on-disk value; see
//! `crate::system_status::resolve_self`'s doc comment for the full
//! rationale).
//! Test: `tests::system_status_tool_reports_success_and_names_tool`,
//! `tests::system_status_tool_with_resolved_endpoint_reflects_override`.

use async_trait::async_trait;
use serde_json::Value;

use crate::agents::RunnerKind;
use crate::tools::traits::{ToolExecutor, ToolResult};

/// `system_status` tool executor.
///
/// Why: carries the one piece of context the report needs that isn't
/// otherwise derivable inside `execute` — which agent this tool instance was
/// registered for, and (when known) the exact model/runner already driving
/// this dispatch, so the report can never disagree with reality. See module
/// docs for why there are two constructors.
pub struct SystemStatusTool {
    pub agent_name: String,
    /// `Some((model, runner))` when constructed via
    /// [`Self::with_resolved_endpoint`] — skips the on-disk re-lookup
    /// entirely so a session-scoped `/model`/`/provider` override is never
    /// missed. `None` falls back to [`crate::system_status::gather`]'s
    /// on-disk lookup.
    resolved_endpoint: Option<(String, RunnerKind)>,
}

impl SystemStatusTool {
    /// Construct for a call site with NO live session to consult — the
    /// report re-derives model/runner from `agent_name`'s on-disk TOML.
    pub fn new(agent_name: impl Into<String>) -> Self {
        Self {
            agent_name: agent_name.into(),
            resolved_endpoint: None,
        }
    }

    /// Construct with the model/runner ALREADY resolved by the caller's live
    /// dispatch (post any `/model`/`/provider` override) — the required
    /// constructor for any session-aware call site. See module docs.
    pub fn with_resolved_endpoint(
        agent_name: impl Into<String>,
        model: impl Into<String>,
        runner: RunnerKind,
    ) -> Self {
        Self {
            agent_name: agent_name.into(),
            resolved_endpoint: Some((model.into(), runner)),
        }
    }
}

#[async_trait]
impl ToolExecutor for SystemStatusTool {
    fn name(&self) -> &str {
        "system_status"
    }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "system_status",
                "description": "Report YOUR OWN actual resolved configuration (agent/model/runner/credential provider — never guess or assume which provider you're running on) plus the health of the trusty-* subsystems (trusty-search, trusty-memory, trusty-analyze, trusty-mpm daemons; configured MCP servers; inference-provider credential status by name/tier only, never values; agent + skill registry counts). A down subsystem is reported as down, not an error. Use when asked about system status, what's running, or your own runtime configuration.",
                "parameters": {
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, _args: Value) -> ToolResult {
        let report = match &self.resolved_endpoint {
            Some((model, runner)) => {
                crate::system_status::gather_with_resolved_endpoint(
                    &self.agent_name,
                    model.clone(),
                    *runner,
                )
                .await
            }
            None => crate::system_status::gather(&self.agent_name).await,
        };
        ToolResult::ok(crate::system_status::format::render_text(&report))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the tool must succeed (never `ToolResult::err`) even when every
    /// probed subsystem is down — the whole point of the graceful-degrade
    /// design — and the schema must name the tool consistently for the LLM
    /// and the registry.
    /// Test: itself.
    #[tokio::test]
    async fn system_status_tool_reports_success_and_names_tool() {
        let tool = SystemStatusTool::new("definitely-not-a-real-agent-xyz");
        assert_eq!(tool.name(), "system_status");
        assert_eq!(tool.schema()["function"]["name"], "system_status");

        let out = tool.execute(serde_json::json!({})).await;
        assert!(!out.is_error(), "system_status must never error: {out:?}");
        assert!(out.content().contains("Daemons:"));
        assert!(out.content().contains("Credentials"));
    }

    /// Why: this is the tool-level regression test for the "live after
    /// /switch and /model" requirement — proves a resolved-endpoint
    /// construction reports the exact model/runner it was given, not a
    /// re-derived on-disk value.
    /// Test: itself.
    #[tokio::test]
    async fn system_status_tool_with_resolved_endpoint_reflects_override() {
        let overridden_model = "test-vendor/definitely-not-the-toml-model-xyz";
        let tool = SystemStatusTool::with_resolved_endpoint(
            "assistant",
            overridden_model,
            RunnerKind::Subprocess,
        );
        let out = tool.execute(serde_json::json!({})).await;
        assert!(!out.is_error());
        assert!(
            out.content().contains(overridden_model),
            "expected overridden model {overridden_model} in output: {}",
            out.content()
        );
    }
}
