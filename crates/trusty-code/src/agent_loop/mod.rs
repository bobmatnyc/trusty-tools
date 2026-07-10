//! Multi-turn agent loop: drive an LLM through tool calls until it stops.
//!
//! Why: A single chat call cannot accomplish a task that requires reading files,
//! running tools, and reacting to results. The agent loop is the control
//! structure that repeatedly calls the model, dispatches any requested tool
//! calls through the gated registry, feeds results back, and iterates until the
//! model produces a final answer — bounded by a turn cap, a wall-clock timeout,
//! and token-usage accounting so runs stay cheap and observable.
//! What: Exposes `AgentLoop`, `AgentLoopConfig`, `AgentLoopError`, and (#2056)
//! `ToolEventSink`. `run` seeds a `Transcript`, then loops: build a
//! `ChatRequest` from the running history + registry schemas, call
//! `LlmClientTrait::chat`, accrue usage into a `PerfCollector`, append the
//! assistant turn, and — if there are tool calls — dispatch each via
//! `ToolRegistry::dispatch_gated` (notifying the optional sink around each
//! dispatch) and append the results. Exits with `AgentOutput` in either of two
//! ways: the first assistant turn that has NO tool calls (the D3 no-tool-call
//! finish convention, preserved unchanged as the fallback), or (#2072) an
//! explicit, successfully-validated `finish_task` tool call, whose structured
//! `status`/`summary`/`changes`/test-count fields become the final
//! `AgentOutput` via `build_finish_output` — UNLESS (#2279) an attached
//! `FinishGate` rejects it first, downgrading it to a recoverable retry so
//! the model gets another turn instead of finishing prematurely. Aborts with
//! a partial output when the turn cap, timeout, (#2056) an external
//! cancellation flag, or (#2265 fix #5) an external stop signal fires.
//! What: The whole `chat → dispatch → iterate` body runs inside a single
//! `tokio::time::timeout` so a stalled model or hung tool cannot block forever.
//! (#2344) `run` is now a thin wrapper over [`AgentLoop::run_with_transcript`],
//! which drives the SAME loop body against an already-prepared `Transcript`
//! instead of always seeding fresh — the entry point
//! `task::executor::run_and_record` uses so a session's PM conversation
//! persists and grows across repeated `task.run` calls instead of restarting
//! from scratch each time.
//! Test: `agent_loop::tests` — stubbed two-turn flow, turn-cap abort, recoverable
//! tool-error continuation, usage accrual, sink notification order, cancellation,
//! plus an `#[ignore]`-gated live test.

mod cadence;
mod compaction;
mod error;
mod goals;
mod sink;
mod transcript;

#[cfg(test)]
mod tests;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::Value;

use crate::llm::{
    CacheControl, ChatMessage, ChatRequest, ChatResponse, LlmClientTrait, RequestUsageConfig,
    ToolCall, ToolCallExtractError, ToolCallExtractor, ToolDefinition,
};
use crate::mode::HarnessMode;
use crate::perf::PerfCollector;
use crate::tools::{AgentOutput, FINISH_TASK_TOOL_NAME, FinishTaskArgs, ToolRegistry, ToolResult};

pub use cadence::{CadenceConfig, resolve_cadence_config};
pub use compaction::CompactionConfig;
// (#2260) Crate-internal re-export so `llm::bedrock::cache` can reuse the
// same chars/4 token-estimate heuristic for its cachePoint min-size guard
// instead of duplicating it.
pub(crate) use compaction::estimate_tokens;
pub use error::AgentLoopError;
pub use goals::{GOAL_SLOT_COUNT, GoalSlot, GoalSlotError, GoalSlots, GoalSource};
pub use sink::ToolEventSink;
pub use transcript::Transcript;

/// Verify-before-finish gate hook (#2279): inspects the run's own
/// `Transcript` at the exact moment a successful `finish_task` call would
/// otherwise terminate the loop, and may reject it with a human-readable
/// reason instead.
///
/// Why: A closure (rather than a trait) lets two very different verification
/// strategies share one turn-boundary hook without `agent_loop` knowing
/// about either concrete strategy: a delegated engineer's own loop needs
/// only its own `Transcript` (its `bash` calls land there directly — see
/// `crate::verify_gate::default_finish_gate`); the delegating PM's loop
/// never calls `bash` itself, so its gate closure instead captures an
/// external `run_task::SharedTranscript` and consults that instead (see
/// `crate::verify_gate::pm_finish_gate`). This is the SAME decoupling
/// `Self::stop_signal` (#2265 fix #5) already established for "some external
/// condition the loop must consult at a turn boundary" — reused here rather
/// than adding a bespoke third turn-boundary check.
/// What: Returns `None` when the gate is satisfied (or inert — nothing
/// requires verification for this run); `Some(reason)` when it trips,
/// carrying the message fed back to the model as the rejected `finish_task`
/// call's recoverable tool result.
/// Test: `agent_loop::tests::finish_gate_*`.
pub type FinishGate = Arc<dyn Fn(&Transcript) -> Option<String> + Send + Sync>;

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
/// `mode` (#2059) is the resolved `HarnessMode` for this run — the branch
/// point `tool_definitions` uses; defaults to `HarnessMode::default()`
/// (`DailyDriver`) so every pre-#2059 call site that doesn't set it
/// unchanged.
/// Test: `agent_loop::tests::config_defaults_are_sane`.
#[derive(Debug, Clone)]
pub struct AgentLoopConfig {
    /// Maximum number of LLM round-trips before aborting with a partial result.
    pub max_turns: u32,

    /// Wall-clock budget for the entire run, in seconds.
    pub timeout_secs: u64,

    /// OpenRouter model slug sent on every chat request.
    pub model: String,

    /// Per-turn completion-token cap sent as `ChatRequest.max_tokens`.
    ///
    /// Why: A single hard-coded cap starves any turn that must emit more than
    /// its value — e.g. `write_file` on a real source file — silently
    /// truncating output until the loop churns out its turn budget. Callers
    /// must resolve this from the agent's own `[llm].max_tokens` (via
    /// `crate::provider::resolve_max_tokens`) rather than relying on the
    /// default below.
    /// What: Sent verbatim as `Some(self.max_tokens)` on every `ChatRequest`
    /// built by `AgentLoop::build_request`.
    pub max_tokens: u32,

