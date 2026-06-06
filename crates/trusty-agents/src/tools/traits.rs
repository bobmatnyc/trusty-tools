//! Core trait abstractions for tool execution, agent running, search, and skills.
//!
//! Why: DI through traits lets us swap concrete implementations (subprocess
//! runner vs. in-process mock; Brave vs. Tavily search; fs skill store vs.
//! embedded) without changing call sites. This is the SOA seam that keeps
//! the workflow engine and PM loop testable.
//! What: Defines `ToolExecutor`, `AgentRunner`, `SearchProvider`, and
//! `SkillResolver`. All are object-safe (`dyn`-able) and `Send + Sync`.
//! Test: Mock impls of each trait are constructed in unit tests for
//! `ToolRegistry`, `WorkflowEngine`, and related code.

use std::path::PathBuf;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

// Why: `ToolExecutor`, `ToolResult`, and `ToolExecutionTier` were extracted
//      to `trusty-agents-common` so external agent crates (e.g. `cto-assistant`)
//      can implement them without a cargo cycle through `trusty-agents`. Re-exports
//      here preserve every existing `crate::tools::traits::ToolExecutor`
//      import in the workspace — internal call sites are unchanged.
// What: Re-exports the trait + result + tier from the shared crate.
// Test: All existing tool tests still resolve these names and pass.
pub use trusty_agents_common::{ToolExecutionTier, ToolExecutor, ToolResult};

/// Per-invocation context threaded from orchestrator to agent runner.
///
/// Why: Previously, orchestrators (the wave loop in particular) passed
/// per-call directives like the assigned file path and a tightened turn
/// budget via process-global env vars (`std::env::set_var`). That is
/// unsound in Rust 2024 because env mutation is not thread-safe under
/// multi-threaded tokio runtimes. Threading a `RunContext` through the
/// `AgentRunner` trait replaces the shared mutation with a per-call value
/// that runners can apply to the child process (via `Command::env` / `current_dir`)
/// without touching the parent's environment.
/// What: Carries optional overrides — the assigned file for single-file
/// wave-loop invocations, a per-call max-turns cap, and a working directory
/// that the runner should use as the subprocess CWD.
/// Test: Exercised indirectly through the wave-loop and runner dispatch
/// tests (`wave_loop_runs_one_agent_per_file`, runner unit tests).
#[derive(Debug, Default, Clone)]
pub struct RunContext {
    /// When `Some(path)`, the runner should scope any file-writing tool to
    /// exactly this relative path. The wave loop sets this so each per-file
    /// agent can only emit its assigned file.
    pub assigned_file: Option<PathBuf>,
    /// When `Some(n)`, the runner should enforce a max-turns cap of `n` for
    /// this single invocation, overriding whatever the agent's TOML sets.
    pub max_turns_override: Option<u32>,
    /// When `Some(dir)`, the runner should use `dir` as the subprocess's
    /// current working directory. Needed for the claude-code runner so the
    /// CLI's file-writing tools resolve relative paths inside `out_dir`.
    pub working_dir: Option<PathBuf>,
    /// When `Some(model)`, the runner should use `model` as the LLM model for
    /// this invocation, overriding the agent TOML's `[agent].model` /
    /// `[llm].model_override`. Sourced from the workflow phase's
    /// `model` field (#107, renamed from `model_override` in #359).
    ///
    /// Why: Previously the workflow engine only logged `phase.model`
    /// as "advisory" — it never reached the runner. For `ClaudeCodeAgentRunner`
    /// this meant every phase used the plan-agent TOML's model
    /// (`claude-sonnet-4-6`) regardless of what `config/workflows/*.json`
    /// specified. Threading the override through `RunContext` lets the runner
    /// pass it as `--model` to the `claude` CLI (and via
    /// `TAGENT_MODEL_<AGENT>` env for subprocess runners), preserving the
    /// documented priority chain (env > phase override > agent TOML > default).
    /// What: Optional string (e.g. `"anthropic/claude-opus-4-6"`). Runners that
    /// don't understand it are free to ignore it; the default
    /// `AgentRunner::run_with_context` still falls back to `run()` for mock
    /// test doubles.
    /// Test: `ClaudeCodeAgentRunner::run_with_config_ctx` honors it; the
    /// engine populates it from `phase.model` in both the non-wave
    /// and wave-loop paths.
    pub model: Option<String>,
}

