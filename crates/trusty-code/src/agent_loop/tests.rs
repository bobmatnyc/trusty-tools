//! Unit and (gated) integration tests for the multi-turn agent loop.
//!
//! Why: The loop's control flow (continue-on-tool-call, stop-on-finish,
//! abort-on-cap, accrue-usage) is exactly the kind of branching that regresses
//! silently; a scripted mock LLM plus a trivial echo tool lets us assert every
//! branch deterministically and offline.
//! What: Defines `ScriptedLlm` (a `LlmClientTrait` that replays a queue of
//! pre-built `ChatResponse`s and counts calls) and `EchoTool` (a `ToolExecutor`
//! that echoes its `text` argument). Covers a two-turn flow, turn-cap abort,
//! recoverable tool-error continuation, usage accrual, and arg parsing, plus an
//! `#[ignore]`-gated live OpenRouter test.
//! Test: this module is itself the test surface.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use async_trait::async_trait;
use serde_json::{Value, json};

use super::{
    AgentLoop, AgentLoopConfig, AgentLoopError, CadenceConfig, CompactionConfig, ToolEventSink,
    Transcript,
};
use crate::llm::{ChatRequest, ChatResponse, LlmClientTrait, LlmError};
use crate::tools::{FinishTaskTool, ToolExecutor, ToolRegistry, ToolResult};

// ── Test doubles ───────────────────────────────────────────────────────────────

/// A `LlmClientTrait` that replays a fixed script of responses.
///
/// Why: Deterministic, offline substitute for the network client so loop
/// behaviour is testable without an API key.
/// What: Holds a `Vec<ChatResponse>` and an atomic cursor; each `chat` call
/// returns the next scripted response and increments the call counter. Running
/// past the end yields a transport-style error so a runaway loop fails loudly.
/// Test: Used by every loop test below.
struct ScriptedLlm {
    responses: Vec<ChatResponse>,
    cursor: AtomicUsize,
    /// (#2070) Every request's message list, in call order — lets compaction
    /// tests inspect exactly what the loop sent the model on each turn
    /// without reaching into `Transcript` internals.
    requests: Mutex<Vec<Vec<crate::llm::ChatMessage>>>,
    /// Every request's `max_tokens`, in call order — lets the
    /// `configured_max_tokens_reaches_chat_request` regression test assert the
    /// configured cap (not the old hard-coded 1024) reached the wire request.
    max_tokens_seen: Mutex<Vec<Option<u32>>>,
    /// (#2156) Every request's `tools` array, in call order — lets the
    /// prompt-caching gate tests inspect whether the last tool definition
    /// carries a `cache_control` breakpoint without reaching into
    /// `AgentLoop` internals.
    tools_seen: Mutex<Vec<Option<Vec<crate::llm::ToolDefinition>>>>,
    /// Every request's `usage` directive, in call order — lets the
    /// detailed-usage gate test inspect whether `RequestUsageConfig::detailed`
    /// reached the wire without reaching into `AgentLoop` internals
    /// (response-side cache-usage fix).
    usage_seen: Mutex<Vec<Option<crate::llm::RequestUsageConfig>>>,
}

impl ScriptedLlm {
    /// Build a scripted client from a list of JSON response fixtures.
    ///
    /// Why: Constructing `ChatResponse` directly is impossible (it is
    /// `Deserialize`-only), so tests author responses as JSON and parse them.
    /// What: Deserialises each fixture string into a `ChatResponse`.
    /// Test: Used by every loop test below.
    fn from_json(fixtures: &[Value]) -> Self {
        let responses = fixtures
            .iter()
            .map(|v| serde_json::from_value(v.clone()).expect("valid ChatResponse fixture"))
            .collect();
        Self {
            responses,
            cursor: AtomicUsize::new(0),
            requests: Mutex::new(Vec::new()),
            max_tokens_seen: Mutex::new(Vec::new()),
            tools_seen: Mutex::new(Vec::new()),
            usage_seen: Mutex::new(Vec::new()),
        }
    }

    /// Number of `chat` calls made so far.
    ///
    /// Why: Lets tests assert the loop made exactly the expected number of
    /// round-trips (e.g. that it stopped, not over-iterated).
    /// What: Reads the atomic cursor.
    /// Test: `two_turn_flow_completes`.
    fn calls(&self) -> usize {
        self.cursor.load(Ordering::SeqCst)
    }

    /// Every request's message list, in call order (#2070).
    ///
    /// Why: Compaction tests need to inspect what the loop actually sent the
    /// model on a given turn — e.g. the last turn's request should carry a
    /// `[compacted` summary and a replayed last-user message once compaction
    /// has fired.
    /// What: Clones the recorded request message lists.
    /// Test: `agent_loop::tests::daily_driver_mode_compacts_long_running_loop`,
    /// `agent_loop::tests::parity_mode_never_compacts_even_past_threshold`.
    fn requests(&self) -> Vec<Vec<crate::llm::ChatMessage>> {
        self.requests.lock().expect("lock").clone()
    }

    /// Every request's `max_tokens`, in call order.
    ///
    /// Why: The regression guard for the max-tokens bug needs to see exactly
    /// what cap the loop sent per turn, independent of message contents.
    /// What: Clones the recorded `max_tokens` values.
    /// Test: `configured_max_tokens_reaches_chat_request`.
    fn max_tokens_seen(&self) -> Vec<Option<u32>> {
        self.max_tokens_seen.lock().expect("lock").clone()
    }

    /// Every request's `tools` array, in call order (#2156).
    ///
    /// Why: The prompt-caching gate tests need to see exactly what tool
    /// schemas (and whether they carry a `cache_control` breakpoint) reached
    /// the wire on a given turn.
    /// What: Clones the recorded `tools` values.
    /// Test: `daily_driver_anthropic_model_marks_cache_breakpoints`,
    /// `parity_mode_never_marks_cache_breakpoints`,
    /// `non_anthropic_model_never_marks_cache_breakpoints`.
    fn tools_seen(&self) -> Vec<Option<Vec<crate::llm::ToolDefinition>>> {
        self.tools_seen.lock().expect("lock").clone()
    }

    /// Every request's `usage` directive, in call order (response-side
    /// cache-usage fix).
    ///
    /// Why: The detailed-usage gate test needs to see exactly what
    /// `ChatRequest.usage` reached the wire on a given turn.
    /// What: Clones the recorded `usage` values.
    /// Test: `build_request_sets_detailed_usage_for_openrouter`.
    fn usage_seen(&self) -> Vec<Option<crate::llm::RequestUsageConfig>> {
        self.usage_seen.lock().expect("lock").clone()
    }
}

#[async_trait]
impl LlmClientTrait for ScriptedLlm {
    async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse, LlmError> {
        if let Ok(mut guard) = self.requests.lock() {
            guard.push(req.messages.clone());
        }
        if let Ok(mut guard) = self.max_tokens_seen.lock() {
            guard.push(req.max_tokens);
        }
        if let Ok(mut guard) = self.tools_seen.lock() {
            guard.push(req.tools.clone());
        }
        if let Ok(mut guard) = self.usage_seen.lock() {
            guard.push(req.usage.clone());
        }
        let idx = self.cursor.fetch_add(1, Ordering::SeqCst);
        match self.responses.get(idx) {
            Some(resp) => Ok(resp.clone()),
            None => Err(LlmError::MissingConfig(format!(
                "scripted LLM exhausted at call {idx}"
            ))),
        }
    }
}

/// A trivial tool that echoes its `text` argument.
///
/// Why: The loop needs a real `ToolExecutor` to dispatch against; an echo tool
/// keeps assertions simple while exercising the full dispatch + result-append
/// path.
/// What: `execute` returns `Success("echo: <text>")`, or — when `fail` is set —
/// a recoverable error, to drive the recoverable-error test.
/// Test: `two_turn_flow_completes`, `recoverable_tool_error_continues`.
struct EchoTool {
    fail: bool,
}

#[async_trait]
impl ToolExecutor for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "echo",
                "description": "Echo the provided text back.",
                "parameters": {
                    "type": "object",
                    "properties": { "text": { "type": "string" } },
                    "required": ["text"]
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        if self.fail {
            return ToolResult::err("echo tool failed (recoverable)");
        }
        let text = args.get("text").and_then(Value::as_str).unwrap_or("<none>");
        ToolResult::ok(format!("echo: {text}"))
    }
}

// ── Fixture builders ────────────────────────────────────────────────────────────

/// Build a response fixture in which the assistant calls the `echo` tool.
fn tool_call_response(call_id: &str, text_arg: &str) -> Value {
    json!({
        "id": "gen-tool",
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": call_id,
                    "type": "function",
                    "function": {
                        "name": "echo",
                        "arguments": format!("{{\"text\":\"{text_arg}\"}}")
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": { "prompt_tokens": 12, "completion_tokens": 8, "total_tokens": 20 }
    })
}

/// Build a response that BOTH calls the `echo` tool AND reports `finish_reason
/// == "stop"`.
///
/// Why: Exercises the D3 finish/tool-call precedence rule — a `stop` reason must
/// not short-circuit pending tool dispatch. Some providers emit this shape.
fn tool_call_with_stop_finish(call_id: &str, text_arg: &str) -> Value {
    json!({
        "id": "gen-tool-stop",
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": call_id,
                    "type": "function",
                    "function": {
                        "name": "echo",
                        "arguments": format!("{{\"text\":\"{text_arg}\"}}")
                    }
                }]
            },
            "finish_reason": "stop"
        }],
        "usage": { "prompt_tokens": 11, "completion_tokens": 9, "total_tokens": 20 }
    })
}

/// Build a response fixture in which the assistant emits final text and stops.
fn stop_response(text: &str) -> Value {
    json!({
        "id": "gen-stop",
        "choices": [{
            "message": { "role": "assistant", "content": text, "tool_calls": [] },
            "finish_reason": "stop"
        }],
        "usage": { "prompt_tokens": 7, "completion_tokens": 5, "total_tokens": 12 }
    })
}

/// Construct an `AgentLoop` from a scripted client and a registry.
fn make_loop(
    llm: Arc<ScriptedLlm>,
    registry: Arc<ToolRegistry>,
    config: AgentLoopConfig,
) -> AgentLoop {
    AgentLoop::new(config, llm, registry)
}

fn registry_with_echo(fail: bool) -> Arc<ToolRegistry> {
    let mut reg = ToolRegistry::new();
    reg.register(Arc::new(EchoTool { fail }));
    Arc::new(reg)
}

