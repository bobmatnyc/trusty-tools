//! AgentRunner DI seam — portable types for task dispatch and agent output.
//!
//! Why: The `AgentRunner` trait (plus `RunContext`, `AgentOutput`, and
//! `HistoryMessage`) is the SOA boundary between the workflow engine/PM loop
//! and concrete agent implementations (subprocess, in-process, mock). Keeping
//! these types in `trusty-agents` made it impossible for external crates (e.g.
//! orchestration harnesses that don't depend on the full `trusty-agents`
//! binary crate) to implement or mock the runner without a full dependency.
//! Extracting to `trusty-agents-common` breaks that coupling: any crate that
//! only needs to implement or test against `AgentRunner` can depend on this
//! tiny crate.
//! What: Defines `HistoryMessage` (portable IPC wire type), `RunContext`
//! (per-invocation directives), `AgentOutput` (agent result envelope), and
//! the `AgentRunner` async trait.
//! Note: `HistoryMessage::into_typed()` (which converts to async-openai's
//! `ChatCompletionRequestMessage`) is NOT here — it requires `async-openai`
//! and lives in `trusty-agents::session`. Only the portable
//! `{role, content}` struct + its `user`/`assistant` constructors are here.
//! Test: Compile-tested via `trusty-agents` which re-exports these types and
//! has tests that construct `AgentOutput`, `RunContext`, and `HistoryMessage`.
//! The `test_run_with_history_forwards_ctx` test in
//! `trusty-agents::tools::traits` exercises the default `run_with_history`
//! implementation.

use std::path::PathBuf;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::perf::TokenUsage;

/// Simple `{role, content}` serializable form used over IPC.
///
/// Why: `ChatCompletionRequestMessage` is an async-openai enum with complex
/// shape that doesn't round-trip cleanly through NDJSON IPC. A tiny
/// `HistoryMessage` is trivially serde-friendly and enough to rebuild the
/// typed messages on the sub-agent side.
/// What: Pair of `role` ("user"|"assistant"|"system") and `content` strings.
/// Note: The `into_typed()` method (which converts back to
/// `ChatCompletionRequestMessage`) requires `async-openai` and therefore
/// lives in `trusty-agents::session` as a standalone helper function rather
/// than here. Call sites in `trusty-agents` use
/// `trusty_agents::session::history_message_into_typed(msg)`.
/// Test: See `trusty-agents::session` tests + IPC round-trip in
/// `trusty-agents::ipc::tests`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HistoryMessage {
    pub role: String,
    pub content: String,
}

impl HistoryMessage {
    /// Construct a `HistoryMessage` with role=`"user"`.
    ///
    /// Why: Callers serialize a dialog turn over IPC without touching the
    /// role string directly, avoiding typos.
    /// What: Plain struct literal with `role="user"`.
    /// Test: Indirectly via `history_message_typed_round_trip` in
    /// `trusty-agents::session::tests`.
    #[allow(dead_code)]
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".to_string(),
            content: content.into(),
        }
    }

    /// Construct a `HistoryMessage` with role=`"assistant"`.
    ///
    /// Why: Symmetric with `user` — keeps IPC construction declarative.
    /// What: Plain struct literal with `role="assistant"`.
    /// Test: Indirectly via `history_message_typed_round_trip` in
    /// `trusty-agents::session::tests`.
    #[allow(dead_code)]
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".to_string(),
            content: content.into(),
        }
    }
}

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
/// `## Summary` section (or a prefix fallback); `usage` is the aggregated
/// LLM token count for the invocation.
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
    pub usage: TokenUsage,
}

impl AgentOutput {
    /// Build from content alone; summary/usage will be defaults.
    ///
    /// Why: Most test doubles and simple callers only care about content;
    /// a convenience constructor avoids boilerplate.
    /// What: Constructs with empty summary and zero usage.
    /// Test: Used across mock runner construction in unit tests.
    #[allow(dead_code)]
    pub fn from_content(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            summary: None,
            usage: TokenUsage::default(),
        }
    }

    /// Return summary if present, else fall back to content.
    ///
    /// Why: Template substitution in the workflow engine prefers the summary
    /// (concise) but falls back to full content when no summary was extracted.
    /// What: Returns `summary.as_deref()` or `&content`.
    /// Test: Indirect — every template that calls `summary_or_content()`.
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
/// Test: `tests/` substitute in-memory implementations; `test_run_with_history_forwards_ctx`
/// in `trusty-agents::tools::traits` is the regression guard for default
/// `run_with_history` forwarding through `run_with_context`.
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
        _history: &[HistoryMessage],
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use anyhow::Result;
    use async_trait::async_trait;

    use super::{AgentOutput, AgentRunner, HistoryMessage, RunContext, TokenUsage};

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
    }

    /// Verifies that the trait default `run_with_history` forwards `ctx` to
    /// `run_with_context` rather than dropping it (bug #122 regression guard).
    ///
    /// Why: Before the fix, the default called `self.run()` which ignores ctx,
    /// meaning persistent-session agents lost working_dir and model.
    /// What: Calls `run_with_history` with specific working_dir and model,
    /// then asserts `run_with_context` received those values.
    /// Test: Run with `cargo test -p trusty-agents-common test_run_with_history_forwards_ctx`.
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

    #[test]
    fn history_message_constructors() {
        let u = HistoryMessage::user("hello");
        assert_eq!(u.role, "user");
        assert_eq!(u.content, "hello");

        let a = HistoryMessage::assistant("world");
        assert_eq!(a.role, "assistant");
        assert_eq!(a.content, "world");
    }

    #[test]
    fn agent_output_from_content() {
        let out = AgentOutput::from_content("test output");
        assert_eq!(out.content, "test output");
        assert!(out.summary.is_none());
        assert_eq!(out.usage, TokenUsage::default());
    }

    #[test]
    fn agent_output_summary_or_content_fallback() {
        let out = AgentOutput::from_content("fallback content");
        assert_eq!(out.summary_or_content(), "fallback content");

        let out_with_summary = AgentOutput {
            content: "full content".into(),
            summary: Some("short summary".into()),
            usage: TokenUsage::default(),
        };
        assert_eq!(out_with_summary.summary_or_content(), "short summary");
    }
}
