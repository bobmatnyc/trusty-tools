//! Core trait abstractions for tool execution, agent running, search, and skills.
//!
//! Why: DI through traits lets tcode swap concrete implementations (subprocess
//! runner vs. in-process mock; Brave vs. Tavily search; fs skill store vs.
//! embedded) without changing call sites. This is the SOA seam that keeps
//! the workflow engine and PM loop testable.
//! What: Defines `ToolExecutor`, `AgentRunner`, `SearchProvider`, and
//! `SkillResolver`. All are object-safe (`dyn`-able) and `Send + Sync`.
//! Test: Mock impls of each trait are constructed in unit tests for
//! `ToolRegistry`, delegating commands, and related code.

use std::path::PathBuf;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

// ── ToolResult ──────────────────────────────────────────────────────────────

/// Structured result of a tool execution.
///
/// Why: Hard-failing the LLM loop on every tool error is brittle — the model
/// often can recover (retry with different args, fall back to another tool, or
/// explain the failure in its final answer). A structured error lets the caller
/// surface failure back to the LLM as a `tool_result` with `is_error: true`
/// while keeping the loop running.
/// What: `Success(String)` carries a successful textual result; `Error` carries
/// a message plus a `recoverable` flag.
/// Test: `ToolResult::err(...).is_error()` is true; `ok(...).content()` returns
/// the success string. Exercised in `tools::registry` and delegate tests.
#[derive(Debug)]
pub enum ToolResult {
    /// Tool executed successfully; inner string is the textual result.
    Success(String),
    /// Tool failed; `message` describes why; `recoverable` advises the LLM loop.
    Error { message: String, recoverable: bool },
}

impl ToolResult {
    /// Construct a successful result.
    ///
    /// Why: Single canonical happy-path constructor used by every tool.
    /// What: Wraps `s` in `Success`.
    /// Test: Trivially exercised by every successful tool execute().
    pub fn ok(s: impl Into<String>) -> Self {
        ToolResult::Success(s.into())
    }

    /// Construct a recoverable error.
    ///
    /// Why: Most tool failures are non-fatal — wrong arg, transient network,
    /// empty result. The LLM should see the error and decide.
    /// What: Wraps `msg` with `recoverable = true`.
    /// Test: Exercised by delegate and registry tests.
    pub fn err(msg: impl Into<String>) -> Self {
        ToolResult::Error {
            message: msg.into(),
            recoverable: true,
        }
    }

    /// Construct a fatal (non-recoverable) error.
    ///
    /// Why: Some failures (invariant violations, credential rejection) should
    /// not be retried; callers should bail.
    /// What: Wraps `msg` with `recoverable = false`.
    /// Test: Checked by `is_fatal` predicates in tests.
    pub fn fatal(msg: impl Into<String>) -> Self {
        ToolResult::Error {
            message: msg.into(),
            recoverable: false,
        }
    }

    /// Whether this result is an error variant.
    ///
    /// Why: Dispatch paths need a cheap predicate to log/branch on failure.
    /// What: Returns `true` for any `Error`, `false` for `Success`.
    /// Test: `ToolResult::err("x").is_error()` is true.
    pub fn is_error(&self) -> bool {
        matches!(self, ToolResult::Error { .. })
    }

    /// Whether this error is fatal (not recoverable). `false` for Success.
    ///
    /// Why: Callers that distinguish fatal-vs-recoverable need this to decide
    /// whether to retry or bail.
    /// What: True only for `Error { recoverable: false, .. }`.
    /// Test: `ToolResult::fatal("x").is_fatal()` is true.
    pub fn is_fatal(&self) -> bool {
        matches!(
            self,
            ToolResult::Error {
                recoverable: false,
                ..
            }
        )
    }

    /// Extract the message string for both variants.
    ///
    /// Why: Callers like the LLM loop need the raw string regardless of
    /// success/failure to pass back as `tool_result` content.
    /// What: Returns the payload string for both `Success` and `Error`.
    /// Test: `ToolResult::ok("hi").content()` == "hi";
    ///       `ToolResult::err("oops").content()` == "oops".
    pub fn content(&self) -> &str {
        match self {
            ToolResult::Success(s) => s.as_str(),
            ToolResult::Error { message, .. } => message.as_str(),
        }
    }
}

