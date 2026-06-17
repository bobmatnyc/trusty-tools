//! Multi-turn agent loop: drive an LLM through tool calls until it stops.
//!
//! Why: A single chat call cannot accomplish a task that requires reading files,
//! running tools, and reacting to results. The agent loop is the control
//! structure that repeatedly calls the model, dispatches any requested tool
//! calls through the gated registry, feeds results back, and iterates until the
//! model produces a final answer — bounded by a turn cap, a wall-clock timeout,
//! and token-usage accounting so runs stay cheap and observable.
//! What: Exposes `AgentLoop`, `AgentLoopConfig`, and `AgentLoopError`. `run`
//! seeds a `Transcript`, then loops: build a `ChatRequest` from the running
//! history + registry schemas, call `LlmClientTrait::chat`, accrue usage into a
//! `PerfCollector`, append the assistant turn, and — if there are tool calls —
//! dispatch each via `ToolRegistry::dispatch_gated` and append the results.
//! Exits with `AgentOutput` when the model stops; aborts with a partial output
//! when the turn cap or timeout fires.
//! What: The whole `chat → dispatch → iterate` body runs inside a single
//! `tokio::time::timeout` so a stalled model or hung tool cannot block forever.
//! Test: `agent_loop::tests` — stubbed two-turn flow, turn-cap abort, recoverable
//! tool-error continuation, usage accrual, plus an `#[ignore]`-gated live test.

mod error;
mod transcript;

#[cfg(test)]
mod tests;

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use crate::llm::{ChatRequest, ChatResponse, LlmClientTrait, ToolCall, ToolDefinition};
use crate::perf::PerfCollector;
use crate::tools::{AgentOutput, ToolRegistry};

pub use error::AgentLoopError;
pub use transcript::Transcript;

/// Finish reason the API emits when the model is done and wants no more tools.
const FINISH_REASON_STOP: &str = "stop";

/// Phase name used when accruing token usage into the `PerfCollector`.
const PERF_PHASE: &str = "agent_loop";

/// Workflow label used to construct the loop's `PerfCollector`.
const PERF_WORKFLOW: &str = "agent_loop";

/// Tuning knobs for a single agent-loop run.
///
/// Why: The three limits (turn cap, wall-clock timeout, model) are the dials a
/// caller most often needs to vary per task; grouping them keeps the `AgentLoop`
/// constructor signature stable as more knobs are added later.
/// What: `max_turns` bounds LLM round-trips; `timeout_secs` bounds total
/// wall-clock time; `model` is the OpenRouter slug sent on every request.
/// Test: `agent_loop::tests::config_defaults_are_sane`.
#[derive(Debug, Clone)]
pub struct AgentLoopConfig {
    /// Maximum number of LLM round-trips before aborting with a partial result.
    pub max_turns: u32,

    /// Wall-clock budget for the entire run, in seconds.
    pub timeout_secs: u64,

    /// OpenRouter model slug sent on every chat request.
    pub model: String,
}

impl Default for AgentLoopConfig {
    /// Sensible defaults for an interactive agent run.
    ///
    /// Why: Most callers want a bounded but generous loop; defaults avoid
    /// boilerplate at every construction site.
    /// What: 8 turns, 120s timeout, `openai/gpt-4o-mini`.
    /// Test: `config_defaults_are_sane`.
    fn default() -> Self {
        Self {
            max_turns: 8,
            timeout_secs: 120,
            model: "openai/gpt-4o-mini".to_string(),
        }
    }
}

/// Drives an LLM through a bounded multi-turn tool-calling conversation.
///
/// Why: Encapsulates the repetitive, error-prone control flow (call → extract
/// tool calls → dispatch → append → repeat) behind one `run` method so call
/// sites express intent ("run this task") not mechanics.
/// What: Holds the config, an `Arc<dyn LlmClientTrait>` (mockable in tests), and
/// an `Arc<ToolRegistry>` whose schemas are advertised and whose
/// `dispatch_gated` executes tool calls.
/// Test: `agent_loop::tests::*`.
pub struct AgentLoop {
    config: AgentLoopConfig,
    llm: Arc<dyn LlmClientTrait>,
    registry: Arc<ToolRegistry>,
}