/// Build a response fixture in which the assistant calls `finish_task`.
///
/// Why: `raw_arguments` is passed through verbatim (not re-serialised) so
/// tests can construct both well-formed JSON and deliberately malformed
/// strings for the repair-path tests.
fn finish_task_call_response(call_id: &str, raw_arguments: &str) -> Value {
    json!({
        "id": "gen-finish",
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": call_id,
                    "type": "function",
                    "function": { "name": "finish_task", "arguments": raw_arguments }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": { "prompt_tokens": 6, "completion_tokens": 6, "total_tokens": 12 }
    })
}

/// A registry containing only `FinishTaskTool` (#2072).
fn registry_with_finish_task() -> Arc<ToolRegistry> {
    let mut reg = ToolRegistry::new();
    reg.register(Arc::new(FinishTaskTool::new()));
    Arc::new(reg)
}

/// A registry containing `FinishTaskTool` AND `BashTool` (#2279).
///
/// Why: The verify-before-finish gate tests need a real `bash` tool so a
/// scripted `bash` call actually dispatches (its dispatch outcome does not
/// matter to the gate — only that the call landed in the transcript — but a
/// registered tool keeps the scenario representative of production wiring).
fn registry_with_finish_task_and_bash() -> Arc<ToolRegistry> {
    let mut reg = ToolRegistry::new();
    reg.register(Arc::new(FinishTaskTool::new()));
    reg.register(Arc::new(crate::tools::BashTool::default_config()));
    Arc::new(reg)
}

/// Build a response fixture in which the assistant calls `bash` with the
/// given `command` (#2279).
fn bash_call_response(call_id: &str, command: &str) -> Value {
    json!({
        "id": "gen-bash",
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": call_id,
                    "type": "function",
                    "function": {
                        "name": "bash",
                        "arguments": json!({"command": command}).to_string()
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": { "prompt_tokens": 8, "completion_tokens": 6, "total_tokens": 14 }
    })
}

// ── Tests ───────────────────────────────────────────────────────────────────────

/// Config defaults are present and sane.
///
/// Why: Defaults are the most-used construction path; guard them.
/// What: Assert `max_turns`, `timeout_secs`, a non-empty model, and that
/// `max_tokens` is generous enough for a real write turn (not the old
/// hard-coded 1024 that truncated file writes).
/// Test: this test.
#[test]
fn config_defaults_are_sane() {
    let cfg = AgentLoopConfig::default();
    assert!(cfg.max_turns >= 1);
    assert!(cfg.timeout_secs >= 1);
    assert!(!cfg.model.is_empty());
    assert!(
        cfg.max_tokens > 1024,
        "default max_tokens must exceed the old hard-coded 1024 cap"
    );
}

/// A configured `max_tokens` reaches the outgoing `ChatRequest` unchanged.
///
/// Why: This is the direct regression guard for the bug where `build_request`
/// hard-coded `max_tokens: Some(1024)` regardless of `AgentLoopConfig` —
/// truncating any turn (e.g. a real file write) that needed more. Every call
/// site now resolves the agent's `[llm].max_tokens` into `AgentLoopConfig`
/// before constructing the loop; this test pins that the loop itself honours
/// whatever value it is given rather than overriding it.
/// What: Configure `max_tokens: 8192`, run a single-turn stop, and assert the
/// `ScriptedLlm` observed `Some(8192)` — never `Some(1024)`.
/// Test: this test.
#[tokio::test]
async fn configured_max_tokens_reaches_chat_request() {
    let llm = Arc::new(ScriptedLlm::from_json(&[stop_response("done")]));
    let registry = registry_with_echo(false);
    let agent = make_loop(
        llm.clone(),
        registry,
        AgentLoopConfig {
            max_tokens: 8192,
            ..AgentLoopConfig::default()
        },
    );

    agent
        .run("system", "write a file")
        .await
        .expect("single stop turn should complete");

    let seen = llm.max_tokens_seen();
    assert_eq!(seen, vec![Some(8192)]);
    assert_ne!(
        seen[0],
        Some(1024),
        "configured max_tokens must not be clamped back to the old default"
    );
}

/// A tool call with syntactically malformed JSON arguments is reported as a
/// recoverable tool result, and the loop continues to completion.
///
/// Why: #1023 replaces the pre-#1023 silent `{}` degrade (the old private
/// `parse_args` helper) with `llm::ToolCallExtractor::parse_and_validate`.
/// The model must see the real parse failure — not have its malformed call
/// silently dispatched with empty arguments — and the loop must still
/// recover exactly like any other recoverable tool error.
/// What: Script [malformed tool_call, stop]; assert the run completes with
/// the stop text and made exactly two LLM calls (proving it continued past
/// the malformed call rather than aborting).
/// Test: this test.
#[tokio::test]
async fn malformed_tool_arguments_report_recoverable_error_and_loop_continues() {
    let malformed_call = json!({
        "id": "gen-tool-malformed",
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_bad",
                    "type": "function",
                    "function": { "name": "echo", "arguments": "{not valid json" }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": { "prompt_tokens": 5, "completion_tokens": 5, "total_tokens": 10 }
    });
    let llm = Arc::new(ScriptedLlm::from_json(&[
        malformed_call,
        stop_response("Recovered from malformed arguments"),
    ]));
    let registry = registry_with_echo(false);
    let agent = make_loop(llm.clone(), registry, AgentLoopConfig::default());

    let out = agent
        .run("system", "call echo with bad json")
        .await
        .expect("loop should continue past malformed tool arguments");

    assert_eq!(out.content, "Recovered from malformed arguments");
    assert_eq!(
        llm.calls(),
        2,
        "loop should make two calls despite the malformed call"
    );
}

/// An unknown tool name (not registered) is also reported as a recoverable
/// tool result rather than panicking or dispatching against a missing tool.
///
/// Why: `ToolCallExtractor::parse_and_validate` returns `UnknownTool` when the
/// schema lookup misses; `dispatch_all` must route that through the same
/// recoverable path as a malformed-JSON failure.
/// What: Script a tool call naming an unregistered tool, then a stop; assert
/// the run still completes.
/// Test: this test.
#[tokio::test]
async fn unknown_tool_call_reports_recoverable_error_and_loop_continues() {
    let unknown_call = json!({
        "id": "gen-tool-unknown",
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_unknown",
                    "type": "function",
                    "function": { "name": "does_not_exist", "arguments": "{}" }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": { "prompt_tokens": 5, "completion_tokens": 5, "total_tokens": 10 }
    });
    let llm = Arc::new(ScriptedLlm::from_json(&[
        unknown_call,
        stop_response("Recovered from unknown tool"),
    ]));
    let registry = registry_with_echo(false);
    let agent = make_loop(llm.clone(), registry, AgentLoopConfig::default());

    let out = agent
        .run("system", "call a nonexistent tool")
        .await
        .expect("loop should continue past an unknown tool call");

    assert_eq!(out.content, "Recovered from unknown tool");
    assert_eq!(llm.calls(), 2);
}

/// `schema_tool_name` extracts `function.name` and degrades gracefully.
///
/// Why: When a tool schema fails to parse, the warn log names the offending tool
/// via this helper; it must pull the name from a still-valid raw schema and not
/// panic on absent/malformed paths.
/// What: Assert the name is read from a well-formed schema, and that missing or
/// non-string paths fall back to `"<unknown>"`.
/// Test: this test.
#[test]
fn schema_tool_name_extracts_or_falls_back() {
    let good = json!({ "type": "function", "function": { "name": "echo" } });
    assert_eq!(super::schema_tool_name(&good), "echo");

    let no_function = json!({ "type": "function" });
    assert_eq!(super::schema_tool_name(&no_function), "<unknown>");

    let non_string_name = json!({ "function": { "name": 42 } });
    assert_eq!(super::schema_tool_name(&non_string_name), "<unknown>");
}

/// #2059: `tool_definitions`'s `HarnessMode` branch must produce IDENTICAL
/// tool schemas for both modes in M1 — the branch exists (a real seam for
/// P1B to hook into) but has no behavioural difference yet.
///
/// Why: pins the documented "byte-identical until P1B" contract at the
/// tool-schema layer, mirroring `prompt::tests::assemble_system_prompt_for_mode_is_identical_in_m1`
/// for the prompt-assembly layer.
/// What: constructs two loops over the SAME registry, differing only in
/// `AgentLoopConfig.mode`, and asserts `tool_definitions()` returns the
/// exact same `Vec<ToolDefinition>` for both.
/// Test: this test.
#[test]
fn tool_definitions_identical_across_modes_in_m1() {
    let llm = Arc::new(ScriptedLlm::from_json(&[]));
    let registry = registry_with_echo(false);

    let parity = make_loop(
        llm.clone(),
        registry.clone(),
        AgentLoopConfig {
            mode: crate::mode::HarnessMode::Parity,
            ..AgentLoopConfig::default()
        },
    );
    let daily_driver = make_loop(
        llm,
        registry,
        AgentLoopConfig {
            mode: crate::mode::HarnessMode::DailyDriver,
            ..AgentLoopConfig::default()
        },
    );

    assert_eq!(parity.tool_definitions(), daily_driver.tool_definitions());
}

/// A low-threshold `CompactionConfig` for a `DailyDriver` run to compact
/// aggressively and deterministically inside a test.
fn aggressive_compaction() -> CompactionConfig {
    CompactionConfig {
        token_threshold: 1,
        keep_last_messages: 2,
    }
}

/// A long-running `DailyDriver` loop compacts older turns; the request it
/// finally sends carries a summary and a replayed last user message (#2070).
///
/// Why: §5.4's whole mechanism only matters if it is actually wired into the
/// live turn loop, not just the `Transcript` unit — this is the
/// loop-level integration proof.
/// What: Scripts 5 tool-call round-trips then a final stop, with an
/// aggressive `CompactionConfig` so compaction fires well before the run
/// ends. Asserts the LAST request the scripted client received contains a
/// `[compacted` summary message and ends with a replayed `user` message
/// matching the original task, and that it is far shorter than the raw
/// message count the run accumulated.
/// Test: this test.
#[tokio::test]
async fn daily_driver_mode_compacts_long_running_loop() {
    let mut fixtures: Vec<Value> = (0..5)
        .map(|i| tool_call_response(&format!("call_{i}"), &format!("turn-{i}")))
        .collect();
    fixtures.push(stop_response("all done"));
    let llm = Arc::new(ScriptedLlm::from_json(&fixtures));
    let registry = registry_with_echo(false);

    let agent = make_loop(
        llm.clone(),
        registry,
        AgentLoopConfig {
            max_turns: 10,
            mode: crate::mode::HarnessMode::DailyDriver,
            compaction: aggressive_compaction(),
            ..AgentLoopConfig::default()
        },
    );

    agent
        .run("system prompt", "the original task")
        .await
        .expect("run completes");

    let requests = llm.requests();
    let last_request = requests.last().expect("at least one request");

    let has_summary = last_request
        .iter()
        .any(|m| m.content.as_deref().unwrap_or("").contains("[compacted"));
    assert!(
        has_summary,
        "expected a compaction summary in {last_request:?}"
    );

    let tail = last_request.last().expect("non-empty request");
    assert_eq!(tail.role, "user");
    assert_eq!(tail.content.as_deref(), Some("the original task"));

    // Raw history at this point is 2 seed + 5*(assistant+tool) = 12 messages;
    // the compacted view sent on the last turn must be materially smaller.
    assert!(
        last_request.len() < 12,
        "expected a shrunk request, got {} messages",
        last_request.len()
    );
}

/// `Parity` mode never compacts, even past the same aggressive threshold
/// that triggers compaction under `DailyDriver` (#2070, §5.9 reconciliation).
///
/// Why: The parity-spec (D2) requires byte-identical, full-history requests
/// for cross-model benchmark fairness; compaction silently changing what a
/// Parity run sends would break that guarantee.
/// What: Same script and the SAME aggressive `CompactionConfig` as
/// `daily_driver_mode_compacts_long_running_loop`, but `mode: Parity`.
/// Asserts no request ever carries a `[compacted` summary and the last
/// request's message count equals the full raw history (2 seed + 5 tool
/// round-trips' worth of turns).
/// Test: this test.
#[tokio::test]
async fn parity_mode_never_compacts_even_past_threshold() {
    let mut fixtures: Vec<Value> = (0..5)
        .map(|i| tool_call_response(&format!("call_{i}"), &format!("turn-{i}")))
        .collect();
    fixtures.push(stop_response("all done"));
    let llm = Arc::new(ScriptedLlm::from_json(&fixtures));
    let registry = registry_with_echo(false);

    let agent = make_loop(
        llm.clone(),
        registry,
        AgentLoopConfig {
            max_turns: 10,
            mode: crate::mode::HarnessMode::Parity,
            compaction: aggressive_compaction(),
            ..AgentLoopConfig::default()
        },
    );

    agent
        .run("system prompt", "the original task")
        .await
        .expect("run completes");

    let requests = llm.requests();
    for request in &requests {
        let has_summary = request
            .iter()
            .any(|m| m.content.as_deref().unwrap_or("").contains("[compacted"));
        assert!(!has_summary, "Parity must never compact, got {request:?}");
    }

    let last_request = requests.last().expect("at least one request");
    assert_eq!(
        last_request.len(),
        12,
        "Parity must send the full raw history"
    );
}

// ── #2346: cadence-compressor turn-boundary wiring ──────────────────────────

/// Cadence stays off by default (#2346): `AgentLoopConfig::default().cadence`
/// is `None`, and the loop never ticks the transcript's cadence counter when
/// it is `None` — the exact "zero behaviour change for run_task/delegated
/// engineer" contract this ticket requires.
///
/// Why: Every pre-#2346 call site (`run_task`, the delegated engineer's own
/// loop) constructs `AgentLoopConfig` via `..AgentLoopConfig::default()`;
/// this is the direct regression guard that doing so keeps cadence off.
/// What: Run a several-turn script under `mode: DailyDriver` (the default)
/// with `cadence: None` (the default) via `run_with_transcript` against an
/// externally-owned `Transcript`, then assert `cadence_turn_count() == 0`.
/// Test: this test.
#[tokio::test]
async fn cadence_disabled_by_default() {
    let mut fixtures: Vec<Value> = (0..3)
        .map(|i| tool_call_response(&format!("call_{i}"), &format!("turn-{i}")))
        .collect();
    fixtures.push(stop_response("all done"));
    let llm = Arc::new(ScriptedLlm::from_json(&fixtures));
    let registry = registry_with_echo(false);

    let agent = make_loop(llm, registry, AgentLoopConfig::default());
    let mut transcript = Transcript::seed("system prompt", "the task");
    agent
        .run_with_transcript(&mut transcript, "the task")
        .await
        .expect("run completes");

    assert_eq!(transcript.cadence_turn_count(), 0);
}

/// `Parity` never ticks cadence even when a `CadenceConfig` is explicitly
/// attached (#2346, mirrors `parity_mode_never_compacts_even_past_threshold`'s
/// mode gate for the threshold compactor).
///
/// Why: The parity-spec (D2) byte-identical guarantee must hold for cadence
/// exactly as it does for threshold compaction — an operator accidentally
/// wiring `cadence: Some(_)` onto a Parity run must not silently break
/// benchmark fairness.
/// What: Same script shape as `cadence_disabled_by_default`, but
/// `mode: Parity` with an aggressive `cadence: Some(CadenceConfig { cadence_turns: 1, .. })`
/// attached; assert `cadence_turn_count() == 0` regardless.
/// Test: this test.
#[tokio::test]
async fn cadence_never_fires_in_parity_mode() {
    let mut fixtures: Vec<Value> = (0..3)
        .map(|i| tool_call_response(&format!("call_{i}"), &format!("turn-{i}")))
        .collect();
    fixtures.push(stop_response("all done"));
    let llm = Arc::new(ScriptedLlm::from_json(&fixtures));
    let registry = registry_with_echo(false);

    let agent = make_loop(
        llm,
        registry,
        AgentLoopConfig {
            mode: crate::mode::HarnessMode::Parity,
            cadence: Some(CadenceConfig {
                cadence_turns: 1,
                max_overhead_fraction_pct: 40,
            }),
            ..AgentLoopConfig::default()
        },
    );
    let mut transcript = Transcript::seed("system prompt", "the task");
    agent
        .run_with_transcript(&mut transcript, "the task")
        .await
        .expect("run completes");

    assert_eq!(transcript.cadence_turn_count(), 0);
}

/// `DailyDriver` mode with an explicit `CadenceConfig` attached ticks and
/// fires cadence compression at the turn boundary (#2346).
///
/// Why: The positive case completing the mode-gate matrix — cadence must
/// actually engage when both preconditions (`DailyDriver` + `Some(cadence)`)
/// hold, not just correctly stay inert in the negative cases above.
/// What: Scripts 6 tool-call round-trips then a stop (7 loop turns) under
/// `mode: DailyDriver` with `cadence_turns: 1` (fires every turn) and an
/// aggressive `compaction.keep_last_messages: 1` (small active zone, so
/// cadence has fresh entries to compact on nearly every turn). Asserts both
/// `cadence_turn_count() == 7` (one tick per loop turn) and
/// `cadence_fire_count() > 0` (at least one turn actually compacted
/// something).
/// Test: this test.
#[tokio::test]
async fn cadence_fires_in_daily_driver_when_configured() {
    let mut fixtures: Vec<Value> = (0..6)
        .map(|i| tool_call_response(&format!("call_{i}"), &format!("turn-{i}")))
        .collect();
    fixtures.push(stop_response("all done"));
    let llm = Arc::new(ScriptedLlm::from_json(&fixtures));
    let registry = registry_with_echo(false);

    let agent = make_loop(
        llm,
        registry,
        AgentLoopConfig {
            max_turns: 10,
            mode: crate::mode::HarnessMode::DailyDriver,
            compaction: CompactionConfig {
                token_threshold: 1_000_000, // keep the THRESHOLD backstop inert for this test
                keep_last_messages: 1,
            },
            cadence: Some(CadenceConfig {
                cadence_turns: 1,
                max_overhead_fraction_pct: 99,
            }),
            ..AgentLoopConfig::default()
        },
    );
    let mut transcript = Transcript::seed("system prompt", "the task");
    agent
        .run_with_transcript(&mut transcript, "the task")
        .await
        .expect("run completes");

    assert_eq!(transcript.cadence_turn_count(), 7);
    assert!(
        transcript.cadence_fire_count() > 0,
        "expected at least one cadence fire to actually compact something"
    );
}

// ── #2349: threshold compaction becomes a never-event regression signal ────
//
// Why: no reusable tracing-capture test utility exists elsewhere in this
// crate — `trusty_common::error_capture::layer::BugCaptureLayer` is the
// closest prior art (same "tap ERROR events via a `tracing_subscriber::Layer`"
// shape) but is feature-gated behind `bug-capture` and pulls in a
// fingerprinting/store stack this test has no use for. A small test-local
// layer mirroring its shape is simpler than wiring that feature in for one
// assertion.
// What: `ErrorCaptureLayer` records every ERROR-level event's `message`
// field into a shared `Arc<Mutex<Vec<String>>>`; `install_error_capture`
// installs it as the thread's default subscriber (mirrors
// `trusty_common::error_capture::layer::tests`'s identical
// `tracing::subscriber::with_default`/`set_default` pattern) for the
// duration of the returned guard.
struct ErrorCaptureLayer {
    messages: Arc<Mutex<Vec<String>>>,
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for ErrorCaptureLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        if *event.metadata().level() != tracing::Level::ERROR {
            return;
        }
        struct MessageVisitor(String);
        impl tracing::field::Visit for MessageVisitor {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    self.0 = format!("{value:?}");
                }
            }
        }
        let mut visitor = MessageVisitor(String::new());
        event.record(&mut visitor);
        self.messages.lock().expect("lock").push(visitor.0);
    }
}