/// Structured output returned by an `AgentRunner`.
///
/// Why: Downstream workflow phases need a concise `summary` (~500 words) for
/// template substitution while file-extraction still needs the full `content`.
/// Bundling them in one struct lets callers choose which to consume without
/// needing separate trait methods.
/// What: `content` is the raw agent output; `summary` is the extracted
/// `## Summary` section (or a prefix fallback).
/// Test: See workflow engine tests; constructed from `IpcMessage::Result`.
#[derive(Debug, Clone)]
pub struct AgentOutput {
    pub content: String,
    pub summary: Option<String>,
    /// Aggregated LLM token usage for this agent invocation (#47).
    ///
    /// Why: `WorkflowEngine` needs per-phase token/cost data for the perf
    /// record. Bubbling it from the sub-agent through the runner trait keeps
    /// the instrumentation pipeline single-sourced.
    /// What: `TokenUsage::default()` (all zeros) when no usage was reported
    /// by the sub-agent (e.g. older binary or pre-LLM tool-only error).
    /// Test: `perf::tests::collector_records_phases` exercises the sink;
    /// end-to-end wiring is covered by workflow integration.
    pub usage: crate::perf::TokenUsage,
}

impl AgentOutput {
    /// Build from content alone; summary/usage will be defaults.
    #[allow(dead_code)]
    pub fn from_content(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            summary: None,
            usage: crate::perf::TokenUsage::default(),
        }
    }

    /// Return summary if present, else fall back to content.
    #[allow(dead_code)]
    pub fn summary_or_content(&self) -> &str {
        self.summary.as_deref().unwrap_or(&self.content)
    }
}

/// Runs a task against a named agent.
///
/// Why: Abstracts over how an agent is executed — subprocess, in-process, or
/// mock. Lets tests avoid spawning real processes.
/// What: `run(agent_name, task)` returns the agent's `AgentOutput` (content
/// plus optional summary). `run_with_history` extends this to pass prior
/// conversation turns for persistent-session agents (#51); the default
/// implementation ignores history so legacy impls keep working unchanged.
/// Test: `tests/` substitute in-memory implementations.
#[async_trait]
pub trait AgentRunner: Send + Sync {
    async fn run(&self, agent_name: &str, task: &str) -> Result<AgentOutput>;

    /// Run a task while forwarding any prior session history.
    ///
    /// Why: Persistent-session agents (#51) need their caller to replay
    /// earlier user/assistant turns so the sub-agent can rebuild context
    /// in a fresh subprocess. Bug #122: the previous default fell through to
    /// `run()` instead of `run_with_context()`, so persistent-session callers
    /// silently lost `working_dir` and `model` carried in `ctx`.
    /// What: Default implementation ignores `history` and delegates to
    /// `run_with_context(ctx)`, so `working_dir` and `model` are
    /// honoured even for runners that do not override `run_with_history`.
    /// Concrete runners that know how to carry history across the process
    /// boundary override this.
    /// Test: `SubprocessAgentRunner` exercises the override; the default
    /// path is covered by `test_run_with_history_forwards_ctx` and the
    /// existing mock-runner tests in `workflow`.
    async fn run_with_history(
        &self,
        agent_name: &str,
        task: &str,
        _history: &[crate::session::HistoryMessage],
        ctx: &RunContext,
    ) -> Result<AgentOutput> {
        // Default: fall through to run_with_context so working_dir + model
        // are honoured even when the concrete runner doesn't override run_with_history.
        self.run_with_context(agent_name, task, ctx).await
    }

    /// Run a task with a `RunContext` carrying per-invocation overrides.
    ///
    /// Why: Replaces unsafe `std::env::set_var` threading of per-call state
    /// (assigned file, tightened turn budget, working dir) with a structured
    /// value that runners can apply to the child process only. Default impl
    /// ignores the context and falls back to `run()` so existing mock runners
    /// (including test doubles) keep working unchanged.
    /// What: Concrete runners override this to scope env vars to the child
    /// (`Command::env`) and optionally set its CWD (`Command::current_dir`).
    /// Test: Wave-loop tests prove the context fields reach the child.
    async fn run_with_context(
        &self,
        agent_name: &str,
        task: &str,
        _ctx: &RunContext,
    ) -> Result<AgentOutput> {
        self.run(agent_name, task).await
    }
}