impl AgentLoop {
    /// Construct an agent loop from its three collaborators.
    ///
    /// Why: Constructor injection keeps the loop testable — production passes a
    /// real `LlmClient`, tests pass a scripted mock, both as
    /// `Arc<dyn LlmClientTrait>`.
    /// What: Stores the config, LLM client, and tool registry.
    /// Test: `agent_loop::tests::two_turn_flow_completes`.
    pub fn new(
        config: AgentLoopConfig,
        llm: Arc<dyn LlmClientTrait>,
        registry: Arc<ToolRegistry>,
    ) -> Self {
        Self {
            config,
            llm,
            registry,
        }
    }

    /// Run the loop to completion (or to a budget limit) and return the output.
    ///
    /// Why: This is the single public entry point; it owns the whole lifecycle
    /// so callers never reconstruct the loop body incorrectly.
    /// What: Wraps `run_inner` in a `tokio::time::timeout`. On timeout, returns
    /// `AgentLoopError::Timeout` carrying whatever partial output `run_inner`
    /// had accumulated up to the deadline.
    /// Test: `agent_loop::tests::two_turn_flow_completes`,
    /// `turn_cap_returns_partial_transcript`, and `timeout_returns_partial`.
    pub async fn run(&self, system: &str, task: &str) -> Result<AgentOutput, AgentLoopError> {
        let mut transcript = Transcript::seed(system, task);
        let mut perf = PerfCollector::new(0, PERF_WORKFLOW, task);

        let timeout = Duration::from_secs(self.config.timeout_secs);
        let inner = self.run_inner(&mut transcript, &mut perf);

        match tokio::time::timeout(timeout, inner).await {
            Ok(result) => result,
            Err(_elapsed) => Err(AgentLoopError::Timeout {
                timeout_secs: self.config.timeout_secs,
                partial: Box::new(build_output(&transcript, &perf)),
            }),
        }
    }

    /// The core loop body, separated so the timeout wrapper stays thin.
    ///
    /// Why: Keeping the iteration logic out of the timeout match arm makes both
    /// halves readable and lets the timeout path reuse the same transcript/perf
    /// state to assemble a partial result.
    /// What: Iterates up to `max_turns`: build request → chat → accrue usage →
    /// append assistant turn → if tool calls, dispatch each and append results →
    /// else (or on `stop`) return the assembled output. Exhausting the turn
    /// budget returns `TurnCapExceeded` with the partial transcript.
    /// Test: Same tests as `run`.
    async fn run_inner(
        &self,
        transcript: &mut Transcript,
        perf: &mut PerfCollector,
    ) -> Result<AgentOutput, AgentLoopError> {
        let schemas = self.tool_definitions();

        for _turn in 0..self.config.max_turns {
            let request = self.build_request(transcript, &schemas);
            let response = self.llm.chat(&request).await?;

            accrue_usage(perf, &self.config.model, &response);
            transcript.push_response(&response);

            let tool_calls = response.first_tool_calls().to_vec();
            let finished = response.finish_reason() == Some(FINISH_REASON_STOP);

            if tool_calls.is_empty() || finished {
                return Ok(build_output(transcript, perf));
            }

            self.dispatch_all(&tool_calls, transcript).await;
        }

        Err(AgentLoopError::TurnCapExceeded {
            max_turns: self.config.max_turns,
            partial: Box::new(build_output(transcript, perf)),
        })
    }

    /// Dispatch every tool call from one assistant turn, appending each result.
    ///
    /// Why: A single assistant turn may request several tools; all must run and
    /// be answered before the next chat call, or the API rejects the follow-up.
    /// What: For each call, parse its JSON arguments (a parse failure becomes a
    /// recoverable error string rather than aborting the loop), dispatch through
    /// `dispatch_gated` with no per-agent allowlist, and append the result as a
    /// `tool` message.
    /// Test: `agent_loop::tests::recoverable_tool_error_continues`.
    async fn dispatch_all(&self, tool_calls: &[ToolCall], transcript: &mut Transcript) {
        for call in tool_calls {
            let args = parse_args(&call.function.arguments);
            let result = self
                .registry
                .dispatch_gated(&call.function.name, args, None)
                .await;
            transcript.push_tool_result(&call.id, &call.function.name, result.content());
        }
    }