// ── ToolExecutor ─────────────────────────────────────────────────────────────

/// Access tiers controlling which users/transports may invoke a tool.
///
/// Why: tcode exposes tools over multiple surfaces (CLI, API, TUI). Some tools
/// (memory writes, shell exec) are unsafe for untrusted callers. `ServiceTier`
/// provides a stable ladder that tools can declare their restrictions against.
/// What: Ordered by trust level — `All` is the highest-privilege tier.
/// Test: `rbac::tests::*` cover tier matching logic.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceTier {
    /// Trusted operator — full tool access.
    #[default]
    All,
    /// Analytics/reporting callers — read-heavy, no mutations.
    Analytics,
    /// Read-only callers — can only observe state.
    ReadOnly,
}

/// Object-safe executor interface for a single named tool.
///
/// Why: Abstracts over all tool kinds (fs, shell, web search, delegate, MCP
/// bridge) so the registry and dispatch loop are tool-agnostic. New tools drop
/// in by implementing this trait without touching the PM loop.
/// What: `name()` is the string key used in LLM function calls; `schema()`
/// emits the JSON function descriptor; `execute(args)` performs the action.
/// Test: `ToolRegistry` tests register a `MockTool` implementing this trait.
#[async_trait]
pub trait ToolExecutor: Send + Sync {
    /// The tool's function name as exposed to the LLM.
    fn name(&self) -> &str;

    /// JSON schema describing the tool's parameters (OpenAI function format).
    fn schema(&self) -> Value;

    /// Execute the tool with the given JSON argument object.
    async fn execute(&self, args: Value) -> ToolResult;

    /// Access tiers that are **blocked** from calling this tool.
    ///
    /// Why: Tools declare which tiers they restrict; an empty list means
    /// universally accessible. This inverted model means adding RBAC to
    /// existing tools is a no-op by default.
    /// What: Returns a slice of `ServiceTier` values that are NOT allowed.
    /// Test: `rbac::tests::can_access_tier_*`.
    fn restricted_tiers(&self) -> &[ServiceTier] {
        &[]
    }
}

// ── RunContext ───────────────────────────────────────────────────────────────

/// Per-invocation context threaded from orchestrator to agent runner.
///
/// Why: Passing per-call directives (assigned file, turn budget, working dir)
/// as a `RunContext` instead of via `std::env::set_var` is sound under
/// multi-threaded tokio — env mutation is not thread-safe in Rust 2024.
/// What: Carries optional overrides; runners apply them to child processes via
/// `Command::env` / `Command::current_dir`.
/// Test: Exercised in `AgentRunner` trait tests (`test_run_with_history_forwards_ctx`).
#[derive(Debug, Default, Clone)]
pub struct RunContext {
    /// Path to scope any file-writing tool to for this call.
    pub assigned_file: Option<PathBuf>,
    /// Max-turns cap override for this invocation.
    pub max_turns_override: Option<u32>,
    /// Working directory for the subprocess.
    pub working_dir: Option<PathBuf>,
    /// LLM model override for this invocation.
    pub model: Option<String>,
}

// ── AgentOutput ──────────────────────────────────────────────────────────────

/// Structured output returned by an `AgentRunner`.
///
/// Why: Downstream phases need a concise `summary` for template substitution
/// while also needing the full `content` for file extraction. Bundling them
/// avoids separate trait methods.
/// What: `content` is raw agent output; `summary` is the extracted `## Summary`
/// section (or a prefix fallback).
/// Test: Constructed from IPC result messages in integration paths.
#[derive(Debug, Clone)]
pub struct AgentOutput {
    /// Raw output from the agent.
    pub content: String,
    /// Extracted summary (e.g. `## Summary` section), if present.
    pub summary: Option<String>,
    /// Aggregated LLM token usage for this invocation.
    pub usage: crate::perf::TokenUsage,
}

