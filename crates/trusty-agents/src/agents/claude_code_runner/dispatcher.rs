//! Per-call runner selection based on each agent's TOML `runner` field (#60).
//!
//! Why: The `WorkflowEngine` holds a single `Arc<dyn AgentRunner>` for the
//! whole run, but individual agents can now opt into either the subprocess
//! (API-key) path, the claude-code (OAuth) path, or the in-process path. A
//! thin dispatcher keeps the engine untouched — it still sees one runner —
//! while picking the right underlying implementation per agent.
//! What: `DispatchingAgentRunner` loads the agent config by name and routes
//! `run` / `run_with_history` / `run_with_context` to the matching concrete
//! runner, falling back to the subprocess runner for unknown / unconfigured
//! kinds.
//! Test: `dispatcher_routes_subprocess_by_default`,
//! `dispatcher_errors_when_claude_code_requested_but_unwired`.

use anyhow::Result;
use async_trait::async_trait;

use super::ClaudeCodeAgentRunner;
use crate::agents::AgentConfig;
use crate::tools::traits::{AgentOutput, AgentRunner, RunContext};

/// Dispatching runner that selects a concrete `AgentRunner` per-call based
/// on the agent's TOML `runner` field (#60).
///
/// Why: The `WorkflowEngine` holds a single `Arc<dyn AgentRunner>` for the
/// whole run, but individual agents can now opt into either the subprocess
/// (API-key) path or the claude-code (OAuth) path. A thin dispatcher keeps
/// the engine untouched — it still sees one runner — while picking the
/// right underlying implementation per agent.
/// What: Holds an `Arc<dyn AgentRunner>` fallback (the normal subprocess
/// runner) plus an optional `Arc<ClaudeCodeAgentRunner>`. On each call it
/// loads the agent config by name; `runner = "claude-code"` routes to the
/// Claude runner, everything else falls through to the fallback.
/// Test: `dispatcher_routes_subprocess_by_default`,
/// `dispatcher_routes_claude_code`.
pub struct DispatchingAgentRunner {
    fallback: std::sync::Arc<dyn AgentRunner>,
    claude_code: Option<std::sync::Arc<ClaudeCodeAgentRunner>>,
    /// Optional in-process runner (#198 / Phase C). When `Some`, agents whose
    /// TOML declares `runner = "in-process"` are dispatched here instead of
    /// the subprocess fallback, eliminating the per-call startup overhead.
    in_process: Option<std::sync::Arc<dyn AgentRunner>>,
}

impl DispatchingAgentRunner {
    /// Build a dispatcher. Pass `None` for `claude_code` when no agent in
    /// the workflow opts into it — the dispatcher then behaves as a thin
    /// passthrough to `fallback`.
    pub fn new(
        fallback: std::sync::Arc<dyn AgentRunner>,
        claude_code: Option<std::sync::Arc<ClaudeCodeAgentRunner>>,
    ) -> Self {
        Self {
            fallback,
            claude_code,
            in_process: None,
        }
    }

    /// Builder-style setter for the in-process runner (#198).
    ///
    /// Why: Lets the workflow runner wire in a single shared
    /// `InProcessAgentRunner` (carrying the shared LLM client) without
    /// changing every existing call site that constructs a dispatcher.
    /// What: Stores the `Arc<dyn AgentRunner>`; route based on the agent's
    /// declared `RunnerKind::InProcess` happens in `run` / `run_with_context`
    /// / `run_with_history`. When `None`, in-process agents fall through to
    /// the subprocess fallback (with a warn log).
    /// Test: `dispatcher_routes_in_process`.
    pub fn with_in_process(mut self, in_process: Option<std::sync::Arc<dyn AgentRunner>>) -> Self {
        self.in_process = in_process;
        self
    }
}