/// Install an [`ErrorCaptureLayer`] as this thread's default subscriber.
///
/// Why: `#[tokio::test]` defaults to the current-thread runtime, so a
/// `tracing::subscriber::set_default` guard held for the test body's
/// lifetime (including across `.await` points) captures every ERROR event
/// the awaited future emits on this thread — no need for `with_default`'s
/// synchronous-closure shape.
/// What: Returns the `DefaultGuard` (drop it to restore the prior
/// subscriber) plus the shared message buffer.
fn install_error_capture() -> (tracing::subscriber::DefaultGuard, Arc<Mutex<Vec<String>>>) {
    use tracing_subscriber::layer::SubscriberExt as _;
    let messages = Arc::new(Mutex::new(Vec::new()));
    let layer = ErrorCaptureLayer {
        messages: messages.clone(),
    };
    let subscriber = tracing_subscriber::registry().with(layer);
    (tracing::subscriber::set_default(subscriber), messages)
}

/// A `CadenceConfig` present (cadence enabled) but sized so it never
/// actually schedules a fire, forcing the aggressive threshold compactor to
/// trip anyway — the "cadence sizing assumptions violated" forced-
/// degradation scenario #2349's acceptance criteria call for.
fn overwhelmed_cadence() -> CadenceConfig {
    CadenceConfig {
        cadence_turns: 1_000_000, // never a scheduled-fire turn in this test
        max_overhead_fraction_pct: 99,
    }
}