    /// The resolved harness mode for this run (#2059). See
    /// `Self::tool_definitions`' docs for the branch point this feeds.
    pub mode: HarnessMode,

    /// Non-destructive context compaction tuning (#2070, §5.4). Only
    /// consulted when `mode == HarnessMode::DailyDriver` — see
    /// `Self::maybe_compact_transcript`'s docs for why Parity must never
    /// compact.
    pub compaction: CompactionConfig,

    /// Cadence-compressor tuning (#2346, epic #2343). `None` (the default)
    /// disables cadence entirely — the delegated engineer's own loop and
    /// `run_task`'s legacy one-shot/bake-off path both rely on this default
    /// to stay byte-identical to pre-#2346 behaviour; only
    /// `task::executor::run_and_record`'s persistent-session PM loop sets
    /// `Some(_)`. Consulted only under `HarnessMode::DailyDriver` — see
    /// `Self::maybe_cadence_compress`'s docs.
    pub cadence: Option<CadenceConfig>,
}

impl Default for AgentLoopConfig {
    /// Sensible defaults for an interactive agent run.
    ///
    /// Why: Most callers want a bounded but generous loop; defaults avoid
    /// boilerplate at every construction site. `max_tokens` defaults to
    /// `crate::provider::DEFAULT_MAX_TOKENS` — callers that need the agent's
    /// configured `[llm].max_tokens` must set this field explicitly via
    /// `crate::provider::resolve_max_tokens`; this default is only the
    /// generous fallback for when no agent config is consulted.
    /// What: 8 turns, 120s timeout, `openai/gpt-4o-mini`,
    /// `DEFAULT_MAX_TOKENS`, `HarnessMode::default()`, `CompactionConfig::default()`,
    /// `cadence: None` (#2346 — disabled unless a caller opts in).
    /// Test: `config_defaults_are_sane`.
    fn default() -> Self {
        Self {
            max_turns: 8,
            timeout_secs: 120,
            model: "openai/gpt-4o-mini".to_string(),
            max_tokens: crate::provider::DEFAULT_MAX_TOKENS,
            mode: HarnessMode::default(),
            compaction: CompactionConfig::default(),
            cadence: None,
        }
    }
}

/// Drives an LLM through a bounded multi-turn tool-calling conversation.
///
/// Why: Encapsulates the repetitive, error-prone control flow (call → extract
/// tool calls → dispatch → append → repeat) behind one `run` method so call
/// sites express intent ("run this task") not mechanics.
/// What: Holds the config, an `Arc<dyn LlmClientTrait>` (mockable in tests), an
/// `Arc<ToolRegistry>` whose schemas are advertised and whose `dispatch_gated`
/// executes tool calls, and three OPTIONAL collaborators set via builder
/// methods after construction: (#2056) a `sink` notified around every tool
/// dispatch, (#2056) a `cancel` flag checked once per turn boundary, and
/// (#2265 fix #5) a `stop_signal` closure checked at that SAME turn boundary.
/// All three default to `None`, so every existing call site (`run_task`,
/// `runner::in_process`) is unaffected. A fourth optional collaborator,
/// (#2279) `finish_gate`, is set via [`Self::with_finish_gate`] — see
/// [`FinishGate`]'s docs.
/// Test: `agent_loop::tests::*`.
pub struct AgentLoop {
    config: AgentLoopConfig,
    llm: Arc<dyn LlmClientTrait>,
    registry: Arc<ToolRegistry>,
    sink: Option<Arc<dyn ToolEventSink>>,
    cancel: Option<Arc<AtomicBool>>,
    stop_signal: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    finish_gate: Option<FinishGate>,
}

