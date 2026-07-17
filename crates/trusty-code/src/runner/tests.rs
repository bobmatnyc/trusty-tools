//! Unit tests for the in-process agent runner (issue #1029).
//!
//! Why: The runner is the runtime execution layer the PM delegates through;
//! every acceptance criterion — engineer runs its own loop on its own slug,
//! returns an `AgentOutput` to the PM, `tools.allowed` is enforced, and usage
//! rolls up — must be provable offline, without a live LLM. The scripted
//! `LlmClientTrait` mock from the agent-loop tests is the seam that makes this
//! deterministic.
//! What: Defines a `ScriptedLlm` that replays a queue of `ChatResponse`s and
//! records the system prompt + model of every request, a `RecordingTool` whose
//! execution is observable, and exercises the runner end-to-end: a stubbed PM
//! delegates to a `python-engineer` agent whose loop runs against its own slug.
//! Test: this module is itself the test surface.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use async_trait::async_trait;
use serde_json::{Value, json};
use tempfile::TempDir;

use super::{InProcessAgentRunner, InProcessRunnerConfig};
use crate::agent_loop::ToolEventSink;
use crate::agents::AgentConfig;
use crate::llm::{ChatRequest, ChatResponse, LlmClientTrait, LlmError};
use crate::tools::{
    AgentRunner, DelegateToAgentTool, RunContext, ToolExecutor, ToolRegistry, ToolResult,
};

// ── Test doubles ───────────────────────────────────────────────────────────────

/// A `LlmClientTrait` that replays a fixed script and records every request.
///
/// Why: Deterministic, offline substitute for the network client; recording the
/// requests lets tests assert what model and system prompt the runner resolved.
/// What: Holds scripted `ChatResponse`s, an atomic cursor, and a Mutex log of
/// `(model, system_prompt)` pairs captured from each `chat` call. Running past
/// the end yields a transport-style error so a runaway loop fails loudly.
/// Test: Used by every runner test below.
struct ScriptedLlm {
    responses: Vec<ChatResponse>,
    cursor: AtomicUsize,
    seen: Mutex<Vec<(String, String)>>,
    /// Every request's `max_tokens`, in call order — lets
    /// `engineer_llm_max_tokens_reaches_chat_request` assert the delegated
    /// engineer's `[llm].max_tokens` reached the wire request rather than
    /// being silently dropped in favour of the agent-loop default.
    max_tokens_seen: Mutex<Vec<Option<u32>>>,
}

impl ScriptedLlm {
    /// Build a scripted client from JSON response fixtures.
    ///
    /// Why: `ChatResponse` is `Deserialize`-only; tests author responses as JSON.
    /// What: Deserialises each fixture and seeds an empty request log.
    /// Test: Used by every runner test below.
    fn from_json(fixtures: &[Value]) -> Self {
        let responses = fixtures
            .iter()
            .map(|v| serde_json::from_value(v.clone()).expect("valid ChatResponse fixture"))
            .collect();
        Self {
            responses,
            cursor: AtomicUsize::new(0),
            seen: Mutex::new(Vec::new()),
            max_tokens_seen: Mutex::new(Vec::new()),
        }
    }

    /// Number of `chat` calls made so far.
    fn calls(&self) -> usize {
        self.cursor.load(Ordering::SeqCst)
    }

    /// The `(model, system_prompt)` of the first recorded request.
    fn first_request(&self) -> (String, String) {
        self.seen
            .lock()
            .expect("seen lock")
            .first()
            .cloned()
            .expect("at least one request recorded")
    }

    /// The `max_tokens` of the first recorded request.
    fn first_max_tokens(&self) -> Option<u32> {
        self.max_tokens_seen
            .lock()
            .expect("max_tokens_seen lock")
            .first()
            .copied()
            .expect("at least one request recorded")
    }
}

#[async_trait]
impl LlmClientTrait for ScriptedLlm {
    async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse, LlmError> {
        let system = req
            .messages
            .iter()
            .find(|m| m.role == "system")
            .and_then(|m| m.content.clone())
            .unwrap_or_default();
        self.seen
            .lock()
            .expect("seen lock")
            .push((req.model.clone(), system));
        self.max_tokens_seen
            .lock()
            .expect("max_tokens_seen lock")
            .push(req.max_tokens);

        let idx = self.cursor.fetch_add(1, Ordering::SeqCst);
        match self.responses.get(idx) {
            Some(resp) => Ok(resp.clone()),
            None => Err(LlmError::MissingConfig(format!(
                "scripted LLM exhausted at call {idx}"
            ))),
        }
    }
}

/// An `LlmClientTrait` that sleeps before every `chat` call, then delegates.
///
/// Why: #2207's `with_timeout_secs` override must actually shorten the loop's
/// wall-clock budget; the only deterministic, offline way to prove that is a
/// mock whose response arrives later than a short configured deadline.
/// What: Wraps a `ScriptedLlm`; `chat` sleeps `delay` before returning
/// whatever the inner scripted client would have returned.
/// Test: `runner::tests::with_timeout_secs_shortens_the_deadline`.
struct SleepyLlm {
    inner: ScriptedLlm,
    delay: std::time::Duration,
}