/// Forced-degradation acceptance test: cadence is enabled but never actually
/// schedules a fire (`overwhelmed_cadence`), while `aggressive_compaction`'s
/// `token_threshold: 1` trips the #2308 threshold compactor on the very
/// first turn boundary. This must increment
/// `Transcript::compaction_events()` AND emit an ERROR-level log — the
/// "cadence sizing / daemon-availability / turn-size assumptions violated"
/// regression signal.
///
/// Why: This is #2349's core acceptance criterion — proving the signal
/// actually fires end-to-end through `AgentLoop::maybe_compact_transcript`,
/// not just at the `Transcript` unit level.
/// What: Runs a 3-tool-call-then-stop script under `mode: DailyDriver` with
/// both `compaction: aggressive_compaction()` and
/// `cadence: Some(overwhelmed_cadence())` attached. Asserts
/// `transcript.compaction_events() > 0` and that the captured ERROR-level
/// log messages contain the expected "regression signal" text.
/// Test: this test.
#[tokio::test]
async fn forced_degradation_increments_counter_and_logs_error() {
    let (_guard, captured) = install_error_capture();

    let mut fixtures: Vec<Value> = (0..3)
        .map(|i| tool_call_response(&format!("call_{i}"), &format!("turn-{i}")))
        .collect();
    fixtures.push(stop_response("all done"));
    let llm = Arc::new(ScriptedLlm::from_json(&fixtures));
    let registry = registry_with_echo(false);

    let agent = make_loop(
        llm,
        registry,
        AgentLoopConfig {
            max_turns: 10,
            mode: crate::mode::HarnessMode::DailyDriver,
            compaction: aggressive_compaction(),
            cadence: Some(overwhelmed_cadence()),
            ..AgentLoopConfig::default()
        },
    );
    let mut transcript = Transcript::seed("system prompt", "the task");
    agent
        .run_with_transcript(&mut transcript, "the task")
        .await
        .expect("run completes");

    drop(_guard);

    assert!(
        transcript.compaction_events() > 0,
        "threshold compaction should have fired at least once under the tiny token_threshold"
    );
    let messages = captured.lock().expect("lock");
    assert!(
        messages
            .iter()
            .any(|m| m.contains("regression signal") && m.contains("cadence")),
        "expected an error-level regression-signal log, got: {messages:?}"
    );
}

/// The cadence-`None` counterpart: threshold compaction firing is the
/// PRIMARY, EXPECTED mechanism for `run_task` one-shot / Parity / delegated
/// sub-agent contexts (all `cadence: None`) — it must NOT emit an
/// error-level log, and today's existing (no-log) behaviour at this call
/// site must stay exactly as it was before this ticket.
///
/// Why: §3 of the ticket's semantics is explicit that the error-log framing
/// applies ONLY when `cadence.is_some()` — this is the negative-case guard
/// against that gate regressing to "always log" or "log regardless of
/// cadence".
/// What: Identical script and `aggressive_compaction` to
/// `forced_degradation_increments_counter_and_logs_error`, but
/// `cadence: None` (the default). Asserts `compaction_events() > 0` (the
/// counter itself is NOT cadence-gated) while the captured ERROR log list is
/// empty.
/// Test: this test.
#[tokio::test]
async fn cadence_none_threshold_fire_does_not_log_error() {
    let (_guard, captured) = install_error_capture();

    let mut fixtures: Vec<Value> = (0..3)
        .map(|i| tool_call_response(&format!("call_{i}"), &format!("turn-{i}")))
        .collect();
    fixtures.push(stop_response("all done"));
    let llm = Arc::new(ScriptedLlm::from_json(&fixtures));
    let registry = registry_with_echo(false);

    let agent = make_loop(
        llm,
        registry,
        AgentLoopConfig {
            max_turns: 10,
            mode: crate::mode::HarnessMode::DailyDriver,
            compaction: aggressive_compaction(),
            cadence: None,
            ..AgentLoopConfig::default()
        },
    );
    let mut transcript = Transcript::seed("system prompt", "the task");
    agent
        .run_with_transcript(&mut transcript, "the task")
        .await
        .expect("run completes");

    drop(_guard);

    assert!(
        transcript.compaction_events() > 0,
        "the counter itself must still increment regardless of cadence"
    );
    let messages = captured.lock().expect("lock");
    assert!(
        messages.is_empty(),
        "cadence: None must never emit the error-level regression signal, got: {messages:?}"
    );
}

/// The FULL parity conformance check tying the model-routing layer to the
/// compaction layer (#2073, the "M2 integration" proof §5.9 asks for): two
/// `HarnessMode::Parity` runs over the identical script, registry, and
/// aggressive `CompactionConfig`, differing ONLY in `AgentLoopConfig.model`,
/// must send the model IDENTICAL message sequences turn-by-turn — proving
/// Parity's assembled requests are deterministic and model-independent, not
/// merely "does not compact" (`parity_mode_never_compacts_even_past_threshold`)
/// or "same schema in M1" (`tool_definitions_identical_across_modes_in_m1`)
/// in isolation.
///
/// Why: Each P1B layer (#2059 mode, #2068 edit-format, #2069 skills, #2070
/// compaction) already has its own per-layer unit test proving IT respects
/// the mode branch; #2073's job is proving the layers hold TOGETHER — that
/// swapping the model under Parity changes nothing about what the loop sends
/// except the `model` field itself (which `ChatRequest.messages` never
/// carries, so comparing `ScriptedLlm::requests()` — the recorded message
/// lists — directly proves this).
/// What: Runs the SAME 5-tool-call-then-stop script against two configs that
/// differ only in `model`, both `mode: Parity` with the SAME aggressive
/// `CompactionConfig` used by the two single-mode tests above. Asserts the
/// two runs' full `requests()` vectors are byte-for-byte equal.
/// Test: this test.
#[tokio::test]
async fn parity_mode_message_sequences_are_model_independent() {
    fn script() -> Vec<Value> {
        let mut fixtures: Vec<Value> = (0..5)
            .map(|i| tool_call_response(&format!("call_{i}"), &format!("turn-{i}")))
            .collect();
        fixtures.push(stop_response("all done"));
        fixtures
    }

    fn run_with_model(model: &str) -> (AgentLoop, Arc<ScriptedLlm>) {
        let llm = Arc::new(ScriptedLlm::from_json(&script()));
        let registry = registry_with_echo(false);
        let agent = make_loop(
            llm.clone(),
            registry,
            AgentLoopConfig {
                max_turns: 10,
                model: model.to_string(),
                mode: crate::mode::HarnessMode::Parity,
                compaction: aggressive_compaction(),
                ..AgentLoopConfig::default()
            },
        );
        (agent, llm)
    }

    let (agent_a, llm_a) = run_with_model("openai/gpt-4o-mini");
    let (agent_b, llm_b) = run_with_model("anthropic/claude-opus-4-5");

    agent_a
        .run("system prompt", "the original task")
        .await
        .expect("run a completes");
    agent_b
        .run("system prompt", "the original task")
        .await
        .expect("run b completes");

    assert_eq!(
        llm_a.requests(),
        llm_b.requests(),
        "Parity's assembled message sequences must be identical across models"
    );
}

/// A two-turn flow: assistant calls the tool, then stops with final text.
///
/// Why: This is the canonical happy path the loop exists to support.
/// What: Script [tool_call, stop]; run; assert the final content is the stop
/// text and the loop made exactly two chat calls.
/// Test: this test.
#[tokio::test]
async fn two_turn_flow_completes() {
    let llm = Arc::new(ScriptedLlm::from_json(&[
        tool_call_response("call_1", "world"),
        stop_response("Final answer: done"),
    ]));
    let registry = registry_with_echo(false);
    let agent = make_loop(llm.clone(), registry, AgentLoopConfig::default());

    let out = agent
        .run("You are helpful.", "Echo 'world' then conclude.")
        .await
        .expect("loop should complete");

    assert_eq!(out.content, "Final answer: done");
    assert_eq!(llm.calls(), 2, "loop should make exactly two chat calls");
}

/// A response carrying BOTH tool calls and `finish_reason == "stop"` still
/// dispatches the tool and continues — `stop` does not short-circuit.
///
/// Why: Per the D3 finish/tool-call precedence rule, completion is signalled
/// ONLY by a no-tool-call turn; a `stop` reason alongside pending tool calls
/// must not drop those calls. This guards against the prior `|| finished`
/// early-exit that silently discarded them.
/// What: Script [tool_call_with_stop_finish, stop]; run; assert the loop made
/// exactly TWO chat calls (proving it dispatched the tool and looped) and ended
/// on the second turn's final text, not the first turn's `stop`.
/// Test: this test.
#[tokio::test]
async fn stop_finish_with_tool_call_still_dispatches() {
    let llm = Arc::new(ScriptedLlm::from_json(&[
        tool_call_with_stop_finish("call_1", "world"),
        stop_response("Final answer: done"),
    ]));
    let registry = registry_with_echo(false);
    let agent = make_loop(llm.clone(), registry, AgentLoopConfig::default());

    let out = agent
        .run("You are helpful.", "Echo then conclude.")
        .await
        .expect("loop should dispatch the tool despite stop finish_reason");

    assert_eq!(
        out.content, "Final answer: done",
        "must continue past the stop-with-tool-call turn, not exit early"
    );
    assert_eq!(
        llm.calls(),
        2,
        "stop finish_reason must not short-circuit pending tool dispatch"
    );
}

/// Exhausting the turn cap aborts with a partial transcript, not a bare error.
///
/// Why: Non-converging tool loops must terminate with whatever was produced so
/// the caller can still inspect progress.
/// What: Script three identical tool-call responses but cap `max_turns` at 2;
/// assert `TurnCapExceeded` carrying a non-error partial output.
/// Test: this test.
#[tokio::test]
async fn turn_cap_returns_partial_transcript() {
    let llm = Arc::new(ScriptedLlm::from_json(&[
        tool_call_response("c1", "a"),
        tool_call_response("c2", "b"),
        tool_call_response("c3", "c"),
    ]));
    let registry = registry_with_echo(false);
    let config = AgentLoopConfig {
        max_turns: 2,
        ..AgentLoopConfig::default()
    };
    let agent = make_loop(llm.clone(), registry, config);

    let err = agent
        .run("system", "loop forever")
        .await
        .expect_err("should hit the turn cap");

    match err {
        AgentLoopError::TurnCapExceeded { max_turns, partial } => {
            assert_eq!(max_turns, 2);
            // Two turns ran → usage accrued from two chat calls.
            assert!(
                partial.usage.prompt_tokens > 0,
                "partial usage should accrue"
            );
        }
        other => panic!("expected TurnCapExceeded, got {other:?}"),
    }
    assert_eq!(llm.calls(), 2, "loop must stop calling at the cap");
}