impl AgentOutput {
    /// Build from content alone; summary/usage will be defaults.
    ///
    /// Why: Convenience constructor for simple agent results.
    /// What: Sets `content`, leaves `summary` None, `usage` zeroed.
    /// Test: Used in mock runners throughout this crate's tests.
    pub fn from_content(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            summary: None,
            usage: crate::perf::TokenUsage::default(),
        }
    }

    /// Return summary if present, else fall back to content.
    ///
    /// Why: Callers that want a short form for template injection should not
    /// have to branch on `Option`.
    /// What: Returns `&summary` if `Some`, otherwise `&content`.
    /// Test: `agent_output_summary_or_content_fallback`.
    pub fn summary_or_content(&self) -> &str {
        self.summary.as_deref().unwrap_or(&self.content)
    }
}

// ── AgentRunner ──────────────────────────────────────────────────────────────

/// Runs a task against a named agent.
///
/// Why: Abstracts over how an agent is executed — subprocess, in-process, or
/// mock. Lets tests avoid spawning real processes.
/// What: `run(agent_name, task)` returns the agent's `AgentOutput`.
/// `run_with_context` extends this to pass per-invocation overrides; the
/// default forwards to `run()` so existing mock runners keep working.
/// Test: `MockAgentRunner` in tests implements this; `test_run_with_history_forwards_ctx`
/// verifies the default delegation chain.
#[async_trait]
pub trait AgentRunner: Send + Sync {
    /// Run a task against the named agent.
    async fn run(&self, agent_name: &str, task: &str) -> Result<AgentOutput>;

    /// Run with per-invocation overrides in `ctx`.
    ///
    /// Why: Replaces `std::env::set_var` threading of per-call state. Default
    /// impl ignores `ctx` and falls back to `run()` so legacy mock runners
    /// keep working unchanged.
    /// What: Concrete runners override this to scope env vars to the child.
    /// Test: Wave-loop and runner tests prove context fields reach the child.
    async fn run_with_context(
        &self,
        agent_name: &str,
        task: &str,
        _ctx: &RunContext,
    ) -> Result<AgentOutput> {
        self.run(agent_name, task).await
    }

    /// Run a task while forwarding prior session history.
    ///
    /// Why: Persistent-session agents need prior turns so the sub-agent can
    /// rebuild context in a fresh subprocess.
    /// What: Default delegates to `run_with_context(ctx)` so `working_dir`
    /// and `model` are honoured even when the runner does not override this.
    /// Test: `test_run_with_history_forwards_ctx`.
    async fn run_with_history(
        &self,
        agent_name: &str,
        task: &str,
        _history: &[HistoryMessage],
        ctx: &RunContext,
    ) -> Result<AgentOutput> {
        self.run_with_context(agent_name, task, ctx).await
    }
}

// ── HistoryMessage ───────────────────────────────────────────────────────────

/// A prior conversation turn forwarded to persistent-session agents.
///
/// Why: Persistent-session runners replay history on each subprocess restart
/// so the sub-agent preserves conversational context.
/// What: `role` is `"user"` or `"assistant"`; `content` is the turn text.
/// Test: Used in `AgentRunner::run_with_history` signature.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryMessage {
    pub role: String,
    pub content: String,
}

// ── SearchProvider ───────────────────────────────────────────────────────────

/// A search hit returned by a `SearchProvider`.
///
/// Why: Uniform result type for web/code search so callers are provider-agnostic.
/// What: `title`, `url`, `snippet` — the minimal useful set.
/// Test: Used in `SearchProvider` mock tests.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// Web/code search backend abstraction.
///
/// Why: Swap Brave for Tavily / DuckDuckGo / a test stub by changing the
/// registered impl — no callers change.
/// What: `search(query, n)` returns up to `n` results.
/// Test: A stub `SearchProvider` returns a fixed vector in mock tests.
#[async_trait]
pub trait SearchProvider: Send + Sync {
    /// Run a search for `query` and return up to `n` results.
    async fn search(&self, query: &str, n: usize) -> Result<Vec<SearchResult>>;
}

// ── SkillResolver ────────────────────────────────────────────────────────────