impl AgentLoop {
    /// Construct an agent loop from its three core collaborators.
    ///
    /// Why: Constructor injection keeps the loop testable — production passes a
    /// real `LlmClient`, tests pass a scripted mock, both as
    /// `Arc<dyn LlmClientTrait>`.
    /// What: Stores the config, LLM client, and tool registry; `sink`/`cancel`/
    /// `stop_signal` start `None` — attach them via
    /// [`Self::with_tool_event_sink`]/[`Self::with_cancel_flag`]/
    /// [`Self::with_stop_signal`] when the caller needs them (#2056's
    /// daemon-driven task execution attaches the first two; `run_task`'s PM
    /// loop attaches `stop_signal` (#2265 fix #5); the delegated engineer's
    /// own loop attaches none of them).
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
            sink: None,
            cancel: None,
            stop_signal: None,
            finish_gate: None,
        }
    }

    /// Attach a [`ToolEventSink`] notified around every tool dispatch (#2056).
    ///
    /// Why: Lets a daemon-driven run make its tool activity observable to a
    /// `session.attach`ed client without forking the dispatch loop.
    /// What: Builder-style setter; returns `self` for chaining.
    /// Test: `agent_loop::tests::sink_receives_started_then_finished_in_order`.
    pub fn with_tool_event_sink(mut self, sink: Arc<dyn ToolEventSink>) -> Self {
        self.sink = Some(sink);
        self
    }

    /// Attach a cancellation flag checked once per turn boundary (#2056).
    ///
    /// Why: `session.cancel` on an in-flight daemon-driven run must stop the
    /// loop cooperatively; sharing one `Arc<AtomicBool>` between the caller and
    /// every loop it spawns (PM + any delegated sub-agent loops) is the
    /// cheapest possible cancellation signal.
    /// What: Builder-style setter; returns `self` for chaining. The flag is
    /// checked with `Ordering::Relaxed` — only "has it been set at all" matters,
    /// not fine-grained memory ordering with other state.
    /// Test: `agent_loop::tests::cancel_flag_aborts_before_next_turn`.
    pub fn with_cancel_flag(mut self, cancel: Arc<AtomicBool>) -> Self {
        self.cancel = Some(cancel);
        self
    }

    /// Attach an external stop condition checked once per turn boundary
    /// (#2265 fix #5).
    ///
    /// Why: Some callers know, independently of this loop's own turn/timeout
    /// budget, that a shared external condition has made further turns
    /// pointless — `run_task`'s PM loop is the motivating case: once its
    /// shared `redelegation::RedelegationCapSignal` latches (the engineer
    /// delegation retry decorator has exhausted `MAX_REDELEGATIONS`), every
    /// subsequent `delegate_to_agent` call is a guaranteed dead end, so the
    /// PM must stop issuing them rather than silently burning its remaining
    /// `max_turns` one doomed tool call at a time (the exact bake-off L1
    /// regression this fix closes). Modelled as a boolean-returning closure,
    /// not a narrower `RedelegationCapSignal`-specific hook, so `agent_loop`
    /// stays generic and does not need to depend on `run_task`.
    /// What: Builder-style setter; returns `self` for chaining. Checked with
    /// the SAME turn-boundary placement as [`Self::with_cancel_flag`] — once
    /// per iteration, before the next `chat` call, never mid-tool-call — so a
    /// tool dispatch already in flight always completes and is answered
    /// first.
    /// Test: `agent_loop::tests::stop_signal_aborts_before_next_turn`.
    pub fn with_stop_signal(mut self, signal: Arc<dyn Fn() -> bool + Send + Sync>) -> Self {
        self.stop_signal = Some(signal);
        self
    }

    /// Attach a [`FinishGate`] consulted the moment a successful
    /// `finish_task` call would otherwise terminate the loop (#2279).
    ///
    /// Why: Gives a caller a hook to reject a premature `finish_task` with a
    /// recoverable retry instead of letting the loop terminate — see
    /// [`FinishGate`]'s docs for how the delegated-engineer and delegating-PM
    /// cases differ. `None` (the default) is a pure no-op; every existing
    /// call site that never attaches a gate is unaffected.
    /// What: Builder-style setter; returns `self` for chaining.
    /// Test: `agent_loop::tests::finish_gate_trips_and_recovers`,
    /// `agent_loop::tests::finish_gate_inert_without_named_test_command`.
    pub fn with_finish_gate(mut self, gate: FinishGate) -> Self {
        self.finish_gate = Some(gate);
        self
    }

    /// Run the loop to completion (or to a budget limit) and return the output.
    ///
    /// Why: This is the single public entry point for a fresh, one-shot run —
    /// `run_task` and the delegated engineer's own loop, neither of which
    /// participate in cross-run persistence, always start from a clean slate.
    /// What: Seeds a brand-new `Transcript` (`system` + `task`), then
    /// delegates to [`Self::run_with_transcript`]. Because a fresh seed's
    /// `assistant_text_mark()` is always `0`, the returned output's `content`
    /// is identical to the pre-#2344 behaviour (the whole run's joined
    /// assistant text) — this method's observable contract is unchanged.
    /// Test: `agent_loop::tests::two_turn_flow_completes`,
    /// `turn_cap_returns_partial_transcript`, and `timeout_returns_partial`.
    pub async fn run(&self, system: &str, task: &str) -> Result<AgentOutput, AgentLoopError> {
        let mut transcript = Transcript::seed(system, task);
        self.run_with_transcript(&mut transcript, task).await
    }

    /// Run the loop against an ALREADY-PREPARED transcript instead of
    /// seeding a fresh one (#2344 — the persistent-session entry point).
    ///
    /// Why: `task::executor::run_and_record` needs the PM's conversation to
    /// survive across repeated `task.run` calls on the same session:
    /// `session::SessionRegistry::begin_pm_transcript` either seeds fresh (a
    /// session's first run) or appends the new request as a `user` turn onto
    /// the session's existing, growing history (every run after that) —
    /// either way, `transcript` arrives here already holding exactly the
    /// messages this run should send. [`Self::run`] is just this method
    /// called with a freshly seeded transcript, so both entry points share
    /// one loop implementation.
    /// What: Records `transcript.assistant_text_mark()` BEFORE driving the
    /// loop, runs the SAME timeout-wrapped `run_inner` body `run` used to,
    /// then rewrites `content` on the resulting `AgentOutput` (success or any
    /// partial-abort variant, via `AgentLoopError::partial_output_mut`) to
    /// `transcript.assistant_text_since(mark)` — scoping THIS call's reported
    /// answer to only the turns it produced, even though the transcript
    /// itself keeps the full cumulative history for the model and for
    /// `session.get_transcript`. Skips the rewrite when `summary.is_some()`:
    /// an explicit `finish_task` completion (#2072) already carries a
    /// deliberate, structured summary via `build_finish_output`, which must
    /// not be clobbered by the plain assistant-text join.
    /// Test: `agent_loop::tests::run_with_transcript_does_not_reseed_system_message`,
    /// `agent_loop::tests::run_with_transcript_scopes_output_to_new_turns`,
    /// `agent_loop::tests::run_with_transcript_scopes_partial_output_on_turn_cap`,
    /// `agent_loop::tests::run_with_transcript_does_not_clobber_finish_task_summary`.
    pub async fn run_with_transcript(
        &self,
        transcript: &mut Transcript,
        task: &str,
    ) -> Result<AgentOutput, AgentLoopError> {
        let mark = transcript.assistant_text_mark();
        let mut perf = PerfCollector::new(0, PERF_WORKFLOW, task);

        let timeout = Duration::from_secs(self.config.timeout_secs);
        let inner = self.run_inner(transcript, &mut perf);

        let mut result = match tokio::time::timeout(timeout, inner).await {
            Ok(result) => result,
            Err(_elapsed) => Err(AgentLoopError::Timeout {
                timeout_secs: self.config.timeout_secs,
                partial: Box::new(build_output(transcript, &perf)),
            }),
        };

        match &mut result {
            Ok(output) => scope_output_to_new_turns(output, transcript, mark),
            Err(e) => {
                if let Some(partial) = e.partial_output_mut() {
                    scope_output_to_new_turns(partial, transcript, mark);
                }
            }
        }
        result
    }

    /// The core loop body, separated so the timeout wrapper stays thin.
    ///
    /// Why: Keeping the iteration logic out of the timeout match arm makes both
    /// halves readable and lets the timeout path reuse the same transcript/perf
    /// state to assemble a partial result.
    /// What: Iterates up to `max_turns`: check the (#2056) cancellation flag →
    /// build request → chat → accrue usage → append assistant turn → if tool
    /// calls are present, dispatch each (notifying the optional sink) and
    /// append results, then continue — UNLESS one of those calls was a
    /// successfully-validated `finish_task` (#2072), in which case the loop
    /// returns immediately with the structured completion output built by
    /// `build_finish_output`. Otherwise (a no-tool-call turn) return the
    /// assembled output — the D3 fallback, unchanged. Per D3, `finish_reason`
    /// never gates termination — pending tool calls always run first.
    /// Exhausting the turn budget returns `TurnCapExceeded`; an observed
    /// cancellation returns `Cancelled`; an observed (#2265 fix #5) external
    /// stop signal returns `StoppedBySignal` — all three with the partial
    /// transcript.
    /// Test: Same tests as `run`, plus `cancel_flag_aborts_before_next_turn`,
    /// `stop_signal_aborts_before_next_turn`,
    /// `explicit_finish_task_terminates_loop_with_structured_summary`.
    async fn run_inner(
        &self,
        transcript: &mut Transcript,
        perf: &mut PerfCollector,
    ) -> Result<AgentOutput, AgentLoopError> {
        let schemas = self.tool_definitions();

        for _turn in 0..self.config.max_turns {
            // #2056: checked at the top of every turn boundary — never
            // mid-tool-call — matching the vision spec's "cancellation is not
            // instantaneous" semantics (§12, 11.6).
            if let Some(cancel) = &self.cancel
                && cancel.load(Ordering::Relaxed)
            {
                return Err(AgentLoopError::Cancelled {
                    partial: Box::new(build_output(transcript, perf)),
                });
            }

            // #2265 fix #5: the external stop-signal check, same turn
            // boundary as the cancellation check above — never mid-tool-call.
            // This is what stops `run_task`'s PM loop from issuing further
            // `delegate_to_agent` calls once the shared re-delegation cap has
            // latched, instead of silently spinning through the remaining
            // `max_turns` on doomed retries.
            if let Some(stop) = &self.stop_signal
                && stop()
            {
                return Err(AgentLoopError::StoppedBySignal {
                    partial: Box::new(build_output(transcript, perf)),
                });
            }

            // #2346: cadence compression runs FIRST, same turn boundary as
            // the checks above — the continuous, turn-count-driven mechanism
            // that keeps working context under budget so #2070's threshold
            // compactor below becomes a backstop rather than the primary
            // token-efficiency path.
            self.maybe_cadence_compress(transcript);

            // #2070: compaction check, same turn boundary as the cancellation
            // check above — never mid-tool-call. Runs AFTER cadence so it
            // only ever sees a transcript cadence already kept under budget.
            self.maybe_compact_transcript(transcript);

            let request = self.build_request(transcript, &schemas);
            // TODO(#2265 follow-up): add in-loop retry-with-backoff for
            // AgentLoopError::Llm before aborting to re-delegation — a single
            // retryable Bedrock/transport hiccup here currently propagates
            // straight out via `?` and aborts this whole engineer turn
            // immediately, forcing a full re-delegation cycle (now bounded by
            // `run_task::redelegation::MAX_REDELEGATIONS`, #2265) rather than
            // a cheap in-loop retry of just this one `chat` call. Deferred
            // (optional fix #5) to keep #2265's required scope (fixes 1-4)
            // tightly bounded.
            let response = self.llm.chat(&request).await?;

            accrue_usage(perf, &self.config.model, &response);
            transcript.push_response(&response);

            let tool_calls = response.first_tool_calls().to_vec();

            // Finish/tool-call precedence (D3, per docs/trusty-code/parity-spec.md §5):
            // pending tool calls are ALWAYS executed and the loop continues, even
            // when the same response also carries `finish_reason == "stop"`.
            // Completion is signalled ONLY by an assistant turn with NO tool calls;
            // that turn returns the final answer regardless of `finish_reason`.
            // We must not let a `stop` reason short-circuit pending tool dispatch,
            // or those calls would be silently dropped and the run would end with
            // an unanswered tool request.
            if tool_calls.is_empty() {
                return Ok(build_output(transcript, perf));
            }

            // #2072: an explicit, successfully-validated `finish_task` call
            // terminates the loop immediately with the structured completion
            // report, rather than waiting for a later no-tool-call turn. Every
            // tool call in this turn is still dispatched and answered first
            // (the API requires a result for each), so this check happens
            // AFTER `dispatch_all`, not instead of it.
            if let Some(finish_args) = self.dispatch_all(&tool_calls, transcript).await {
                return Ok(build_finish_output(transcript, perf, &finish_args));
            }
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
    /// What: For each call, parse and schema-validate its JSON arguments via
    /// [`crate::llm::ToolCallExtractor::parse_and_validate`] (#1023) — a parse
    /// or validation failure is reported through [`Self::report_invalid_arguments`]
    /// as a recoverable tool result rather than aborting the loop OR (the
    /// pre-#1023 behaviour) silently dispatching with `{}`. On valid
    /// arguments, notifies the optional (#2056) sink's `tool_started`,
    /// dispatches through `dispatch_gated` with no per-agent allowlist,
    /// notifies `tool_finished` (success) or `tool_error` (a
    /// fatal/non-recoverable `ToolResult::Error`) based on the result, and
    /// appends the result as a `tool` message. EVERY call in `tool_calls` is
    /// dispatched and answered, regardless of whether an earlier one was
    /// `finish_task` — the API requires a result for each pending call.
    /// What (#2072): if any call names [`FINISH_TASK_TOOL_NAME`] and its
    /// dispatch did not return an error (i.e. its arguments passed both the
    /// generic schema validation above AND `FinishTaskTool::execute`'s own
    /// deserialisation), returns `Some(args)` with that call's validated
    /// argument `Value` — the LAST such call wins if the model (unusually)
    /// emits more than one in a single turn. A malformed/invalid `finish_task`
    /// call is reported through the same recoverable-error path as any other
    /// tool and does NOT terminate the loop, letting the model retry.
    /// (#2279) A `finish_task` call that DID succeed is still subject to the
    /// optional [`Self::finish_gate`] — see [`FinishGate`]'s docs. When
    /// attached and it trips, the result is downgraded in place to
    /// `ToolResult::err(reason)` — the SAME recoverable-error shape used
    /// throughout this method — BEFORE the sink notification and transcript
    /// push below, so both see the rejection, not the original success; this
    /// also means the `finish_args` assignment below never fires for a
    /// gate-rejected call, so the loop simply continues to the next turn.
    /// Test: `agent_loop::tests::recoverable_tool_error_continues`,
    /// `agent_loop::tests::malformed_tool_arguments_report_recoverable_error_and_loop_continues`,
    /// `sink_receives_started_then_finished_in_order`,
    /// `sink_receives_tool_error_for_fatal_failures`,
    /// `explicit_finish_task_terminates_loop_with_structured_summary`,
    /// `malformed_finish_task_repairs_then_terminates`,
    /// `finish_task_missing_required_field_is_recoverable_not_terminal`,
    /// `finish_task_invalid_enum_value_is_recoverable_not_terminal`,
    /// `finish_gate_trips_and_recovers`,
    /// `finish_gate_inert_without_named_test_command`.
    async fn dispatch_all(
        &self,
        tool_calls: &[ToolCall],
        transcript: &mut Transcript,
    ) -> Option<Value> {
        let extractor = ToolCallExtractor::new(|name| self.registry.schema_for(name));
        let mut finish_args: Option<Value> = None;

        for call in tool_calls {
            let tool = call.function.name.as_str();

            let args = match extractor.parse_and_validate(tool, &call.function.arguments) {
                Ok(args) => args,
                Err(error) => {
                    self.report_invalid_arguments(&call.id, tool, &error, transcript)
                        .await;
                    continue;
                }
            };

            if let Some(sink) = &self.sink {
                sink.tool_started(&call.id, tool, &args.to_string()).await;
            }

            let mut result = self.registry.dispatch_gated(tool, args.clone(), None).await;

            // #2279: a `finish_task` call that otherwise SUCCEEDED is still
            // subject to the verify-before-finish gate, if one is attached.
            // Tripping it downgrades the result to the same recoverable-error
            // shape every other tool failure uses, so the model sees WHY
            // finish was rejected and gets another turn to fix it, rather
            // than the loop silently declining to terminate.
            if tool == FINISH_TASK_TOOL_NAME
                && !result.is_error()
                && let Some(reason) = self.finish_gate.as_ref().and_then(|gate| gate(transcript))
            {
                result = ToolResult::err(reason);
            }

            if let Some(sink) = &self.sink {
                notify_result(sink.as_ref(), &call.id, tool, &result).await;
            }

            transcript.push_tool_result(&call.id, tool, result.content());

            if tool == FINISH_TASK_TOOL_NAME && !result.is_error() {
                finish_args = Some(args);
            }
        }

        finish_args
    }

    /// Report an invalid or malformed tool call as a recoverable tool result.
    ///
    /// Why: #1023 replaces the prior silent `{}` degrade with the extractor's
    /// precise validation failure. Feeding it back through the SAME
    /// tool-result channel used for a real execution error lets the model
    /// see exactly what was wrong and self-correct on its next turn — bounded
    /// by the loop's existing `max_turns`, with no separate network
    /// round-trip required for this same-turn dispatch path.
    /// What: Notifies the optional sink (`tool_started` with a placeholder
    /// arguments string, then `tool_error` with `error`'s `Display` text),
    /// then appends a `tool` message carrying that same text.
    /// Test: `agent_loop::tests::malformed_tool_arguments_report_recoverable_error_and_loop_continues`.
    async fn report_invalid_arguments(
        &self,
        call_id: &str,
        tool: &str,
        error: &ToolCallExtractError,
        transcript: &mut Transcript,
    ) {
        let message = error.to_string();
        if let Some(sink) = &self.sink {
            sink.tool_started(call_id, tool, "<invalid-arguments>")
                .await;
            sink.tool_error(call_id, tool, &message).await;
        }
        transcript.push_tool_result(call_id, tool, &message);
    }

    /// Apply non-destructive context compaction to `transcript`, gated on mode
    /// (#2070, §5.4; reconciled with §5.9's D2 the same way `tool_definitions`
    /// is).
    ///
    /// Why: The parity-spec (D2) requires `HarnessMode::Parity` runs to stay
    /// byte-identical / full-history for cross-model benchmark fairness —
    /// compacting would silently change what a Parity run sends after enough
    /// turns, breaking that guarantee. `HarnessMode::DailyDriver` is
    /// production's token-efficiency mode, so it is the only mode this method
    /// ever calls `Transcript::maybe_compact` for. (#2349, epic #2343) Under
    /// the #2346 cadence system a threshold fire is no longer expected in
    /// steady state — it means cadence sizing / daemon-availability /
    /// turn-size assumptions were violated — so this is also where that
    /// regression signal is raised. Cadence-DISABLED contexts (`run_task`
    /// one-shot, `Parity`, delegated sub-agent engineer loops — all
    /// `cadence: None`) still use threshold compaction as their PRIMARY,
    /// EXPECTED mechanism, so the error-level framing below must never apply
    /// to them.
    /// What: No-op under `HarnessMode::Parity`; under `HarnessMode::DailyDriver`,
    /// delegates to `transcript.maybe_compact(&self.config.compaction)`. When
    /// that returns `true` (a fire actually happened), unconditionally calls
    /// `transcript.record_threshold_compaction()` (#2349's fact counter,
    /// exposed via `session::TranscriptRecord.compaction_events` —
    /// increments regardless of cadence), then — ONLY when
    /// `self.config.cadence.is_some()` — emits `tracing::error!` with the
    /// current estimated token count, the configured `token_threshold`, and
    /// `cadence_enabled = true` for context. `cadence: None` keeps today's
    /// existing behaviour exactly as it was before this ticket: no log at
    /// this call site at all.
    /// Test: `agent_loop::tests::daily_driver_mode_compacts_long_running_loop`,
    /// `agent_loop::tests::parity_mode_never_compacts_even_past_threshold`,
    /// `agent_loop::tests::forced_degradation_increments_counter_and_logs_error`,
    /// `agent_loop::tests::cadence_none_threshold_fire_does_not_log_error`.
    fn maybe_compact_transcript(&self, transcript: &mut Transcript) {
        if self.config.mode != HarnessMode::DailyDriver {
            return;
        }
        if !transcript.maybe_compact(&self.config.compaction) {
            return;
        }
        transcript.record_threshold_compaction();

        if self.config.cadence.is_some() {
            let estimated_tokens = compaction::estimate_total_tokens(&transcript.to_messages());
            tracing::error!(
                estimated_tokens,
                token_threshold = self.config.compaction.token_threshold,
                cadence_enabled = true,
                "threshold compaction fired unexpectedly under cadence — this is a regression \
                 signal: cadence sizing, daemon availability, or turn-size assumptions were \
                 likely violated (epic #2343 expects compaction_events == 0 in steady state)"
            );
        }
    }

    /// Apply the #2346 cadence compressor to `transcript`, gated on mode the
    /// SAME way [`Self::maybe_compact_transcript`] is, plus an explicit
    /// per-run opt-in.
    ///
    /// Why: Cadence must respect the identical `HarnessMode::Parity` carve-out
    /// as threshold compaction (byte-identical benchmark runs), AND must stay
    /// off by default (`self.config.cadence == None`) so every call site that
    /// doesn't explicitly opt in — the delegated engineer's own loop,
    /// `run_task`'s legacy one-shot/bake-off path — sees zero behavioural
    /// change from before this ticket. Only
    /// `task::executor::run_and_record`'s persistent-session PM loop sets
    /// `AgentLoopConfig.cadence = Some(_)`.
    /// What: No-op unless `mode == HarnessMode::DailyDriver` AND
    /// `self.config.cadence` is `Some`; otherwise resolves the model's real
    /// context window (`crate::provider::resolve_context_window`, mirroring
    /// `Self::maybe_compact_transcript`'s own model-aware sibling) and
    /// delegates to `cadence::maybe_cadence_compress`, reusing
    /// `self.config.compaction.keep_last_messages` as the shared active-zone
    /// size rather than introducing a second, possibly-inconsistent knob.
    /// Test: `agent_loop::tests::cadence_disabled_by_default`,
    /// `agent_loop::tests::cadence_never_fires_in_parity_mode`,
    /// `agent_loop::tests::cadence_fires_in_daily_driver_when_configured`.
    fn maybe_cadence_compress(&self, transcript: &mut Transcript) {
        if self.config.mode != HarnessMode::DailyDriver {
            return;
        }
        let Some(cadence_cfg) = &self.config.cadence else {
            return;
        };
        let context_window = crate::provider::resolve_context_window(&self.config.model);
        cadence::maybe_cadence_compress(
            transcript,
            cadence_cfg,
            self.config.compaction.keep_last_messages,
            context_window,
        );
    }

    /// Build a `ChatRequest` from the running transcript and tool schemas.
    ///
    /// Why: Each turn re-sends the full history plus the advertised tools so the
    /// model has complete context. The completion-token cap must come from
    /// `self.config.max_tokens` (the caller-resolved agent `[llm].max_tokens`,
    /// or `DEFAULT_MAX_TOKENS`) — a hard-coded cap here previously truncated
    /// any turn (e.g. a real `write_file`) needing more than 1024 tokens.
    /// What: Clones the transcript messages, attaches tools (when any), and sets
    /// `tool_choice = "auto"` so the model may call tools but isn't forced to.
    /// (#2156) When [`Self::prompt_cache_enabled`] is `true`, marks the LAST
    /// tool definition and the system-role message with an ephemeral
    /// prompt-cache breakpoint — the byte-stable prefix this turn resends
    /// unchanged from the last — AND (rolling-history follow-up to #2260)
    /// marks the last two non-system messages via
    /// [`mark_cache_breakpoint_on_history`], rolling the cached prefix
    /// forward to cover the growing transcript, the dominant token cost.
    /// When `false` (Parity mode, or a
    /// provider/model that hasn't verified the passthrough), the request is
    /// byte-identical to pre-#2156. This marker is provider-neutral: each
    /// transport interprets it in its own wire shape — OpenRouter's
    /// `anthropic/*` passthrough serialises it as Anthropic's `cache_control`
    /// content-block field (`llm::message::ChatMessage::serialize`); Bedrock's
    /// Converse transport (#2260) translates it into a native `cachePoint`
    /// content block (`llm::bedrock::cache`). Independently, when the resolved
    /// provider's `Provider::wants_detailed_usage()` is `true` (currently
    /// OpenRouter, unconditionally), attaches
    /// `usage: Some(RequestUsageConfig::detailed())` so the response carries
    /// OpenRouter's authoritative `cost` and cache-token breakdown (the
    /// response-side cache-usage fix).
    /// Test: Exercised by every loop test; `config_max_tokens_reaches_chat_request`
    /// pins that a configured cap flows through unchanged;
    /// `daily_driver_anthropic_model_marks_cache_breakpoints`,
    /// `daily_driver_bedrock_model_marks_cache_breakpoints`, and
    /// `parity_mode_never_marks_cache_breakpoints` cover the #2156/#2260 gate;
    /// `build_request_sets_detailed_usage_for_openrouter` covers the
    /// detailed-usage gate.
    fn build_request(&self, transcript: &Transcript, schemas: &[ToolDefinition]) -> ChatRequest {
        let cache_prefix = self.prompt_cache_enabled();
        let provider = crate::provider::provider_for(&self.config.model);

        let tools = if schemas.is_empty() {
            None
        } else {
            let mut schemas = schemas.to_vec();
            if cache_prefix {
                mark_cache_breakpoint_on_tools(&mut schemas);
            }
            Some(schemas)
        };
        let tool_choice = tools.as_ref().map(|_| Value::String("auto".to_string()));

        let mut messages = transcript.to_messages();
        if cache_prefix {
            mark_cache_breakpoint_on_system(&mut messages);
            mark_cache_breakpoint_on_history(&mut messages);
        }

        let usage = provider
            .wants_detailed_usage()
            .then(RequestUsageConfig::detailed);

        ChatRequest {
            model: self.config.model.clone(),
            messages,
            temperature: Some(0.0),
            max_tokens: Some(self.config.max_tokens),
            tools,
            tool_choice,
            usage,
        }
    }

    /// Whether this run should mark the static tools+system prefix with a
    /// prompt-cache breakpoint (#2156).
    ///
    /// Why: Caching changes cost/latency only, never model output, so it is
    /// safe in principle under any mode — but per the #2156 spec this stays
    /// scoped to `HarnessMode::DailyDriver` (mirroring
    /// `Self::maybe_compact_transcript`'s existing mode gate) rather than
    /// silently changing Parity's wire payload. It is further gated on the
    /// resolved `Provider::supports_prompt_caching` so a model/backend whose
    /// `cache_control` passthrough hasn't been verified never receives the
    /// marker.
    /// What: `true` only when `self.config.mode == HarnessMode::DailyDriver`
    /// AND `crate::provider::provider_for(&self.config.model)
    /// .supports_prompt_caching()` is `true`.
    /// Test: `agent_loop::tests::daily_driver_anthropic_model_marks_cache_breakpoints`,
    /// `agent_loop::tests::daily_driver_bedrock_model_marks_cache_breakpoints`,
    /// `agent_loop::tests::parity_mode_never_marks_cache_breakpoints`,
    /// `agent_loop::tests::non_anthropic_model_never_marks_cache_breakpoints`.
    fn prompt_cache_enabled(&self) -> bool {
        self.config.mode == HarnessMode::DailyDriver
            && crate::provider::provider_for(&self.config.model).supports_prompt_caching()
    }

    /// Convert the registry's raw JSON schemas into typed `ToolDefinition`s.
    ///
    /// Why: The registry emits OpenAI-format `serde_json::Value` schemas, but
    /// `ChatRequest.tools` is typed as `Vec<ToolDefinition>`; this bridges the
    /// two without forcing every tool to know about the request type. The
    /// `self.config.mode` branch (#2059) is the tool-schema CONSUMPTION POINT
    /// for P1B's token-efficiency work: `HarnessMode::Parity` must forever
    /// return the registry's full schema set verbatim (parity-spec D2 requires
    /// byte-identical schemas across models); `HarnessMode::DailyDriver` is
    /// where a future deferred/metadata-only schema set will be substituted.
    /// Both arms are identical in M1 — no such set exists yet.
    /// What: Deserialises each schema value; a schema that fails to parse is
    /// dropped (a malformed schema must not abort the run) but logged with
    /// `tracing::warn!` — including the offending tool's name (best-effort from
    /// the raw schema) and the parse error — so a misconfigured tool that is
    /// never advertised to the model is diagnosable rather than silent.
    /// Test: `agent_loop::tests::two_turn_flow_completes` advertises a real tool
    /// schema through this path; `agent_loop::tests::tool_definitions_identical_across_modes_in_m1`
    /// pins the #2059 "no behavioural difference yet" contract.
    fn tool_definitions(&self) -> Vec<ToolDefinition> {
        let schemas = match self.config.mode {
            HarnessMode::Parity => self.registry.schemas(),
            // P1B hooks in here: a deferred/metadata-only schema set. Full
            // schemas until then (#2059 M1 scope).
            HarnessMode::DailyDriver => self.registry.schemas(),
        };
        schemas
            .into_iter()
            .filter_map(
                |v| match serde_json::from_value::<ToolDefinition>(v.clone()) {
                    Ok(def) => Some(def),
                    Err(e) => {
                        let name = schema_tool_name(&v);
                        tracing::warn!(
                            "dropping unparseable tool schema (tool={name}): {e}; raw schema: {v}"
                        );
                        None
                    }
                },
            )
            .collect()
    }
}

/// Mark the LAST tool definition with an ephemeral prompt-cache breakpoint
/// (#2156).
///
/// Why: Anthropic caches everything up to and including a `cache_control`
/// breakpoint as one prefix; tools render first on the wire, so marking only
/// the last tool caches the entire (byte-stable) tools array as a unit —
/// matching `block/goose`'s verified OpenRouter provider.
/// What: No-op on an empty slice; otherwise sets
/// `schemas.last_mut().function.cache_control = Some(CacheControl::ephemeral())`.
/// Test: `agent_loop::tests::daily_driver_anthropic_model_marks_cache_breakpoints`.
fn mark_cache_breakpoint_on_tools(schemas: &mut [ToolDefinition]) {
    if let Some(last) = schemas.last_mut() {
        last.function.cache_control = Some(CacheControl::ephemeral());
    }
}

/// Mark the system-role message with an ephemeral prompt-cache breakpoint
/// (#2156).
///
/// Why: The system prompt is the second (and, combined with the tools
/// breakpoint, final) segment of the byte-stable prefix `build_request`
/// resends every turn; marking it caches tools+system together as OpenRouter
/// renders tools before the system message on the wire.
/// What: Finds the first `role == "system"` entry (there is always exactly
/// one, from `Transcript::seed`) and sets its `cache_control`; no-op if none
/// is found.
/// Test: `agent_loop::tests::daily_driver_anthropic_model_marks_cache_breakpoints`.
fn mark_cache_breakpoint_on_system(messages: &mut [ChatMessage]) {
    if let Some(system) = messages.iter_mut().find(|m| m.role == "system") {
        system.cache_control = Some(CacheControl::ephemeral());
    }
}

/// Mark the last two non-system messages with an ephemeral prompt-cache
/// breakpoint, rolling the cached prefix forward each turn (Bedrock
/// history-cache follow-up to #2260).
///
/// Why: The tools+system prefix `mark_cache_breakpoint_on_tools`/
/// `mark_cache_breakpoint_on_system` mark is small and static — the growing
/// transcript is the dominant token cost. A live cost-proof of #2260
/// as-merged showed cache_read staying 0 and ~452K fresh tokens/run on L1
/// because nothing ever marked the history: every turn re-sent the full
/// conversation as fresh input. Marking the LAST TWO non-system messages
/// (rather than just the last one) mirrors opencode's verified
/// `final.slice(-2)` pattern and guards against a single turn appending more
/// content blocks than Bedrock's ~20-block cache lookback window covers — if
/// only the very last message were marked and it alone exceeded that window,
/// the previous turn's cache write would fall outside it and never hit.
/// Uses breakpoint slots 3+4 of Bedrock's 4-per-request cachePoint budget
/// (slots 1+2 are tools + system).
/// Caveat: the OpenRouter/Anthropic-passthrough serializer
/// (`llm::message::ChatMessage::serialize`) drops `cache_control` when a
/// message has `content: None` (e.g. an assistant turn that is only
/// `tool_calls`). Marking the last two messages already mitigates this in
/// practice (at least one of the two usually carries text content); the
/// Bedrock Converse path (`llm::bedrock::convert`) does not go through that
/// serializer and is unaffected.
/// What: Collects the indices of all non-system messages, then sets
/// `cache_control` on the last (up to) two of them, in original order.
/// No-op when there are zero or one non-system messages (the loop still
/// marks whatever is present).
/// Test: `agent_loop::tests::daily_driver_bedrock_model_marks_cache_breakpoints`.
fn mark_cache_breakpoint_on_history(messages: &mut [ChatMessage]) {
    let idxs: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.role != "system")
        .map(|(i, _)| i)
        .collect();
    for &i in idxs.iter().rev().take(2) {
        messages[i].cache_control = Some(CacheControl::ephemeral());
    }
}