#[async_trait]
impl LlmClientTrait for SleepyLlm {
    async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse, LlmError> {
        tokio::time::sleep(self.delay).await;
        self.inner.chat(req).await
    }
}

/// A tool whose invocations are recorded, used to prove gating.
///
/// Why: To assert `tools.allowed` enforcement we need a tool that records
/// whether it was actually dispatched.
/// What: `execute` pushes its name onto a shared log and returns success.
/// Test: `tools_allowed_is_enforced`.
struct RecordingTool {
    name: &'static str,
    invoked: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl ToolExecutor for RecordingTool {
    fn name(&self) -> &str {
        self.name
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": self.name,
                "description": "recording tool",
                "parameters": { "type": "object", "properties": {} }
            }
        })
    }

    async fn execute(&self, _args: Value) -> ToolResult {
        self.invoked
            .lock()
            .expect("invoked lock")
            .push(self.name.to_string());
        ToolResult::ok(format!("{}: ran", self.name))
    }
}

// ── Fixture builders ─────────────────────────────────────────────────────────

/// A response in which the assistant calls a named tool with empty args.
fn tool_call_response(call_id: &str, tool: &str) -> Value {
    json!({
        "id": "gen-tool",
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": call_id,
                    "type": "function",
                    "function": { "name": tool, "arguments": "{}" }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": { "prompt_tokens": 30, "completion_tokens": 10, "total_tokens": 40 }
    })
}

/// A response in which the assistant emits final text and stops.
fn stop_response(text: &str) -> Value {
    json!({
        "id": "gen-stop",
        "choices": [{
            "message": { "role": "assistant", "content": text, "tool_calls": [] },
            "finish_reason": "stop"
        }],
        "usage": { "prompt_tokens": 15, "completion_tokens": 5, "total_tokens": 20 }
    })
}

// ── Fixtures: an agents config dir ───────────────────────────────────────────

/// Create a temp agents dir containing a `python-engineer.toml`.
///
/// Why: The runner loads `<dir>/<agent>.toml`; tests need a real file on disk.
/// What: Writes `python-engineer.toml` with the given body and returns the dir.
/// Test: Used by most runner tests.
fn agents_dir_with(body: &str, agent: &str) -> TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(tmp.path().join(format!("{agent}.toml")), body).expect("write agent toml");
    tmp
}