/// Resolves named skills to their Markdown content.
///
/// Why: Skill sources vary (project `.claude/`, user `~/.claude/`, bundled
/// config); callers should not care which.
/// What: `resolve(name)` returns `Some(content)` if found. `list()` returns
/// all discoverable names.
/// Test: A `FsSkillResolver` in tests places a file in a tempdir and asserts
/// `resolve()` returns its contents.
pub trait SkillResolver: Send + Sync {
    /// Look up a skill by name and return its Markdown content, if found.
    fn resolve(&self, name: &str) -> Option<String>;
    /// List all discoverable skill names.
    fn list(&self) -> Vec<String>;
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use anyhow::Result;
    use async_trait::async_trait;

    use super::*;
    use crate::perf::TokenUsage;

    /// Mock runner that records the `RunContext` received in `run_with_context`.
    ///
    /// Why: We need to assert that the default `run_with_history` impl actually
    /// forwards `ctx` to `run_with_context` (regression guard).
    /// What: Records each ctx snapshot; `run` is unused in this test.
    /// Test: `test_run_with_history_forwards_ctx`.
    struct CtxCapture {
        captured: Arc<Mutex<Vec<RunContext>>>,
    }

    #[async_trait]
    impl AgentRunner for CtxCapture {
        async fn run(&self, _agent_name: &str, _task: &str) -> Result<AgentOutput> {
            Ok(AgentOutput::from_content("run"))
        }

        async fn run_with_context(
            &self,
            _agent_name: &str,
            _task: &str,
            ctx: &RunContext,
        ) -> Result<AgentOutput> {
            self.captured
                .lock()
                .expect("lock poisoned")
                .push(ctx.clone());
            Ok(AgentOutput::from_content("run_with_context"))
        }
        // NOTE: run_with_history is intentionally NOT overridden — we test the
        // default impl defined in the trait.
    }

    /// Verifies that the default `run_with_history` forwards `ctx` to
    /// `run_with_context` rather than dropping it.
    ///
    /// Why: Without this, persistent-session agents lose `working_dir` and
    /// `model` when the default implementation falls through.
    /// What: Calls `run_with_history` with specific `working_dir` and `model`,
    /// then asserts `run_with_context` received those values.
    /// Test: This test itself.
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

        let snaps = captured.lock().expect("lock poisoned");
        assert_eq!(
            snaps.len(),
            1,
            "run_with_context must be called exactly once"
        );

        let recorded = &snaps[0];
        assert_eq!(
            recorded.working_dir,
            Some(PathBuf::from("/tmp/test-wd")),
            "working_dir must be forwarded"
        );
        assert_eq!(
            recorded.model,
            Some("anthropic/claude-opus-4-5".to_string()),
            "model must be forwarded"
        );
        assert_eq!(
            recorded.max_turns_override,
            Some(5),
            "max_turns_override must be forwarded"
        );
    }

    /// Verify `ToolResult` predicates and content accessor.
    ///
    /// Why: Guard the semantics of `is_error`, `is_fatal`, and `content`.
    /// What: Constructs each variant and checks all three predicates.
    /// Test: This test itself.
    #[test]
    fn tool_result_predicates() {
        let ok = ToolResult::ok("hello");
        assert!(!ok.is_error());
        assert!(!ok.is_fatal());
        assert_eq!(ok.content(), "hello");

        let err = ToolResult::err("oops");
        assert!(err.is_error());
        assert!(!err.is_fatal());
        assert_eq!(err.content(), "oops");

        let fatal = ToolResult::fatal("crash");
        assert!(fatal.is_error());
        assert!(fatal.is_fatal());
        assert_eq!(fatal.content(), "crash");
    }

    /// Verify `AgentOutput::summary_or_content` fallback.
    ///
    /// Why: Callers rely on this to get a short form without branching.
    /// What: `summary=Some` returns summary; `summary=None` returns content.
    /// Test: This test itself.
    #[test]
    fn agent_output_summary_or_content_fallback() {
        let with_summary = AgentOutput {
            content: "full content".into(),
            summary: Some("short summary".into()),
            usage: TokenUsage::default(),
        };
        assert_eq!(with_summary.summary_or_content(), "short summary");

        let no_summary = AgentOutput::from_content("just content");
        assert_eq!(no_summary.summary_or_content(), "just content");
    }
}