/// Best-effort extraction of a tool's name from its raw OpenAI-format schema.
///
/// Why: When a schema fails to deserialise into `ToolDefinition`, the warning is
/// far more actionable if it names the offending tool; pulling `function.name`
/// out of the still-valid JSON gives operators a handle even though the full
/// typed parse failed.
/// What: Reads `schema["function"]["name"]` as a string, falling back to
/// `"<unknown>"` when the path is absent or non-string.
/// Test: `agent_loop::tests::schema_tool_name_extracts_or_falls_back`.
fn schema_tool_name(schema: &Value) -> &str {
    schema
        .get("function")
        .and_then(|f| f.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("<unknown>")
}

/// Notify a [`ToolEventSink`] of one tool dispatch's outcome (#2056).
///
/// Why: Maps `ToolResult`'s three real states onto the sink's two completion
/// hooks: a non-recoverable (`is_fatal`) error is exceptional — `tool_error` —
/// while success and a recoverable error are both ordinary completions
/// distinguished by `success`, matching #2055's `ToolFinished{success}`/
/// `ToolError` taxonomy split.
/// What: `is_fatal()` -> `tool_error`; otherwise -> `tool_finished(!is_error())`.
/// Test: `agent_loop::tests::sink_receives_started_then_finished_in_order`,
/// `sink_receives_tool_error_for_fatal_failures`.
async fn notify_result(sink: &dyn ToolEventSink, call_id: &str, tool: &str, result: &ToolResult) {
    if result.is_fatal() {
        sink.tool_error(call_id, tool, result.content()).await;
    } else {
        sink.tool_finished(call_id, tool, !result.is_error(), result.content())
            .await;
    }
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

/// Rewrite `output.content` to only the assistant text produced SINCE `mark`
/// (#2344).
///
/// Why: `AgentLoop::run_with_transcript` scopes a persistent-session run's
/// reported output to just its own new turns — see that method's docs.
/// What: No-op when `output.summary` is `Some` (an explicit `finish_task`
/// completion's structured summary must never be overwritten by the plain
/// assistant-text join). Otherwise sets `output.content =
/// transcript.assistant_text_since(mark)`.
/// Test: `agent_loop::tests::run_with_transcript_scopes_output_to_new_turns`,
/// `agent_loop::tests::run_with_transcript_does_not_clobber_finish_task_summary`.
fn scope_output_to_new_turns(output: &mut AgentOutput, transcript: &Transcript, mark: usize) {
    if output.summary.is_none() {
        output.content = transcript.assistant_text_since(mark);
    }
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

/// Assemble the final `AgentOutput` from an explicit `finish_task` call (#2072).
///
/// Why: An explicit `finish_task` call carries a deterministic, structured
/// completion report the model built on purpose — reusing that report as the
/// loop's final output (rather than falling back to whatever prose the
/// transcript happens to contain) is the entire point of §5.8's "-20 to -30%
/// per agent output; deterministic, no prose interpretation" impact claim.
/// What: Starts from the same usage/content baseline as [`build_output`], then
/// — if `finish_args` deserialises into a `FinishTaskArgs` (it always should,
/// since `dispatch_all` only calls this after a successful `finish_task`
/// dispatch, which itself required a successful deserialisation) — overwrites
/// `content` with [`crate::tools::render_finish_summary`] and sets `summary` to
/// the model's own one-line `summary` field. On the (should-be-unreachable)
/// deserialisation failure, falls back to the transcript-derived content
/// `build_output` already computed, rather than panicking.
/// Test: `agent_loop::tests::explicit_finish_task_terminates_loop_with_structured_summary`.
fn build_finish_output(
    transcript: &Transcript,
    perf: &PerfCollector,
    finish_args: &Value,
) -> AgentOutput {
    let mut output = build_output(transcript, perf);
    if let Ok(parsed) = serde_json::from_value::<FinishTaskArgs>(finish_args.clone()) {
        output.summary = Some(parsed.summary.clone());
        output.content = crate::tools::render_finish_summary(&parsed);
    }
    output
}