/// The wall-clock timeout aborts the loop with a partial transcript (#2207).
///
/// Why: #2207 raised the default `timeout_secs` and made it configurable, but
/// the underlying mechanism — `AgentLoop::run` wrapping the whole loop body in
/// `tokio::time::timeout` — is otherwise unchanged; this pins that it still
/// fires, returns `AgentLoopError::Timeout` (not `TurnCapExceeded` or a
/// transport error), carries the configured `timeout_secs`, and preserves
/// whatever usage accrued before the deadline.
/// What: Configure a tiny `timeout_secs: 1`; the scripted client sleeps 3s
/// before responding. Assert the run errors with `Timeout { timeout_secs: 1,
/// .. }` well before the full 3s delay would have elapsed.
/// Test: this test.
#[tokio::test]
async fn timeout_returns_partial() {
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

    let llm: Arc<dyn LlmClientTrait> = Arc::new(SleepyLlm {
        inner: ScriptedLlm::from_json(&[stop_response("too slow")]),
        delay: std::time::Duration::from_secs(3),
    });
    let registry = registry_with_echo(false);
    let config = AgentLoopConfig {
        timeout_secs: 1,
        ..AgentLoopConfig::default()
    };
    let agent = AgentLoop::new(config, llm, registry);

    let started = std::time::Instant::now();
    let err = agent
        .run("system", "a task that takes too long")
        .await
        .expect_err("a 1s deadline against a 3s-delayed response must time out");
    let elapsed = started.elapsed();

    match err {
        AgentLoopError::Timeout {
            timeout_secs,
            partial,
        } => {
            assert_eq!(timeout_secs, 1, "must report the configured deadline");
            // No turn completed before the deadline (the single chat call
            // never returned), so usage is legitimately still zero here —
            // the assertion is on the ERROR VARIANT and its `timeout_secs`,
            // not on partial usage (that is covered end-to-end by
            // `run_task::tests::exit_code_reflects_deadline_exceeded_distinct_from_run_failure`,
            // where an earlier turn DOES complete before the deadline).
            let _ = partial;
        }
        other => panic!("expected Timeout, got {other:?}"),
    }
    assert!(
        elapsed < std::time::Duration::from_secs(3),
        "the 1s deadline must fire well before the mock's 3s delay, elapsed={elapsed:?}"
    );
}

/// A generous timeout does NOT prematurely abort a loop that finishes well
/// within budget (#2207).
///
/// Why: The companion regression guard to `timeout_returns_partial` — a
/// caller raising the deadline (e.g. for the M3 bake-off's L2/L3 multi-hour
/// tasks) must not have their run cut short by an unrelated bug in the
/// timeout wiring.
/// What: Configure a 5s timeout against a normal, instantly-responding
/// two-turn script; assert the run completes successfully.
/// Test: this test.
#[tokio::test]
async fn generous_timeout_does_not_abort_a_fast_run() {
    let llm = Arc::new(ScriptedLlm::from_json(&[
        tool_call_response("call_1", "world"),
        stop_response("done well within budget"),
    ]));
    let registry = registry_with_echo(false);
    let config = AgentLoopConfig {
        timeout_secs: 5,
        ..AgentLoopConfig::default()
    };
    let agent = make_loop(llm, registry, config);

    let output = agent
        .run("system", "quick task")
        .await
        .expect("a fast run under a generous deadline must not be aborted");
    assert_eq!(output.content, "done well within budget");
}

/// A recoverable tool error does not abort the loop; iteration continues.
///
/// Why: Tool failures are usually recoverable — the model should see the error
/// and decide. The loop must feed the error back and keep going.
/// What: Echo tool is configured to fail; script [tool_call, stop]; assert the
/// loop still completes with the stop text (proving it continued past the
/// failed tool result).
/// Test: this test.
#[tokio::test]
async fn recoverable_tool_error_continues() {
    let llm = Arc::new(ScriptedLlm::from_json(&[
        tool_call_response("call_1", "world"),
        stop_response("Recovered and concluded"),
    ]));
    let registry = registry_with_echo(true); // tool returns recoverable error
    let agent = make_loop(llm.clone(), registry, AgentLoopConfig::default());

    let out = agent
        .run("system", "try the failing tool")
        .await
        .expect("loop should continue past a recoverable tool error");

    assert_eq!(out.content, "Recovered and concluded");
    assert_eq!(
        llm.calls(),
        2,
        "loop should make two calls despite tool failure"
    );
}

/// Token usage accrues across every turn of the run.
///
/// Why: Cost tracking is a hard requirement; usage must sum across turns, not
/// just reflect the last one.
/// What: Script [tool_call(12+8), stop(7+5)]; assert the output's usage equals
/// the sum of both turns' prompt and completion tokens.
/// Test: this test.
#[tokio::test]
async fn usage_accrues_across_turns() {
    let llm = Arc::new(ScriptedLlm::from_json(&[
        tool_call_response("call_1", "x"),
        stop_response("done"),
    ]));
    let registry = registry_with_echo(false);
    let agent = make_loop(llm, registry, AgentLoopConfig::default());

    let out = agent.run("system", "task").await.expect("completes");

    // tool_call fixture: prompt 12, completion 8; stop fixture: prompt 7, completion 5.
    assert_eq!(out.usage.prompt_tokens, 12 + 7);
    assert_eq!(out.usage.completion_tokens, 8 + 5);
}

/// A run with no tools and an immediate stop returns the text directly.
///
/// Why: Not every task needs tools; the loop must short-circuit on the first
/// `stop` without requiring any registered tool.
/// What: Empty registry; script a single stop response; assert one call and the
/// final text.
/// Test: this test.
#[tokio::test]
async fn no_tools_immediate_stop() {
    let llm = Arc::new(ScriptedLlm::from_json(&[stop_response("just text")]));
    let registry = Arc::new(ToolRegistry::new());
    let agent = make_loop(llm.clone(), registry, AgentLoopConfig::default());

    let out = agent
        .run("system", "say something")
        .await
        .expect("completes");
    assert_eq!(out.content, "just text");
    assert_eq!(llm.calls(), 1);
}

// ── #2072: `finish_task` tool + repair loop ─────────────────────────────────────

/// A valid, explicit `finish_task` call terminates the loop immediately with
/// the structured completion report, WITHOUT waiting for a later
/// no-tool-call turn.
///
/// Why: This is the core #2072 acceptance criterion — an explicit finish call
/// is a first-class alternative to the implicit D3 no-tool-call convention.
/// What: Script a single `finish_task` response with valid `status`/`summary`;
/// assert the loop returns after exactly ONE chat call (proving it did not
/// wait for a second turn) with `AgentOutput.summary` and `.content` reflecting
/// the structured report.
/// Test: this test.
#[tokio::test]
async fn explicit_finish_task_terminates_loop_with_structured_summary() {
    let llm = Arc::new(ScriptedLlm::from_json(&[finish_task_call_response(
        "call-finish",
        r#"{"status": "completed", "summary": "implemented the feature"}"#,
    )]));
    let registry = registry_with_finish_task();
    let agent = make_loop(llm.clone(), registry, AgentLoopConfig::default());

    let out = agent
        .run("system", "do the task")
        .await
        .expect("explicit finish_task should terminate the loop");

    assert_eq!(llm.calls(), 1, "loop must not make a second chat call");
    assert_eq!(out.summary.as_deref(), Some("implemented the feature"));
    assert_eq!(
        out.content, "Task completed: implemented the feature",
        "content must reflect the structured report, not raw transcript text"
    );
}

/// A `finish_task` call with syntactically malformed JSON arguments is
/// reported as a recoverable error and repaired on the next turn, after which
/// the corrected call terminates the loop.
///
/// Why: This is the #2072 "malformed args → recoverable error → repair →
/// success" acceptance-criterion scenario, reusing #1023's
/// `ToolCallExtractor::parse_and_validate` — no bespoke validation exists in
/// `finish_task.rs` itself.
/// What: Script [malformed finish_task call, valid finish_task call]; assert
/// exactly two chat calls and a final structured summary from the SECOND
/// (corrected) call.
/// Test: this test.
#[tokio::test]
async fn malformed_finish_task_repairs_then_terminates() {
    let llm = Arc::new(ScriptedLlm::from_json(&[
        finish_task_call_response("call-bad", "{not valid json"),
        finish_task_call_response(
            "call-good",
            r#"{"status": "completed", "summary": "fixed and done"}"#,
        ),
    ]));
    let registry = registry_with_finish_task();
    let agent = make_loop(llm.clone(), registry, AgentLoopConfig::default());

    let out = agent
        .run("system", "do the task")
        .await
        .expect("loop should repair and terminate on the corrected call");

    assert_eq!(
        llm.calls(),
        2,
        "one malformed attempt, one repaired attempt"
    );
    assert_eq!(out.summary.as_deref(), Some("fixed and done"));
    assert_eq!(out.content, "Task completed: fixed and done");
}