/// A search hit returned by a `SearchProvider`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// Web search backend abstraction.
///
/// Why: Swap Brave for Tavily / DuckDuckGo / a test stub by changing the
/// registered impl — no callers change.
/// What: `search(query, n)` returns up to `n` results.
/// Test: A stub `SearchProvider` returns a fixed vector in tests.
#[async_trait]
pub trait SearchProvider: Send + Sync {
    async fn search(&self, query: &str, n: usize) -> Result<Vec<SearchResult>>;
}

/// Resolves named skills to their Markdown content.
///
/// Why: Skill sources vary (project `.claude/`, user `~/.claude/`, bundled
/// `config/skills/`); callers should not care which.
/// What: `resolve(name)` returns `Some(content)` if found. `list()` returns
/// all discoverable names.
/// Test: `FsSkillResolver` unit test places a file in a tempdir and asserts
/// `resolve()` returns its contents.
pub trait SkillResolver: Send + Sync {
    fn resolve(&self, name: &str) -> Option<String>;
    fn list(&self) -> Vec<String>;
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use anyhow::Result;
    use async_trait::async_trait;

    use super::{AgentOutput, AgentRunner, RunContext};
    use crate::perf::TokenUsage;

    /// Mock runner that records the RunContext it receives in `run_with_context`.
    ///
    /// Why: We need to assert that the trait default `run_with_history` impl
    /// actually forwards `ctx` to `run_with_context` (bug #122 regression guard).
    /// What: Stores each ctx snapshot; `run` is unused (test calls only
    /// `run_with_history`).
    /// Test: `test_run_with_history_forwards_ctx` constructs one of these,
    /// calls `run_with_history` with a specific `RunContext`, and asserts the
    /// recorded snapshot matches.
    struct CtxCapture {
        /// Every RunContext received by run_with_context, in call order.
        captured: Arc<Mutex<Vec<RunContext>>>,
    }

    #[async_trait]
    impl AgentRunner for CtxCapture {
        async fn run(&self, _agent_name: &str, _task: &str) -> Result<AgentOutput> {
            Ok(AgentOutput {
                content: "run".into(),
                summary: None,
                usage: TokenUsage::default(),
            })
        }

        async fn run_with_context(
            &self,
            _agent_name: &str,
            _task: &str,
            ctx: &RunContext,
        ) -> Result<AgentOutput> {
            self.captured.lock().unwrap().push(ctx.clone());
            Ok(AgentOutput {
                content: "run_with_context".into(),
                summary: None,
                usage: TokenUsage::default(),
            })
        }
        // Note: run_with_history is intentionally NOT overridden — we test the
        // default impl behaviour defined in the trait.
    }

    /// Verifies that the trait default `run_with_history` forwards `ctx` to
    /// `run_with_context` rather than dropping it (bug #122 regression guard).
    ///
    /// Why: Before the fix, the default called `self.run()` which ignores ctx,
    /// meaning persistent-session agents lost working_dir and model.
    /// What: Calls `run_with_history` with specific working_dir and
    /// model, then asserts `run_with_context` received those values.
    /// Test: Run with `cargo test test_run_with_history_forwards_ctx`.
    #[tokio::test]
    async fn test_run_with_history_forwards_ctx() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let runner = CtxCapture {
            captured: captured.clone(),
        };

        let ctx = RunContext {
            working_dir: Some(PathBuf::from("/tmp/test-wd")),
            model: Some("anthropic/claude-opus-4-5".to_string()),
            max_turns_override: Some(5),
            assigned_file: None,
        };

        let out = runner
            .run_with_history("test-agent", "do something", &[], &ctx)
            .await
            .expect("run_with_history should succeed");

        // The default impl must delegate to run_with_context, not run().
        assert_eq!(
            out.content, "run_with_context",
            "default run_with_history must call run_with_context, not run()"
        );

        let snaps = captured.lock().unwrap();
        assert_eq!(
            snaps.len(),
            1,
            "run_with_context should be called exactly once"
        );

        let recorded = &snaps[0];
        assert_eq!(
            recorded.working_dir,
            Some(PathBuf::from("/tmp/test-wd")),
            "working_dir must be forwarded through run_with_history"
        );
        assert_eq!(
            recorded.model,
            Some("anthropic/claude-opus-4-5".to_string()),
            "model must be forwarded through run_with_history"
        );
        assert_eq!(
            recorded.max_turns_override,
            Some(5),
            "max_turns_override must be forwarded through run_with_history"
        );
    }
}
