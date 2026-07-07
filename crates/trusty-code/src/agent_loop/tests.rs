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

use super::{AgentLoop, AgentLoopConfig, AgentLoopError, CompactionConfig, ToolEventSink};
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
}

#[async_trait]
impl LlmClientTrait for ScriptedLlm {
    async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse, LlmError> {
        if let Ok(mut guard) = self.requests.lock() {
            guard.push(req.messages.clone());
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

// ── Tests ───────────────────────────────────────────────────────────────────────

/// Config defaults are present and sane.
///
/// Why: Defaults are the most-used construction path; guard them.
/// What: Assert `max_turns`, `timeout_secs`, and a non-empty model.
/// Test: this test.
#[test]
fn config_defaults_are_sane() {
    let cfg = AgentLoopConfig::default();
    assert!(cfg.max_turns >= 1);
    assert!(cfg.timeout_secs >= 1);
    assert!(!cfg.model.is_empty());
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

/// Live OpenRouter test: trivial task through the real client + a real tool.
///
/// Why: End-to-end confidence that the loop drives a real model to a final
/// answer. Gated on `OPENROUTER_API_KEY` so CI stays offline-green.
/// What: Build a real `LlmClient`, register the echo tool, and ask the model to
/// reply with a short word; assert a non-empty final answer and that usage
/// accrued.
/// Test: `cargo test -p trusty-code -- --include-ignored agent_loop_live`.
#[tokio::test]
#[ignore = "requires OPENROUTER_API_KEY; skipped in CI"]
async fn agent_loop_live() {
    use crate::llm::{LlmClient, LlmClientConfig};

    let Ok(key) = std::env::var("OPENROUTER_API_KEY") else {
        eprintln!("OPENROUTER_API_KEY not set — skipping live agent-loop test");
        return;
    };
    if key.is_empty() {
        eprintln!("OPENROUTER_API_KEY empty — skipping live agent-loop test");
        return;
    }

    let client = LlmClient::from_config(
        LlmClientConfig::new(key)
            .expect("config")
            .with_title("trusty-code-agent-loop-test"),
    )
    .expect("client");

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