/// A `finish_task` call missing the required `summary` field is reported as a
/// recoverable schema violation and does NOT terminate the loop — the run
/// continues to whatever comes next (here, a plain no-tool-call finish).
///
/// Why: #2072's "missing required field → recoverable error (no panic)"
/// acceptance criterion. Also guards that an invalid finish_task call is never
/// mistaken for a successful one (`dispatch_all` only treats a NON-error
/// dispatch as a finish signal).
/// What: Script [finish_task missing `summary`, stop response]; assert the run
/// completes via the D3 fallback, not the finish_task path, after two chat
/// calls.
/// Test: this test.
#[tokio::test]
async fn finish_task_missing_required_field_is_recoverable_not_terminal() {
    let llm = Arc::new(ScriptedLlm::from_json(&[
        finish_task_call_response("call-missing", r#"{"status": "completed"}"#),
        stop_response("recovered from missing summary"),
    ]));
    let registry = registry_with_finish_task();
    let agent = make_loop(llm.clone(), registry, AgentLoopConfig::default());

    let out = agent
        .run("system", "do the task")
        .await
        .expect("missing required field must not abort the loop");

    assert_eq!(llm.calls(), 2);
    assert_eq!(out.content, "recovered from missing summary");
    assert_eq!(
        out.summary, None,
        "the D3 fallback path must not set a finish_task summary"
    );
}

/// A `finish_task` call with a `status` value outside the declared enum is
/// reported as a recoverable schema violation, not a panic, and does not
/// terminate the loop.
///
/// Why: #2072's "schema-invalid enum value → recoverable error" acceptance
/// criterion.
/// What: Script [finish_task with `status: "in_progress"`, stop response];
/// assert the run recovers via the D3 fallback after two chat calls.
/// Test: this test.
#[tokio::test]
async fn finish_task_invalid_enum_value_is_recoverable_not_terminal() {
    let llm = Arc::new(ScriptedLlm::from_json(&[
        finish_task_call_response(
            "call-bad-enum",
            r#"{"status": "in_progress", "summary": "still working"}"#,
        ),
        stop_response("recovered from bad enum value"),
    ]));
    let registry = registry_with_finish_task();
    let agent = make_loop(llm.clone(), registry, AgentLoopConfig::default());

    let out = agent
        .run("system", "do the task")
        .await
        .expect("invalid enum value must not abort the loop");

    assert_eq!(llm.calls(), 2);
    assert_eq!(out.content, "recovered from bad enum value");
}

/// A `finish_task` call carrying `changes` and test-count fields propagates
/// all of them into the final rendered content.
///
/// Why: Guards the full-shape structured report end to end through the loop,
/// not just the tool's own unit tests.
/// What: Script a `finish_task` call with `changes` + `tests_run`/
/// `tests_passed`; assert the rendered content contains every section.
/// Test: this test.
#[tokio::test]
async fn finish_task_full_shape_propagates_into_output() {
    let llm = Arc::new(ScriptedLlm::from_json(&[finish_task_call_response(
        "call-full",
        r#"{"status": "completed", "summary": "shipped it", "changes": [{"file": "a.rs", "lines_added": 10, "lines_removed": 2}], "tests_run": 4, "tests_passed": 4}"#,
    )]));
    let registry = registry_with_finish_task();
    let agent = make_loop(llm.clone(), registry, AgentLoopConfig::default());

    let out = agent
        .run("system", "do the task")
        .await
        .expect("full-shape finish_task should terminate the loop");

    assert!(out.content.contains("Task completed: shipped it"));
    assert!(out.content.contains("a.rs (+10/-2)"));
    assert!(out.content.contains("Tests: 4/4 passed"));
}

// #2279 verify-before-finish gate intercept tests (block → nudge → retry)
// live in a focused child module to keep this file under its SLOC cap.
mod gate_intercept;

/// Live OpenRouter test: trivial task through the real client + a real tool.
///
/// Why: End-to-end confidence that the loop drives a real model to a final
/// answer. Gated on `OPENROUTER_API_KEY` so CI stays offline-green.
/// What: Build a real `OpenAiCompatClient` (shared-adapter transport, #2406),
/// register the echo tool, and ask the model to reply with a short word; assert
/// a non-empty final answer and that usage accrued.
/// Test: `cargo test -p trusty-code -- --include-ignored agent_loop_live`.
#[tokio::test]
#[ignore = "requires OPENROUTER_API_KEY; skipped in CI"]
async fn agent_loop_live() {
    use crate::llm::OpenAiCompatClient;

    let Ok(key) = std::env::var("OPENROUTER_API_KEY") else {
        eprintln!("OPENROUTER_API_KEY not set — skipping live agent-loop test");
        return;
    };
    if key.is_empty() {
        eprintln!("OPENROUTER_API_KEY empty — skipping live agent-loop test");
        return;
    }

    // Construction is credential-free; the shared resolver reads
    // `OPENROUTER_API_KEY` (confirmed present above) at first use.
    let client = OpenAiCompatClient::new();

    let registry = registry_with_echo(false);
    let agent = AgentLoop::new(
        AgentLoopConfig {
            max_turns: 4,
            timeout_secs: 60,
            model: "openai/gpt-4o-mini".to_string(),
            mode: crate::mode::HarnessMode::default(),
            ..AgentLoopConfig::default()
        },
        Arc::new(client),
        registry,
    );

    let out = agent
        .run(
            "You are a concise assistant.",
            "Reply with exactly the word: pong",
        )
        .await
        .expect("live loop should complete");

    assert!(!out.content.is_empty(), "final answer should be non-empty");
    assert!(
        out.usage.prompt_tokens > 0,
        "usage should accrue on a live call"
    );
    eprintln!(
        "live agent-loop output: {:?} usage={:?}",
        out.content, out.usage
    );
}

// ── #2056: ToolEventSink + cancellation ─────────────────────────────────────────

/// A `ToolEventSink` that records every call as a tagged string, in order.
///
/// Why: The sink's whole purpose is call-order + argument fidelity; recording
/// each hook as `"started:name"` / `"finished:name:success"` / `"error:name"`
/// lets a test assert the exact sequence with one `Vec<String>` comparison.
struct RecordingSink {
    calls: Mutex<Vec<String>>,
}

impl RecordingSink {
    fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("lock poisoned").clone()
    }
}

#[async_trait]
impl ToolEventSink for RecordingSink {
    async fn tool_started(&self, call_id: &str, tool: &str, _args_preview: &str) {
        self.calls
            .lock()
            .expect("lock poisoned")
            .push(format!("started:{tool}:{call_id}"));
    }

    async fn tool_finished(&self, call_id: &str, tool: &str, success: bool, _result_preview: &str) {
        self.calls
            .lock()
            .expect("lock poisoned")
            .push(format!("finished:{tool}:{call_id}:{success}"));
    }

    async fn tool_error(&self, call_id: &str, tool: &str, _error: &str) {
        self.calls
            .lock()
            .expect("lock poisoned")
            .push(format!("error:{tool}:{call_id}"));
    }
}

/// A sink must observe `tool_started` then `tool_finished(success=true)`, in
/// that order, for a successful dispatch.
///
/// Why: This is the exact sequence #2056's daemon-driven task execution relies
/// on to stream live `tool_started`/`tool_finished` events to an attached
/// client — a regression here would silently break that observability.
/// What: Script [tool_call, stop]; attach a `RecordingSink`; assert its call
/// log is exactly `["started:echo:call-1", "finished:echo:call-1:true"]`.
/// Test: this test.
#[tokio::test]
async fn sink_receives_started_then_finished_in_order() {
    let llm = Arc::new(ScriptedLlm::from_json(&[
        tool_call_response("call-1", "hi"),
        stop_response("done"),
    ]));
    let registry = registry_with_echo(false);
    let sink = Arc::new(RecordingSink::new());

    let agent =
        make_loop(llm, registry, AgentLoopConfig::default()).with_tool_event_sink(sink.clone());
    agent
        .run("sys", "task")
        .await
        .expect("loop should complete");

    assert_eq!(
        sink.calls(),
        vec!["started:echo:call-1", "finished:echo:call-1:true"]
    );
}

/// A recoverable `ToolResult::Error` must notify `tool_finished(success=false)`,
/// NOT `tool_error` — only a fatal/non-recoverable error is exceptional.
///
/// Why: #2055's taxonomy reserves `tool_error` for exceptional failures (tool
/// crash/timeout); an ordinary recoverable tool error (the model can retry) is
/// still a "the tool finished, unsuccessfully" event, not an exceptional one.
/// What: Script a tool call against the failing echo tool (`EchoTool { fail:
/// true }`, which returns `ToolResult::err` — recoverable); assert the sink saw
/// `finished:...:false`, never `error:...`.
/// Test: this test.
#[tokio::test]
async fn sink_recoverable_error_is_finished_not_error() {
    let llm = Arc::new(ScriptedLlm::from_json(&[
        tool_call_response("call-1", "hi"),
        stop_response("done"),
    ]));
    let registry = registry_with_echo(true);
    let sink = Arc::new(RecordingSink::new());

    let agent =
        make_loop(llm, registry, AgentLoopConfig::default()).with_tool_event_sink(sink.clone());
    agent
        .run("sys", "task")
        .await
        .expect("loop should complete");

    assert_eq!(
        sink.calls(),
        vec!["started:echo:call-1", "finished:echo:call-1:false"]
    );
}

/// A set cancellation flag must abort the loop with `AgentLoopError::Cancelled`
/// before the next turn's LLM call — never mid-tool-call.
///
/// Why: #2056's `session.cancel` on an in-flight daemon-driven run relies on
/// exactly this behaviour to stop cooperatively.
/// What: A flag pre-set to `true` before the loop even starts must abort on
/// the very first turn boundary, making zero `chat` calls.
/// Test: this test.
#[tokio::test]
async fn cancel_flag_aborts_before_next_turn() {
    let llm = Arc::new(ScriptedLlm::from_json(&[stop_response(
        "should never be reached",
    )]));
    let registry = registry_with_echo(false);
    let cancel = Arc::new(AtomicBool::new(true));

    let agent =
        make_loop(llm.clone(), registry, AgentLoopConfig::default()).with_cancel_flag(cancel);

    let err = agent
        .run("sys", "task")
        .await
        .expect_err("must abort as cancelled");
    assert!(matches!(err, AgentLoopError::Cancelled { .. }));
    assert_eq!(
        llm.calls(),
        0,
        "cancellation must be observed before any chat call"
    );
}

/// An external stop signal (#2265 fix #5) that reports `true` must abort the
/// loop with `AgentLoopError::StoppedBySignal` before the next turn's LLM
/// call — never mid-tool-call. Mirrors `cancel_flag_aborts_before_next_turn`
/// exactly, proving `with_stop_signal` shares the same turn-boundary contract
/// as `with_cancel_flag`.
///
/// Why: This is the generic mechanism `run_task`'s PM loop relies on to stop
/// issuing `delegate_to_agent` calls the turn after the shared re-delegation
/// cap latches (see `run_task::mod`'s `execute_run_task` wiring and
/// `run_task::tests::pm_stops_redelegating_once_cap_latched_ends_partial_promptly`
/// for the production end-to-end proof); this test isolates the loop
/// mechanism itself from any `run_task`-specific signal.
/// What: A closure pre-set to always return `true` before the loop even
/// starts must abort on the very first turn boundary, making zero `chat`
/// calls.
/// Test: this test.
#[tokio::test]
async fn stop_signal_aborts_before_next_turn() {
    let llm = Arc::new(ScriptedLlm::from_json(&[stop_response(
        "should never be reached",
    )]));
    let registry = registry_with_echo(false);

    let agent = make_loop(llm.clone(), registry, AgentLoopConfig::default())
        .with_stop_signal(Arc::new(|| true));

    let err = agent
        .run("sys", "task")
        .await
        .expect_err("must abort as stopped-by-signal");
    assert!(matches!(err, AgentLoopError::StoppedBySignal { .. }));
    assert_eq!(
        llm.calls(),
        0,
        "the stop signal must be observed before any chat call"
    );
}

// ── Prompt-caching gate tests (#2156) ────────────────────────────────────────────