    /// Build a `ChatRequest` from the running transcript and tool schemas.
    ///
    /// Why: Each turn re-sends the full history plus the advertised tools so the
    /// model has complete context.
    /// What: Clones the transcript messages, attaches tools (when any), and sets
    /// `tool_choice = "auto"` so the model may call tools but isn't forced to.
    /// Test: Exercised by every loop test.
    fn build_request(&self, transcript: &Transcript, schemas: &[ToolDefinition]) -> ChatRequest {
        let tools = if schemas.is_empty() {
            None
        } else {
            Some(schemas.to_vec())
        };
        let tool_choice = tools.as_ref().map(|_| Value::String("auto".to_string()));

        ChatRequest {
            model: self.config.model.clone(),
            messages: transcript.to_messages(),
            temperature: Some(0.0),
            max_tokens: Some(1024),
            tools,
            tool_choice,
        }
    }

    /// Convert the registry's raw JSON schemas into typed `ToolDefinition`s.
    ///
    /// Why: The registry emits OpenAI-format `serde_json::Value` schemas, but
    /// `ChatRequest.tools` is typed as `Vec<ToolDefinition>`; this bridges the
    /// two without forcing every tool to know about the request type.
    /// What: Deserialises each schema value; silently drops any that fail to
    /// parse (a malformed schema should not abort the run).
    /// Test: `agent_loop::tests::two_turn_flow_completes` advertises a real tool
    /// schema through this path.
    fn tool_definitions(&self) -> Vec<ToolDefinition> {
        self.registry
            .schemas()
            .into_iter()
            .filter_map(|v| serde_json::from_value::<ToolDefinition>(v).ok())
            .collect()
    }
}

/// Parse a tool call's JSON argument string into a `Value`.
///
/// Why: Models occasionally emit malformed or empty argument strings; the loop
/// must not panic on them. An unparseable string degrades to an empty object so
/// the tool can surface its own validation error.
/// What: Parses `raw`; on error or empty input returns `{}`.
/// Test: `agent_loop::tests::parse_args_handles_malformed`.
fn parse_args(raw: &str) -> Value {
    if raw.trim().is_empty() {
        return Value::Object(Default::default());
    }
    serde_json::from_str(raw).unwrap_or_else(|_| Value::Object(Default::default()))
}

/// Accrue one response's token usage into the collector as a phase record.
///
/// Why: Cost/usage tracking across the run is a hard requirement; recording each
/// chat call as a phase keeps per-turn accounting visible in the perf record.
/// What: Clones the response's usage block into `TokenUsage` and records a phase
/// keyed by `PERF_PHASE` against the configured model.
/// Test: `agent_loop::tests::usage_accrues_across_turns`.
fn accrue_usage(perf: &mut PerfCollector, model: &str, response: &ChatResponse) {
    let usage = response.clone().token_usage();
    perf.record_phase(PERF_PHASE, 0, model, &usage);
}

/// Assemble an `AgentOutput` from the current transcript and perf totals.
///
/// Why: Both the success path and the two abort paths need to produce the same
/// shape of output (final text + accumulated usage); centralising it avoids
/// drift between them.
/// What: Uses the transcript's joined assistant text as `content` and the perf
/// record's totals as `usage`.
/// Test: Exercised by every loop test via `run`.
fn build_output(transcript: &Transcript, perf: &PerfCollector) -> AgentOutput {
    let record = perf.build_record();
    let totals = &record.totals;
    let mut output = AgentOutput::from_content(transcript.assistant_text());
    output.usage = crate::perf::TokenUsage::new(
        totals.prompt_tokens,
        totals.completion_tokens,
        totals.cache_read_tokens,
        totals.cache_creation_tokens,
    );
    output
}