#[async_trait]
impl AgentRunner for DispatchingAgentRunner {
    async fn run(&self, agent_name: &str, task: &str) -> Result<AgentOutput> {
        // Resolve the runner kind. If the config can't be loaded, fall back
        // to subprocess semantics — matches the engine's existing "missing
        // TOML shouldn't be fatal" behavior elsewhere. #96: use the async
        // loader so we don't block the runtime worker on the config read.
        let kind = AgentConfig::by_name_async(agent_name)
            .await
            .map(|c| c.agent.runner)
            .unwrap_or_default();
        match kind {
            crate::agents::RunnerKind::ClaudeCode => {
                let Some(cc) = &self.claude_code else {
                    anyhow::bail!(
                        "agent '{agent_name}' requires runner=\"claude-code\" but no ClaudeCodeAgentRunner was constructed"
                    );
                };
                cc.run(agent_name, task).await
            }
            crate::agents::RunnerKind::InProcess => match &self.in_process {
                Some(ip) => ip.run(agent_name, task).await,
                None => {
                    tracing::warn!(
                        agent = %agent_name,
                        "in-process runner not configured; falling back to subprocess"
                    );
                    self.fallback.run(agent_name, task).await
                }
            },
            _ => self.fallback.run(agent_name, task).await,
        }
    }

    async fn run_with_history(
        &self,
        agent_name: &str,
        task: &str,
        history: &[crate::session::HistoryMessage],
        ctx: &RunContext,
    ) -> Result<AgentOutput> {
        // Bug #122: thread ctx through so working_dir / model reach
        // the underlying runner for persistent-session calls.
        let kind = AgentConfig::by_name_async(agent_name)
            .await
            .map(|c| c.agent.runner)
            .unwrap_or_default();
        match kind {
            crate::agents::RunnerKind::ClaudeCode => {
                // Claude CLI has its own session concept we don't bridge yet;
                // history is silently dropped for claude-code agents. A
                // warning makes this visible during runs that mix runners.
                if !history.is_empty() {
                    tracing::warn!(
                        agent = %agent_name,
                        turns = history.len(),
                        "claude-code runner does not thread session history; ignoring"
                    );
                }
                let Some(cc) = &self.claude_code else {
                    anyhow::bail!(
                        "agent '{agent_name}' requires runner=\"claude-code\" but no ClaudeCodeAgentRunner was constructed"
                    );
                };
                cc.run_with_context(agent_name, task, ctx).await
            }
            crate::agents::RunnerKind::InProcess => match &self.in_process {
                Some(ip) => ip.run_with_history(agent_name, task, history, ctx).await,
                None => {
                    tracing::warn!(
                        agent = %agent_name,
                        "in-process runner not configured; falling back to subprocess"
                    );
                    self.fallback
                        .run_with_history(agent_name, task, history, ctx)
                        .await
                }
            },
            _ => {
                self.fallback
                    .run_with_history(agent_name, task, history, ctx)
                    .await
            }
        }
    }

    async fn run_with_context(
        &self,
        agent_name: &str,
        task: &str,
        ctx: &RunContext,
    ) -> Result<AgentOutput> {
        let kind = AgentConfig::by_name_async(agent_name)
            .await
            .map(|c| c.agent.runner)
            .unwrap_or_default();
        match kind {
            crate::agents::RunnerKind::ClaudeCode => {
                let Some(cc) = &self.claude_code else {
                    anyhow::bail!(
                        "agent '{agent_name}' requires runner=\"claude-code\" but no ClaudeCodeAgentRunner was constructed"
                    );
                };
                cc.run_with_context(agent_name, task, ctx).await
            }
            crate::agents::RunnerKind::InProcess => match &self.in_process {
                Some(ip) => ip.run_with_context(agent_name, task, ctx).await,
                None => {
                    tracing::warn!(
                        agent = %agent_name,
                        "in-process runner not configured; falling back to subprocess"
                    );
                    self.fallback.run_with_context(agent_name, task, ctx).await
                }
            },
            _ => self.fallback.run_with_context(agent_name, task, ctx).await,
        }
    }
}