/// A `DailyDriver` run against an `anthropic/*` model marks the static
/// tools+system prefix with an ephemeral prompt-cache breakpoint.
///
/// Why: This is the make-or-break cost lever #2156 exists for — the bake-off
/// L1 pilot showed tcode doing ZERO caching, re-billing the full tools+system
/// prefix every turn. This test proves the wire request the loop actually
/// sends carries the breakpoint in both required places: the last tool
/// definition's `function.cache_control`, and the system message's
/// `cache_control`.
/// What: Run a single-turn `DailyDriver` loop with model
/// `"anthropic/claude-sonnet-4-5"` and the echo tool registered; inspect the
/// `ScriptedLlm`'s recorded request for both markers.
/// Test: this test.
#[tokio::test]
async fn daily_driver_anthropic_model_marks_cache_breakpoints() {
    let llm = Arc::new(ScriptedLlm::from_json(&[stop_response("done")]));
    let registry = registry_with_echo(false);
    let agent = make_loop(
        llm.clone(),
        registry,
        AgentLoopConfig {
            model: "anthropic/claude-sonnet-4-5".into(),
            mode: crate::mode::HarnessMode::DailyDriver,
            ..AgentLoopConfig::default()
        },
    );

    agent
        .run("you are a helpful assistant", "do the thing")
        .await
        .expect("single stop turn should complete");

    let requests = llm.requests();
    let system_msg = requests[0]
        .iter()
        .find(|m| m.role == "system")
        .expect("system message present");
    assert_eq!(
        system_msg.cache_control,
        Some(crate::llm::CacheControl::ephemeral()),
        "system message must carry the cache breakpoint"
    );

    let tools = llm.tools_seen()[0]
        .clone()
        .expect("tools array present (echo tool registered)");
    let last_tool = tools.last().expect("at least one tool");
    assert_eq!(
        last_tool.function.cache_control,
        Some(crate::llm::CacheControl::ephemeral()),
        "last tool definition must carry the cache breakpoint"
    );
}

/// A `DailyDriver` run against a `bedrock/*` model marks the static
/// tools+system prefix AND the rolling history (last two non-system
/// messages) with an ephemeral prompt-cache breakpoint (#2260 +
/// rolling-history follow-up).
///
/// Why: `BedrockProvider::supports_prompt_caching` now returns `true`, so
/// `AgentLoop::prompt_cache_enabled` must treat a Bedrock route the same as
/// an `anthropic/*` OpenRouter slug — this is the `agent_loop`-level half of
/// #2260 (the Converse-transport half, translating this marker into a
/// native `cachePoint` block, is covered by
/// `llm::bedrock::tests::build_converse_messages_emits_cache_point_after_large_cached_system`
/// and its tool-config sibling). A live cost-proof of #2260 as-merged-so-far
/// showed cache_read staying 0 because nothing ever marked the growing
/// transcript — this test pins that the last two non-system messages of a
/// multi-turn request now carry the breakpoint too, while an OLDER
/// non-system message (outside the rolling window) does not.
/// What: Scripts a two-turn flow (tool call, then stop) so the second
/// `ChatRequest` carries four messages: system, user, assistant(tool_calls),
/// tool(result). Assert the system message and the last tool definition
/// carry the breakpoint (unchanged #2260 coverage), AND that the assistant
/// and tool messages (the last two non-system entries) carry it, AND that
/// the user message (now outside the rolling window) does not.
/// Test: this test.
#[tokio::test]
async fn daily_driver_bedrock_model_marks_cache_breakpoints() {
    let llm = Arc::new(ScriptedLlm::from_json(&[
        tool_call_response("call_1", "world"),
        stop_response("done"),
    ]));
    let registry = registry_with_echo(false);
    let agent = make_loop(
        llm.clone(),
        registry,
        AgentLoopConfig {
            model: "bedrock/us.anthropic.claude-sonnet-4-5".into(),
            mode: crate::mode::HarnessMode::DailyDriver,
            ..AgentLoopConfig::default()
        },
    );

    agent
        .run("you are a helpful assistant", "do the thing")
        .await
        .expect("two-turn flow should complete");

    let requests = llm.requests();
    let system_msg = requests[0]
        .iter()
        .find(|m| m.role == "system")
        .expect("system message present");
    assert_eq!(
        system_msg.cache_control,
        Some(crate::llm::CacheControl::ephemeral()),
        "system message must carry the cache breakpoint for a Bedrock route"
    );

    let tools = llm.tools_seen()[0]
        .clone()
        .expect("tools array present (echo tool registered)");
    let last_tool = tools.last().expect("at least one tool");
    assert_eq!(
        last_tool.function.cache_control,
        Some(crate::llm::CacheControl::ephemeral()),
        "last tool definition must carry the cache breakpoint for a Bedrock route"
    );

    // Second request: system, user, assistant(tool_calls), tool(result).
    let second_request = &requests[1];
    let non_system: Vec<&crate::llm::ChatMessage> = second_request
        .iter()
        .filter(|m| m.role != "system")
        .collect();
    assert_eq!(
        non_system.len(),
        3,
        "expected user, assistant, and tool messages on the second turn"
    );
    assert!(
        non_system[0].cache_control.is_none(),
        "the oldest non-system message must NOT carry the breakpoint once it \
         rolls outside the last-two window"
    );
    assert_eq!(
        non_system[1].cache_control,
        Some(crate::llm::CacheControl::ephemeral()),
        "the assistant message (second-to-last) must carry the rolling history breakpoint"
    );
    assert_eq!(
        non_system[2].cache_control,
        Some(crate::llm::CacheControl::ephemeral()),
        "the tool-result message (last) must carry the rolling history breakpoint"
    );
}

/// A goal-slot injection (#2347) at position `[1]` does NOT disturb
/// `mark_cache_breakpoint_on_history`'s last-two-non-system-messages logic,
/// and the real system message's bytes stay untouched by the goal write.
///
/// Why: This is #2347's explicit regression requirement — the goals block is
/// system-role, so it must be excluded from
/// `mark_cache_breakpoint_on_history`'s "last two NON-system messages"
/// selection entirely; if it were accidentally counted, a goal update could
/// shift which real messages get the rolling-history cache breakpoint.
/// What: Mirrors `daily_driver_bedrock_model_marks_cache_breakpoints`'s
/// two-turn Bedrock scenario, but drives it via `run_with_transcript` over a
/// `Transcript` that already has a goal set through `goals_handle` before
/// the run starts. Asserts: (1) the system message at request index `[0]`
/// carries the SAME content as an unmodified seed (proving the goal write
/// left it untouched), (2) the goals block appears at index `[1]` and is
/// excluded from the "last two non-system" set, and (3) the assistant and
/// tool-result messages (the real last-two-non-system entries) still carry
/// the rolling-history breakpoint exactly as they do without any goal set.
/// Test: this test.
#[tokio::test]
async fn daily_driver_goal_slot_injection_does_not_disturb_cache_breakpoints() {
    let mut transcript = Transcript::seed("you are a helpful assistant", "do the thing");
    transcript
        .goals_handle()
        .lock()
        .expect("lock")
        .set(1, "keep shipping", super::GoalSource::Model)
        .expect("valid slot");

    let llm = Arc::new(ScriptedLlm::from_json(&[
        tool_call_response("call_1", "world"),
        stop_response("done"),
    ]));
    let registry = registry_with_echo(false);
    let agent = make_loop(
        llm.clone(),
        registry,
        AgentLoopConfig {
            model: "bedrock/us.anthropic.claude-sonnet-4-5".into(),
            mode: crate::mode::HarnessMode::DailyDriver,
            ..AgentLoopConfig::default()
        },
    );

    agent
        .run_with_transcript(&mut transcript, "do the thing")
        .await
        .expect("two-turn flow should complete");

    let requests = llm.requests();

    // First request: system, goals, user.
    let first_request = &requests[0];
    assert_eq!(first_request[0].role, "system");
    assert_eq!(
        first_request[0].content.as_deref(),
        Some("you are a helpful assistant"),
        "the real system message must be byte-identical to the unmodified seed"
    );
    assert_eq!(
        first_request[0].cache_control,
        Some(crate::llm::CacheControl::ephemeral()),
        "the real system message must still carry its own cache breakpoint"
    );
    assert_eq!(
        first_request[1].role, "system",
        "the goals block is injected at position [1]"
    );
    assert!(
        first_request[1]
            .content
            .as_deref()
            .unwrap_or("")
            .contains("keep shipping")
    );
    assert!(
        first_request[1].cache_control.is_none(),
        "the goals block itself is not part of the static tools+system cache prefix"
    );

    // Second request: system, goals, user, assistant(tool_calls), tool(result).
    let second_request = &requests[1];
    let non_system: Vec<&crate::llm::ChatMessage> = second_request
        .iter()
        .filter(|m| m.role != "system")
        .collect();
    assert_eq!(
        non_system.len(),
        3,
        "the goals block (system-role) must be excluded from the non-system \
         set entirely, leaving exactly user/assistant/tool: {second_request:?}"
    );
    assert!(
        non_system[0].cache_control.is_none(),
        "the oldest non-system message (user) must not carry the rolling breakpoint"
    );
    assert_eq!(
        non_system[1].cache_control,
        Some(crate::llm::CacheControl::ephemeral()),
        "the assistant message must still carry the rolling history breakpoint \
         with the goals block present"
    );
    assert_eq!(
        non_system[2].cache_control,
        Some(crate::llm::CacheControl::ephemeral()),
        "the tool-result message must still carry the rolling history breakpoint \
         with the goals block present"
    );
}

/// `Parity` mode never marks a cache breakpoint, even for an `anthropic/*`
/// model — the request stays byte-identical to pre-#2156.
///
/// Why: #2156's spec scopes caching to `DailyDriver`; Parity's cross-model
/// benchmark fairness guarantee must not have its wire payload altered.
/// What: Same setup as the DailyDriver test but with `mode:
/// HarnessMode::Parity`; assert neither the system message nor the tool
/// definition carries `cache_control`.
/// Test: this test.
#[tokio::test]
async fn parity_mode_never_marks_cache_breakpoints() {
    let llm = Arc::new(ScriptedLlm::from_json(&[stop_response("done")]));
    let registry = registry_with_echo(false);
    let agent = make_loop(
        llm.clone(),
        registry,
        AgentLoopConfig {
            model: "anthropic/claude-sonnet-4-5".into(),
            mode: crate::mode::HarnessMode::Parity,
            ..AgentLoopConfig::default()
        },
    );

    agent
        .run("you are a helpful assistant", "do the thing")
        .await
        .expect("single stop turn should complete");

    let requests = llm.requests();
    let system_msg = requests[0]
        .iter()
        .find(|m| m.role == "system")
        .expect("system message present");
    assert!(
        system_msg.cache_control.is_none(),
        "Parity mode must never carry a cache breakpoint on the system message"
    );

    let tools = llm.tools_seen()[0]
        .clone()
        .expect("tools array present (echo tool registered)");
    let last_tool = tools.last().expect("at least one tool");
    assert!(
        last_tool.function.cache_control.is_none(),
        "Parity mode must never carry a cache breakpoint on the tools array"
    );
}