/// A registry factory closure that registers the given recording tools.
///
/// Why: Tests need a `RegistryFactory` that yields a known tool set so gating
/// and dispatch are observable.
/// What: Returns an `Arc<ToolRegistry>` with each `(name)` registered as a
/// `RecordingTool` sharing `invoked`.
fn recording_factory(
    tools: Vec<&'static str>,
    invoked: Arc<Mutex<Vec<String>>>,
) -> impl Fn(
    AgentConfig,
    RunContext,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Arc<ToolRegistry>> + Send>> {
    move |_agent: AgentConfig, _ctx: RunContext| {
        let tools = tools.clone();
        let invoked = Arc::clone(&invoked);
        Box::pin(async move {
            let mut reg = ToolRegistry::new();
            for name in tools {
                reg.register(Arc::new(RecordingTool {
                    name,
                    invoked: Arc::clone(&invoked),
                }));
            }
            Arc::new(reg)
        })
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

/// Default config is sane.
///
/// Why: Defaults are the most-used construction path; guard them.
/// What: Assert positive turn cap and timeout.
/// Test: this test.
#[test]
fn config_default_is_sane() {
    let cfg = InProcessRunnerConfig::default();
    assert!(cfg.max_turns >= 1);
    assert!(cfg.timeout_secs >= 1);
}

/// The default turn budget is generous enough for a real multi-file task, not
/// the old 8-turn default that a package-sized delegation could exhaust
/// mid-work (bake-off L1 diagnosis).
///
/// Why: A regression here would silently reintroduce the destructive
/// re-delegation failure mode: a delegated engineer running out of turns
/// before finishing a multi-file package, forcing a `TurnCapExceeded` abort.
/// What: Asserts the default is comfortably above the old value of 8 and
/// still overridable (`with_config`/`RunContext.max_turns_override` are
/// covered by other tests in this module).
/// Test: this test.
#[test]
fn default_max_turns_is_generous() {
    let cfg = InProcessRunnerConfig::default();
    assert!(
        cfg.max_turns > 8,
        "default max_turns must be raised above the old 8-turn cap, got {}",
        cfg.max_turns
    );
    assert!(
        cfg.max_turns >= 30,
        "default max_turns should comfortably fit a multi-file package task, got {}",
        cfg.max_turns
    );
}

/// `agent_config_exists` detects present and absent configs.
///
/// Why: Callers pre-check agent names with the same path rule the runner uses.
/// What: Write one agent; assert present is true and a bogus name is false.
/// Test: this test.
#[test]
fn agent_config_exists_detects_present_and_absent() {
    let tmp = agents_dir_with("[agent]\nname = \"python-engineer\"\n", "python-engineer");
    assert!(super::agent_config_exists(tmp.path(), "python-engineer"));
    assert!(!super::agent_config_exists(tmp.path(), "ghost"));
}

/// Stubbed-PM: `delegate_to_agent(python-engineer)` runs the engineer's own loop
/// on its own slug and returns an `AgentOutput` to the PM.
///
/// Why: This is the core #1029 acceptance criterion — the PM's delegate tool
/// dispatches to the in-process runner, which drives the engineer's loop and
/// hands the result back. We assert the engineer's model slug was used and the
/// PM received the engineer's final text.
/// What: Engineer config pins `deepseek/deepseek-chat`. Script [tool_call,
/// stop("engineer done")]. Build the runner, wrap it in a `DelegateToAgentTool`
/// (the PM's tool), and `execute` a delegation. Assert success, the returned
/// content, that the engineer's slug drove the loop, and two chat calls.
/// Test: this test.
#[tokio::test]
async fn delegate_runs_engineer_loop() {
    let body = r#"
[agent]
name = "python-engineer"
model = "deepseek/deepseek-chat"

[system_prompt]
content = "You are a Python engineer."
"#;
    let tmp = agents_dir_with(body, "python-engineer");

    let llm = Arc::new(ScriptedLlm::from_json(&[
        tool_call_response("c1", "work_tool"),
        stop_response("engineer done"),
    ]));
    let invoked = Arc::new(Mutex::new(Vec::new()));
    let factory = Arc::new(recording_factory(vec!["work_tool"], Arc::clone(&invoked)));

    let runner = Arc::new(InProcessAgentRunner::new(
        llm.clone(),
        factory,
        tmp.path().to_path_buf(),
    ));

    // The PM dispatches through its delegate tool — the production call path.
    let delegate = DelegateToAgentTool::new(runner).with_config_dir(tmp.path().to_path_buf());
    let result = delegate
        .execute(json!({"agent_name": "python-engineer", "task": "write a function"}))
        .await;

    assert!(
        !result.is_error(),
        "delegation should succeed: {}",
        result.content()
    );
    assert_eq!(
        result.content(),
        "engineer done",
        "PM must receive the engineer's final AgentOutput content"
    );
    assert_eq!(llm.calls(), 2, "engineer loop made exactly two chat calls");
    let (model, _) = llm.first_request();
    assert_eq!(
        model, "deepseek/deepseek-chat",
        "engineer's own model slug must drive its loop"
    );
    assert_eq!(
        invoked.lock().expect("invoked").as_slice(),
        ["work_tool"],
        "the engineer's tool must have run inside its own loop"
    );
}

/// The delegated engineer's own `[llm].max_tokens` reaches its `ChatRequest`.
///
/// Why: This is the regression guard for the run_task bug where
/// `InProcessAgentRunner::run_pipeline` built the engineer's `AgentLoopConfig`
/// without ever consulting `agent.llm.max_tokens`, silently capping every
/// engineer turn at the agent-loop default (formerly a hard-coded 1024) and
/// truncating real file writes. `resolve_max_tokens` must now flow the
/// configured cap into the loop that actually drives the engineer.
/// What: Engineer config sets `[llm].max_tokens = 8192`. Script a single stop
/// turn; assert the `ScriptedLlm` observed `Some(8192)`.
/// Test: this test.
#[tokio::test]
async fn engineer_llm_max_tokens_reaches_chat_request() {
    let body = r#"
[agent]
name = "python-engineer"
model = "deepseek/deepseek-chat"

[llm]
max_tokens = 8192

[system_prompt]
content = "You are a Python engineer."
"#;
    let tmp = agents_dir_with(body, "python-engineer");

    let llm = Arc::new(ScriptedLlm::from_json(&[stop_response("engineer done")]));
    let invoked = Arc::new(Mutex::new(Vec::new()));
    let factory = Arc::new(recording_factory(vec!["work_tool"], Arc::clone(&invoked)));

    let runner = Arc::new(InProcessAgentRunner::new(
        llm.clone(),
        factory,
        tmp.path().to_path_buf(),
    ));

    let delegate = DelegateToAgentTool::new(runner).with_config_dir(tmp.path().to_path_buf());
    let result = delegate
        .execute(json!({"agent_name": "python-engineer", "task": "write a function"}))
        .await;

    assert!(
        !result.is_error(),
        "delegation should succeed: {}",
        result.content()
    );
    assert_eq!(
        llm.first_max_tokens(),
        Some(8192),
        "the engineer's configured [llm].max_tokens must reach its ChatRequest"
    );
}

/// An unknown agent name yields a `RunnerError::UnknownAgent` (no loop runs).
///
/// Why: Delegating to a non-existent agent must fail cleanly, not panic or spin
/// the loop.
/// What: Empty agents dir; call `run("ghost", ...)`; assert an error whose
/// message names the agent, and that the LLM was never called.
/// Test: this test.
#[tokio::test]
async fn unknown_agent_errors() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let llm = Arc::new(ScriptedLlm::from_json(&[stop_response("never reached")]));
    let invoked = Arc::new(Mutex::new(Vec::new()));
    let factory = Arc::new(recording_factory(vec![], invoked));
    let runner = InProcessAgentRunner::new(llm.clone(), factory, tmp.path().to_path_buf());

    let err = runner
        .run("ghost", "do something")
        .await
        .expect_err("unknown agent must error");

    assert!(
        err.to_string().contains("ghost"),
        "error must name the unknown agent, got: {err}"
    );
    assert_eq!(llm.calls(), 0, "loop must not run for an unknown agent");
}

/// `tools.allowed` is enforced: a tool the model calls but the agent does not
/// allow is not dispatched (it is gated out of the registry).
///
/// Why: Per-agent tool gating is a hard #1029 criterion — an agent must not be
/// able to invoke a tool outside its allowlist.
/// What: Agent allows only `allowed_tool`. The factory builds BOTH `allowed_tool`
/// and `denied_tool`. Script the model to call `denied_tool`, then stop. Assert
/// the denied tool never ran (its name is absent from the invocation log) and the
/// allowed tool would have run had it been called.
/// Test: this test.
#[tokio::test]
async fn tools_allowed_is_enforced() {
    let body = r#"
[agent]
name = "python-engineer"
model = "openai/gpt-4o-mini"

[tools]
allowed = ["allowed_tool"]
"#;
    let tmp = agents_dir_with(body, "python-engineer");

    // Model tries to call the DENIED tool, then concludes.
    let llm = Arc::new(ScriptedLlm::from_json(&[
        tool_call_response("c1", "denied_tool"),
        stop_response("concluded"),
    ]));
    let invoked = Arc::new(Mutex::new(Vec::new()));
    let factory = Arc::new(recording_factory(
        vec!["allowed_tool", "denied_tool"],
        Arc::clone(&invoked),
    ));
    let runner = InProcessAgentRunner::new(llm.clone(), factory, tmp.path().to_path_buf());

    let out = runner
        .run("python-engineer", "try the denied tool")
        .await
        .expect("loop completes (denied tool surfaces a recoverable error)");

    assert_eq!(out.content, "concluded");
    let ran = invoked.lock().expect("invoked");
    assert!(
        !ran.contains(&"denied_tool".to_string()),
        "denied tool must never execute; gated out of the registry, ran: {ran:?}"
    );
}

/// With no `tools.allowed`, every tool the factory builds is permitted.
///
/// Why: An absent allowlist means "all tools"; gating must not strip them.
/// What: Agent has no `[tools]` table. Model calls `any_tool`, then stops.
/// Assert the tool actually ran.
/// Test: this test.
#[tokio::test]
async fn no_allowlist_permits_all() {
    let body = r#"
[agent]
name = "python-engineer"
model = "openai/gpt-4o-mini"
"#;
    let tmp = agents_dir_with(body, "python-engineer");

    let llm = Arc::new(ScriptedLlm::from_json(&[
        tool_call_response("c1", "any_tool"),
        stop_response("ok"),
    ]));
    let invoked = Arc::new(Mutex::new(Vec::new()));
    let factory = Arc::new(recording_factory(vec!["any_tool"], Arc::clone(&invoked)));
    let runner = InProcessAgentRunner::new(llm.clone(), factory, tmp.path().to_path_buf());

    runner
        .run("python-engineer", "use a tool")
        .await
        .expect("completes");

    assert_eq!(
        invoked.lock().expect("invoked").as_slice(),
        ["any_tool"],
        "with no allowlist, the tool must run"
    );
}

/// The engineer's token usage rolls up into the returned `AgentOutput`.
///
/// Why: For #1030 the orchestrator sums the sub-agent's usage into the PM's
/// transcript; that requires the runner's `AgentOutput.usage` to carry the
/// engineer's accrued tokens across all of its turns.
/// What: Script [tool_call (30+10), stop (15+5)]; the returned usage must equal
/// the sum across both engineer turns.
/// Test: this test.
#[tokio::test]
async fn usage_rolls_up_to_output() {
    let body = "[agent]\nname = \"python-engineer\"\nmodel = \"openai/gpt-4o-mini\"\n";
    let tmp = agents_dir_with(body, "python-engineer");

    let llm = Arc::new(ScriptedLlm::from_json(&[
        tool_call_response("c1", "any_tool"),
        stop_response("done"),
    ]));
    let invoked = Arc::new(Mutex::new(Vec::new()));
    let factory = Arc::new(recording_factory(vec!["any_tool"], invoked));
    let runner = InProcessAgentRunner::new(llm.clone(), factory, tmp.path().to_path_buf());

    let out = runner
        .run("python-engineer", "task")
        .await
        .expect("completes");

    assert_eq!(
        out.usage.prompt_tokens,
        30 + 15,
        "prompt tokens sum across turns"
    );
    assert_eq!(
        out.usage.completion_tokens,
        10 + 5,
        "completion tokens sum across turns"
    );
}

/// `RunContext.model` overrides the agent's configured model for one call.
///
/// Why: The orchestrator can pin a model per delegation; per-call override must
/// win over agent config (model-comparison harness varies the model per run).
/// What: Agent config pins one model; `RunContext.model` pins another. Assert the
/// RunContext slug drove the loop's request.
/// Test: this test.
#[tokio::test]
async fn run_context_overrides_model() {
    let body = "[agent]\nname = \"python-engineer\"\nmodel = \"deepseek/deepseek-chat\"\n";
    let tmp = agents_dir_with(body, "python-engineer");

    let llm = Arc::new(ScriptedLlm::from_json(&[stop_response("done")]));
    let invoked = Arc::new(Mutex::new(Vec::new()));
    let factory = Arc::new(recording_factory(vec![], invoked));
    let runner = InProcessAgentRunner::new(llm.clone(), factory, tmp.path().to_path_buf());

    let ctx = RunContext {
        model: Some("anthropic/claude-sonnet-4-6".to_string()),
        ..Default::default()
    };
    runner
        .run_with_context("python-engineer", "task", &ctx)
        .await
        .expect("completes");

    let (model, _) = llm.first_request();
    assert_eq!(
        model, "anthropic/claude-sonnet-4-6",
        "RunContext.model must override the agent's configured model"
    );
}

/// `RunContext.max_turns_override` caps the loop below the runner default.
///
/// Why: A per-call turn cap lets the orchestrator bound a single delegation; the
/// override must win over `InProcessRunnerConfig.max_turns`.
/// What: Runner default is 8 turns, but `max_turns_override = 1`. Script only
/// tool-call responses so the loop would run forever without a cap; assert it
/// aborts after exactly one chat call.
/// Test: this test.
#[tokio::test]
async fn run_context_overrides_max_turns() {
    let body = "[agent]\nname = \"python-engineer\"\nmodel = \"openai/gpt-4o-mini\"\n";
    let tmp = agents_dir_with(body, "python-engineer");

    // Always-tool-call: the loop never converges, so only the cap stops it.
    let llm = Arc::new(ScriptedLlm::from_json(&[
        tool_call_response("c1", "any_tool"),
        tool_call_response("c2", "any_tool"),
        tool_call_response("c3", "any_tool"),
    ]));
    let invoked = Arc::new(Mutex::new(Vec::new()));
    let factory = Arc::new(recording_factory(vec!["any_tool"], invoked));
    let runner = InProcessAgentRunner::new(llm.clone(), factory, tmp.path().to_path_buf())
        .with_config(InProcessRunnerConfig {
            max_turns: 8,
            timeout_secs: 30,
        });

    let ctx = RunContext {
        max_turns_override: Some(1),
        ..Default::default()
    };
    let err = runner
        .run_with_context("python-engineer", "loop", &ctx)
        .await
        .expect_err("a 1-turn cap on a non-converging loop must abort");

    assert!(
        err.to_string().contains("turn cap of 1"),
        "error should report the overridden cap, got: {err}"
    );
    assert_eq!(
        llm.calls(),
        1,
        "loop must stop after the single allowed turn"
    );
}

/// `with_timeout_secs` actually shortens the loop's wall-clock deadline
/// (#2207).
///
/// Why: This is the direct regression guard for `run_task`/`task::executor`
/// threading a resolved run-wide deadline onto the delegated engineer's
/// runner — if `with_timeout_secs` were a no-op, a raised or lowered deadline
/// on the PM side would never actually reach the engineer's own loop.
/// What: A response that would otherwise arrive instantly is delayed 3s by
/// `SleepyLlm`; the runner's timeout is overridden down to 1s. Assert the run
/// errors with the `Timeout` variant's message (not `TurnCapExceeded` or a
/// transport error) and that it does so well before the full 3s delay would
/// have elapsed.
/// Test: this test.
#[tokio::test]
async fn with_timeout_secs_shortens_the_deadline() {
    let body = "[agent]\nname = \"python-engineer\"\nmodel = \"openai/gpt-4o-mini\"\n";
    let tmp = agents_dir_with(body, "python-engineer");

    let llm = Arc::new(SleepyLlm {
        inner: ScriptedLlm::from_json(&[stop_response("done")]),
        delay: std::time::Duration::from_secs(3),
    });
    let invoked = Arc::new(Mutex::new(Vec::new()));
    let factory = Arc::new(recording_factory(vec![], invoked));
    let runner =
        InProcessAgentRunner::new(llm, factory, tmp.path().to_path_buf()).with_timeout_secs(1);

    let started = std::time::Instant::now();
    let err = runner
        .run("python-engineer", "task")
        .await
        .expect_err("a 1s deadline against a 3s-delayed response must time out");
    let elapsed = started.elapsed();

    assert!(
        err.to_string().contains("wall-clock timeout of 1s"),
        "error must be the Timeout variant, got: {err}"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(3),
        "the 1s override must fire well before the mock's 3s delay, elapsed={elapsed:?}"
    );
}

/// `with_timeout_secs` changes ONLY the timeout, leaving a previously
/// configured `max_turns` untouched (#2207).
///
/// Why: `with_timeout_secs` is a targeted setter specifically so callers
/// don't have to reconstruct a whole `InProcessRunnerConfig` (risking a
/// silent `max_turns` reset to its default) just to change the deadline.
/// What: Configure `max_turns: 2` via `with_config`, then call
/// `with_timeout_secs` with a generous value; script tool calls that never
/// converge. Assert the run still aborts via `TurnCapExceeded` at exactly 2
/// calls, proving `max_turns` survived the later `with_timeout_secs` call.
/// Test: this test.
#[tokio::test]
async fn with_timeout_secs_preserves_configured_max_turns() {
    let body = "[agent]\nname = \"python-engineer\"\nmodel = \"openai/gpt-4o-mini\"\n";
    let tmp = agents_dir_with(body, "python-engineer");

    let llm = Arc::new(ScriptedLlm::from_json(&[
        tool_call_response("c1", "any_tool"),
        tool_call_response("c2", "any_tool"),
        tool_call_response("c3", "any_tool"),
    ]));
    let invoked = Arc::new(Mutex::new(Vec::new()));
    let factory = Arc::new(recording_factory(vec!["any_tool"], invoked));
    let runner = InProcessAgentRunner::new(llm.clone(), factory, tmp.path().to_path_buf())
        .with_config(InProcessRunnerConfig {
            max_turns: 2,
            timeout_secs: 30,
        })
        .with_timeout_secs(300);

    let err = runner
        .run("python-engineer", "task")
        .await
        .expect_err("a 2-turn cap on a non-converging loop must abort");

    assert!(
        err.to_string().contains("turn cap of 2"),
        "max_turns=2 from with_config must survive the later with_timeout_secs call, got: {err}"
    );
    assert_eq!(
        llm.calls(),
        2,
        "loop must stop after the configured 2 turns"
    );
}

/// Project `CLAUDE.md` context is injected into the assembled system prompt.
///
/// Why: Sub-agents must see the same project rules as the PM (parity-spec); the
/// runner threads project context into `assemble_system_prompt`.
/// What: Attach a distinctive project-context marker; assert it appears in the
/// system prompt the scripted client received.
/// Test: this test.
#[tokio::test]
async fn project_context_reaches_prompt() {
    let body = "[agent]\nname = \"python-engineer\"\nmodel = \"openai/gpt-4o-mini\"\n[system_prompt]\ncontent = \"AGENT-PROMPT-MARKER\"\n";
    let tmp = agents_dir_with(body, "python-engineer");

    let llm = Arc::new(ScriptedLlm::from_json(&[stop_response("done")]));
    let invoked = Arc::new(Mutex::new(Vec::new()));
    let factory = Arc::new(recording_factory(vec![], invoked));
    let runner = InProcessAgentRunner::new(llm.clone(), factory, tmp.path().to_path_buf())
        .with_project_context("PROJECT-CONTEXT-MARKER");

    runner
        .run("python-engineer", "task")
        .await
        .expect("completes");

    let (_, system) = llm.first_request();
    assert!(
        system.contains("AGENT-PROMPT-MARKER"),
        "assembled prompt must include the agent's own system prompt"
    );
    assert!(
        system.contains("PROJECT-CONTEXT-MARKER"),
        "assembled prompt must include the injected project context"
    );
}

/// The skill catalog (#2069) reaches the assembled prompt in
/// `HarnessMode::DailyDriver`.
///
/// Why: A delegated sub-agent must see the same cheap, metadata-only skill
/// catalog the delegating PM does.
/// What: Attach a distinctive catalog marker via `with_skills_catalog`,
/// default mode (`DailyDriver`); assert it appears in the assembled prompt.
/// Test: this test.
#[tokio::test]
async fn skills_catalog_reaches_daily_driver_prompt() {
    let body = "[agent]\nname = \"python-engineer\"\nmodel = \"openai/gpt-4o-mini\"\n";
    let tmp = agents_dir_with(body, "python-engineer");

    let llm = Arc::new(ScriptedLlm::from_json(&[stop_response("done")]));
    let invoked = Arc::new(Mutex::new(Vec::new()));
    let factory = Arc::new(recording_factory(vec![], invoked));
    let runner = InProcessAgentRunner::new(llm.clone(), factory, tmp.path().to_path_buf())
        .with_skills_catalog("SKILLS-CATALOG-MARKER");

    runner
        .run("python-engineer", "task")
        .await
        .expect("completes");

    let (_, system) = llm.first_request();
    assert!(
        system.contains("SKILLS-CATALOG-MARKER"),
        "assembled DailyDriver prompt must include the skill catalog"
    );
}

/// The skill catalog (#2069) is ignored entirely under `HarnessMode::Parity`.
///
/// Why: Parity's byte-identical-schema/prompt guarantee (parity-spec D2)
/// must never depend on a project's skill catalog.
/// What: Attach a catalog marker AND `with_mode(Parity)`; assert the marker
/// is absent from the assembled prompt.
/// Test: this test.
#[tokio::test]
async fn skills_catalog_ignored_in_parity() {
    let body = "[agent]\nname = \"python-engineer\"\nmodel = \"openai/gpt-4o-mini\"\n";
    let tmp = agents_dir_with(body, "python-engineer");

    let llm = Arc::new(ScriptedLlm::from_json(&[stop_response("done")]));
    let invoked = Arc::new(Mutex::new(Vec::new()));
    let factory = Arc::new(recording_factory(vec![], invoked));
    let runner = InProcessAgentRunner::new(llm.clone(), factory, tmp.path().to_path_buf())
        .with_skills_catalog("SKILLS-CATALOG-MARKER")
        .with_mode(crate::mode::HarnessMode::Parity);

    runner
        .run("python-engineer", "task")
        .await
        .expect("completes");

    let (_, system) = llm.first_request();
    assert!(
        !system.contains("SKILLS-CATALOG-MARKER"),
        "Parity prompt must never include the skill catalog"
    );
}

/// `with_mode` must be chainable and a delegated run must complete
/// successfully under EITHER `HarnessMode` (#2059) — both modes are
/// functionally identical in M1, so this pins "the builder wires through
/// without breaking the run", not a behavioural difference (there isn't
/// one yet — see `crate::mode`'s and `agent_loop::tests`'s own docs for
/// where the real branch point is exercised).
///
/// Why: `InProcessAgentRunner` is the seam that propagates the PM's
/// resolved mode to a delegated sub-agent's own `AgentLoop`; a broken
/// propagation would most likely surface as the delegated run failing to
/// construct/complete, which this test would catch.
/// What: runs the SAME scripted delegation twice, once per mode, asserting
/// both complete successfully.
/// Test: this test.
#[tokio::test]
async fn with_mode_completes_a_delegated_run_in_both_modes() {
    let body = "[agent]\nname = \"python-engineer\"\nmodel = \"openai/gpt-4o-mini\"\n";
    let tmp = agents_dir_with(body, "python-engineer");

    for mode in [
        crate::mode::HarnessMode::DailyDriver,
        crate::mode::HarnessMode::Parity,
    ] {
        let llm = Arc::new(ScriptedLlm::from_json(&[stop_response("done")]));
        let invoked = Arc::new(Mutex::new(Vec::new()));
        let factory = Arc::new(recording_factory(vec![], invoked));
        let runner =
            InProcessAgentRunner::new(llm, factory, tmp.path().to_path_buf()).with_mode(mode);

        runner
            .run("python-engineer", "task")
            .await
            .unwrap_or_else(|e| panic!("run must complete under mode {mode:?}: {e}"));
    }
}

// ── #2056: ToolEventSink / cancel-flag propagation into the delegated loop ──────

/// Minimal `ToolEventSink` recording call order (see
/// `agent_loop::tests::RecordingSink` for the same pattern exercising the loop
/// directly; this one proves the runner PROPAGATES a sink to its own
/// internally-built `AgentLoop`).
struct RecordingSink {
    calls: Mutex<Vec<String>>,
    /// (DOC-39 AC-13) `(agent, agent_id)` pairs seen by `tool_started`, in
    /// call order — kept separate from `calls` so every pre-existing test
    /// asserting `calls`'s string format is untouched; only
    /// `concurrently_delegated_same_named_agents_get_distinct_ids` reads
    /// this.
    started_ids: Mutex<Vec<(String, String)>>,
}

#[async_trait]
impl ToolEventSink for RecordingSink {
    async fn tool_started(
        &self,
        agent: &str,
        agent_id: &str,
        call_id: &str,
        tool: &str,
        _args_preview: &str,
    ) {
        self.calls
            .lock()
            .expect("lock poisoned")
            .push(format!("started:{agent}:{tool}:{call_id}"));
        self.started_ids
            .lock()
            .expect("lock poisoned")
            .push((agent.to_string(), agent_id.to_string()));
    }

    async fn tool_finished(
        &self,
        agent: &str,
        _agent_id: &str,
        call_id: &str,
        tool: &str,
        success: bool,
        _result_preview: &str,
    ) {
        self.calls
            .lock()
            .expect("lock poisoned")
            .push(format!("finished:{agent}:{tool}:{call_id}:{success}"));
    }

    async fn tool_error(
        &self,
        agent: &str,
        _agent_id: &str,
        call_id: &str,
        tool: &str,
        _error: &str,
    ) {
        self.calls
            .lock()
            .expect("lock poisoned")
            .push(format!("error:{agent}:{tool}:{call_id}"));
    }
}

/// A sink attached via `with_tool_event_sink` must observe the DELEGATED
/// engineer's own tool dispatch, not just be silently dropped.
///
/// Why: #2056 needs a delegated sub-agent's tool activity to be observable to
/// the same sink the delegating (PM) loop uses; this is the propagation seam
/// that makes that possible.
/// What: Script [tool_call("mytool"), stop]; attach a `RecordingSink` to the
/// runner; assert it saw `started`/`finished` for `mytool`, ATTRIBUTED
/// (UI Phase 1) to `python-engineer` — the delegated agent — rather than to
/// the PM that shares this same sink instance.
/// Test: this test.
#[tokio::test]
async fn sink_reaches_delegated_loop() {
    let llm = Arc::new(ScriptedLlm::from_json(&[
        tool_call_response("call-1", "mytool"),
        stop_response("engineer done"),
    ]));
    let tmp = agents_dir_with(
        "[agent]\nname = \"python-engineer\"\nmodel = \"deepseek/deepseek-chat\"\n",
        "python-engineer",
    );
    let invoked = Arc::new(Mutex::new(Vec::new()));
    let factory = Arc::new(recording_factory(vec!["mytool"], invoked));
    let sink = Arc::new(RecordingSink {
        calls: Mutex::new(Vec::new()),
        started_ids: Mutex::new(Vec::new()),
    });

    let runner = InProcessAgentRunner::new(llm, factory, tmp.path().to_path_buf())
        .with_tool_event_sink(sink.clone());

    runner
        .run("python-engineer", "task")
        .await
        .expect("completes");

    assert_eq!(
        sink.calls.lock().expect("lock poisoned").as_slice(),
        [
            "started:python-engineer:mytool:call-1",
            "finished:python-engineer:mytool:call-1:true"
        ],
        "the runner must attribute a delegated sub-agent's tool events to \
         that sub-agent — the sink is shared with the PM, so an unattributed \
         (or PM-attributed) event here would make the two indistinguishable"
    );
}

/// Two SEPARATE delegations to the SAME `agent_name` must mint DISTINCT
/// `agent_id`s, even though `agent` is identical on both (DOC-39 AC-13.1/13.2).
///
/// Why: this is the exact regression #2862 left open — `tools::delegate`
/// spawns a delegated agent purely by `agent_name`, so two delegations to
/// `python-engineer` were indistinguishable on the event stream. Each call to
/// `InProcessAgentRunner::run` re-enters `run_pipeline`, which now mints a
/// fresh UUID v4 per invocation — this test drives the SAME runner (and the
/// SAME shared sink, mirroring how one `Arc<dyn ToolEventSink>` is shared
/// across every delegation in production) twice and asserts the two
/// `agent_id`s differ while `agent` stays `"python-engineer"` both times.
/// What: scripts `[tool_call, stop, tool_call, stop]` on one `ScriptedLlm` so
/// one runner + one llm serves both sequential `run()` calls; asserts both
/// recorded `(agent, agent_id)` pairs share `agent` but differ in `agent_id`,
/// and neither is empty/the unattributed sentinel.
/// Test: this test.
#[tokio::test]
async fn concurrently_delegated_same_named_agents_get_distinct_ids() {
    let llm = Arc::new(ScriptedLlm::from_json(&[
        tool_call_response("call-1", "mytool"),
        stop_response("engineer done 1"),
        tool_call_response("call-2", "mytool"),
        stop_response("engineer done 2"),
    ]));
    let tmp = agents_dir_with(
        "[agent]\nname = \"python-engineer\"\nmodel = \"deepseek/deepseek-chat\"\n",
        "python-engineer",
    );
    let invoked = Arc::new(Mutex::new(Vec::new()));
    let factory = Arc::new(recording_factory(vec!["mytool"], invoked));
    let sink = Arc::new(RecordingSink {
        calls: Mutex::new(Vec::new()),
        started_ids: Mutex::new(Vec::new()),
    });

    let runner = InProcessAgentRunner::new(llm, factory, tmp.path().to_path_buf())
        .with_tool_event_sink(sink.clone());

    runner
        .run("python-engineer", "task A")
        .await
        .expect("first delegation completes");
    runner
        .run("python-engineer", "task B")
        .await
        .expect("second delegation completes");

    let ids = sink.started_ids.lock().expect("lock poisoned").clone();
    assert_eq!(ids.len(), 2, "expected one tool_started per delegation");
    let (agent_a, id_a) = &ids[0];
    let (agent_b, id_b) = &ids[1];
    assert_eq!(agent_a, "python-engineer");
    assert_eq!(agent_b, "python-engineer");
    assert_ne!(
        id_a, id_b,
        "two delegations to the SAME agent_name must mint DISTINCT agent_ids \
         (DOC-39 AC-13) — otherwise concurrently-delegated same-named agents \
         stay indistinguishable on the event stream"
    );
    assert!(!id_a.is_empty() && id_a != crate::events::UNATTRIBUTED_AGENT_ID);
    assert!(!id_b.is_empty() && id_b != crate::events::UNATTRIBUTED_AGENT_ID);
}

/// A cancel flag attached via `with_cancel_flag` must abort the DELEGATED
/// engineer's own loop before it makes any chat call, not just the PM's.
///
/// Why: `session.cancel` must stop a whole in-flight run, including any
/// sub-agent currently delegated to — otherwise cancelling a session would
/// leave an orphaned engineer loop running.
/// What: Pre-set the flag, attach it via `with_cancel_flag`, and assert the
/// run errors with zero chat calls made.
/// Test: this test.
#[tokio::test]
async fn cancel_flag_reaches_delegated_loop() {
    let llm = Arc::new(ScriptedLlm::from_json(&[stop_response(
        "should never be reached",
    )]));
    let tmp = agents_dir_with(
        "[agent]\nname = \"python-engineer\"\nmodel = \"deepseek/deepseek-chat\"\n",
        "python-engineer",
    );
    let invoked = Arc::new(Mutex::new(Vec::new()));
    let factory = Arc::new(recording_factory(vec![], invoked));
    let cancel = Arc::new(AtomicBool::new(true));

    let runner = InProcessAgentRunner::new(llm.clone(), factory, tmp.path().to_path_buf())
        .with_cancel_flag(cancel);

    let result = runner.run("python-engineer", "task").await;
    assert!(result.is_err(), "a pre-cancelled run must error");
    assert_eq!(
        llm.calls(),
        0,
        "cancellation must be observed before any chat call"
    );
}