/// A `DailyDriver` run against a non-Anthropic model never marks a cache
/// breakpoint — the passthrough is only verified for `anthropic/*` slugs.
///
/// Why: Emitting `cache_control` to a family whose OpenRouter passthrough
/// hasn't been verified risks silently malformed requests; the gate must key
/// off the resolved `Provider::supports_prompt_caching`, not the mode alone.
/// What: Same setup as the DailyDriver test but with model
/// `"openai/gpt-4o-mini"`; assert neither the system message nor the tool
/// definition carries `cache_control`.
/// Test: this test.
#[tokio::test]
async fn non_anthropic_model_never_marks_cache_breakpoints() {
    let llm = Arc::new(ScriptedLlm::from_json(&[stop_response("done")]));
    let registry = registry_with_echo(false);
    let agent = make_loop(
        llm.clone(),
        registry,
        AgentLoopConfig {
            model: "openai/gpt-4o-mini".into(),
            mode: crate::mode::HarnessMode::DailyDriver,
            ..AgentLoopConfig::default()
        },
    );

    agent
        .run("you are a helpful assistant", "do the thing")
        .await
        .expect("single stop turn should complete");

    let requests = llm.requests();
    let system_msg = requests[0]
        .iter()
        .find(|m| m.role == "system")
        .expect("system message present");
    assert!(
        system_msg.cache_control.is_none(),
        "non-Anthropic models must never carry a cache breakpoint on the system message"
    );

    let tools = llm.tools_seen()[0]
        .clone()
        .expect("tools array present (echo tool registered)");
    let last_tool = tools.last().expect("at least one tool");
    assert!(
        last_tool.function.cache_control.is_none(),
        "non-Anthropic models must never carry a cache breakpoint on the tools array"
    );
}

/// `build_request` attaches OpenRouter's detailed-usage directive
/// (`RequestUsageConfig::detailed`) for an OpenRouter-routed model, but never
/// for a Bedrock-routed model (response-side cache-usage fix).
///
/// Why: This is the request-side half of the fix that makes OpenRouter return
/// its authoritative `usage.cost` and cache-token breakdown — without it,
/// only the bare prompt/completion/total counts are guaranteed. It must stay
/// gated per-provider so the direct/Bedrock path never receives a directive
/// it doesn't understand.
/// What: Run the loop once against `"anthropic/claude-sonnet-4-5"` (routes to
/// OpenRouter) and once against `"bedrock/us.anthropic.claude-sonnet-4-5"`
/// (routes to Bedrock); assert the recorded `usage` directive is
/// `Some(RequestUsageConfig::detailed())` for the former and `None` for the
/// latter.
/// Test: this test.
#[tokio::test]
async fn build_request_sets_detailed_usage_for_openrouter() {
    let or_llm = Arc::new(ScriptedLlm::from_json(&[stop_response("done")]));
    let or_agent = make_loop(
        or_llm.clone(),
        registry_with_echo(false),
        AgentLoopConfig {
            model: "anthropic/claude-sonnet-4-5".into(),
            ..AgentLoopConfig::default()
        },
    );
    or_agent
        .run("you are a helpful assistant", "do the thing")
        .await
        .expect("single stop turn should complete");
    assert_eq!(
        or_llm.usage_seen()[0],
        Some(crate::llm::RequestUsageConfig::detailed()),
        "OpenRouter-routed requests must carry the detailed-usage directive"
    );

    let bedrock_llm = Arc::new(ScriptedLlm::from_json(&[stop_response("done")]));
    let bedrock_agent = make_loop(
        bedrock_llm.clone(),
        registry_with_echo(false),
        AgentLoopConfig {
            model: "bedrock/us.anthropic.claude-sonnet-4-5".into(),
            ..AgentLoopConfig::default()
        },
    );
    bedrock_agent
        .run("you are a helpful assistant", "do the thing")
        .await
        .expect("single stop turn should complete");
    assert_eq!(
        bedrock_llm.usage_seen()[0],
        None,
        "Bedrock-routed requests must never carry the OpenRouter-only detailed-usage directive"
    );
}

// ── #2344: persistent-session `run_with_transcript` ─────────────────────────────

/// Build a transcript that looks like a session's FIRST `task.run` already
/// completed: seeded, then one assistant text turn appended (the "run one"
/// answer), mirroring what `SessionRegistry::begin_pm_transcript` +
/// `AgentLoop::run_with_transcript` leave behind after a real run.
fn transcript_after_one_completed_run() -> Transcript {
    let mut t = Transcript::seed("system prompt", "first task");
    t.push_assistant(Some("run one's answer".into()), &[]);
    t
}

/// A second `run_with_transcript` call on an already-seeded transcript must
/// NOT add a second system message — the original seed's system message
/// stays authoritative across runs.
///
/// Why: This is #2344's explicit "system prompt on subsequent runs"
/// contract — re-seeding on every run would duplicate the system message and
/// waste tokens/confuse the model.
/// What: Build a post-first-run transcript, append the second task as a user
/// turn (mirroring `SessionRegistry::begin_pm_transcript`'s own append), run
/// the loop again, and assert exactly ONE `system`-role message across the
/// whole raw history.
/// Test: this test.
#[tokio::test]
async fn run_with_transcript_does_not_reseed_system_message() {
    let mut transcript = transcript_after_one_completed_run();
    transcript.push_user("second task");

    let llm = Arc::new(ScriptedLlm::from_json(&[stop_response("run two's answer")]));
    let agent = make_loop(
        llm.clone(),
        registry_with_echo(false),
        AgentLoopConfig::default(),
    );

    agent
        .run_with_transcript(&mut transcript, "second task")
        .await
        .expect("second run should complete");

    let messages = transcript.messages();
    assert_eq!(
        messages.iter().filter(|m| m.role == "system").count(),
        1,
        "a continued run must never add a second system message: {messages:?}"
    );
    assert_eq!(messages[0].role, "system");
    assert_eq!(messages[0].content.as_deref(), Some("system prompt"));
}

/// `run_with_transcript`'s reported `AgentOutput.content` is scoped to only
/// the NEW turns this call produced, even though the underlying transcript
/// keeps growing across runs.
///
/// Why: Without this scoping, a session's second `task.run` response would
/// read as the first run's answer immediately followed by the second's —
/// duplicated, ever-growing prose on every subsequent call. #2344's design
/// deliberately keeps the CONVERSATION cumulative while keeping each run's
/// OWN reported answer scoped to itself.
/// What: Drive a second run on `transcript_after_one_completed_run()`;
/// assert the returned `content` is EXACTLY the second run's text (not
/// prefixed by "run one's answer"), while the transcript's own
/// `assistant_text()` still contains both.
/// Test: this test.
#[tokio::test]
async fn run_with_transcript_scopes_output_to_new_turns() {
    let mut transcript = transcript_after_one_completed_run();
    transcript.push_user("second task");

    let llm = Arc::new(ScriptedLlm::from_json(&[stop_response("run two's answer")]));
    let agent = make_loop(
        llm.clone(),
        registry_with_echo(false),
        AgentLoopConfig::default(),
    );

    let out = agent
        .run_with_transcript(&mut transcript, "second task")
        .await
        .expect("second run should complete");

    assert_eq!(
        out.content, "run two's answer",
        "the reported output must be scoped to just this run's new turns"
    );
    assert_eq!(
        transcript.assistant_text(),
        "run one's answer\n\nrun two's answer",
        "the underlying transcript must still hold BOTH runs' assistant text"
    );
}

/// The output-scoping rule also applies to a partial-abort outcome (turn cap
/// exceeded), not just the success path.
///
/// Why: `AgentLoopError::partial_output_mut` is what makes this uniform —
/// this test pins that the turn-cap abort variant is actually covered, not
/// just the happy path.
/// What: Cap `max_turns` at 1 and script only tool-call responses (no text)
/// for the second run; assert the `TurnCapExceeded` partial's `content` does
/// NOT contain "run one's answer" (i.e. it was scoped, not left as the whole
/// transcript's joined text — which would be empty here anyway since run two
/// never produced text, but a pre-#2344 bug would still surface as a
/// non-empty `content` carrying run one's leftover text).
/// Test: this test.
#[tokio::test]
async fn run_with_transcript_scopes_partial_output_on_turn_cap() {
    let mut transcript = transcript_after_one_completed_run();
    transcript.push_user("second task");

    let llm = Arc::new(ScriptedLlm::from_json(&[
        tool_call_response("c1", "a"),
        tool_call_response("c2", "b"),
    ]));
    let agent = make_loop(
        llm.clone(),
        registry_with_echo(false),
        AgentLoopConfig {
            max_turns: 1,
            ..AgentLoopConfig::default()
        },
    );

    let err = agent
        .run_with_transcript(&mut transcript, "second task")
        .await
        .expect_err("turn cap should abort the second run");

    let AgentLoopError::TurnCapExceeded { partial, .. } = err else {
        panic!("expected TurnCapExceeded, got {err:?}");
    };
    assert!(
        !partial.content.contains("run one's answer"),
        "the partial output must be scoped to this run's new turns, got: {:?}",
        partial.content
    );
}

/// An explicit `finish_task` completion on a continued run must keep its
/// structured summary — the output-scoping rewrite must NOT clobber it.
///
/// Why: `build_finish_output` deliberately overwrites `content` with a
/// structured render and sets `summary`; `scope_output_to_new_turns` must
/// detect that (`summary.is_some()`) and leave it alone, or every
/// persistent-session run that finishes via `finish_task` would lose its
/// structured completion report.
/// What: Drive a second run that finishes via `finish_task`; assert
/// `out.summary` and the structured `content` survive unchanged.
/// Test: this test.
#[tokio::test]
async fn run_with_transcript_does_not_clobber_finish_task_summary() {
    let mut transcript = transcript_after_one_completed_run();
    transcript.push_user("second task");

    let llm = Arc::new(ScriptedLlm::from_json(&[finish_task_call_response(
        "call-finish",
        r#"{"status": "completed", "summary": "run two done"}"#,
    )]));
    let agent = make_loop(
        llm.clone(),
        registry_with_finish_task(),
        AgentLoopConfig::default(),
    );

    let out = agent
        .run_with_transcript(&mut transcript, "second task")
        .await
        .expect("finish_task should terminate the loop");

    assert_eq!(out.summary.as_deref(), Some("run two done"));
    assert!(
        out.content.contains("run two done"),
        "the structured finish_task summary must survive the output-scoping rewrite: {:?}",
        out.content
    );
    assert!(
        !out.content.contains("run one's answer"),
        "a finish_task completion must not contain run one's leftover text either: {:?}",
        out.content
    );
}
