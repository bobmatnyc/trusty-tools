//! Transcript-recording `InferenceAdapter` decorator (#1034).
//!
//! Why: `AgentLoop::run` returns only the final `AgentOutput` (content + usage);
//! it does not expose the per-turn conversation. The `run-task` command must
//! emit a structured transcript of every PM and engineer turn (model + assistant
//! text + tool calls). Rather than change the loop's signature, we wrap the
//! shared `InferenceAdapter` in a recorder that observes each request/response
//! pair and appends a `TurnRecord`. The wrapper is transparent — it forwards to
//! the inner client unchanged — so the real OpenRouter client and the offline
//! mock both work behind it.
//! What: `RecordingLlmClient` holds an inner `Arc<dyn InferenceAdapter>`, a role
//! label (`"pm"` / `"python-engineer"`), and a shared `Arc<Mutex<Vec<TurnRecord>>>`.
//! Each `chat` call records the resolved model and the resulting assistant text
//! plus tool-call names, then returns the inner response verbatim.
//! Test: `run_task::tests` assert the recorded transcript carries both roles, and
//! the per-run engineer model slug.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

use crate::llm::{
    ChatRequest, ChatResponse, ChatStream, ChatStreamEvent, InferenceAdapter, InferenceError,
    StreamAssembly,
};
use crate::perf::TokenUsage;

/// One recorded assistant turn for the run transcript.
///
/// Why: The transcript output needs a machine- and human-renderable shape that
/// captures who spoke (role), with which model, what text, and which tools it
/// invoked — enough to reconstruct the PM→engineer interaction without leaking
/// the full raw message history.
/// What: `role` is the agent label; `model` is the resolved slug for that turn;
/// `text` is the assistant prose (empty for tool-only turns); `tool_calls` lists
/// the invoked tool names in order; `ran_test_command` (#2279) is a summarized
/// signal — `true` iff this turn's tool calls included a `bash` invocation
/// matching `crate::verify_gate::is_test_command` — the delegating PM's own
/// `verify_gate::pm_finish_gate` consults this instead of the engineer's full
/// transcript, since the PM never calls `bash` itself; `usage` is the token
/// usage the provider reported for this turn (the per-turn unit the run
/// aggregates over both PM and engineer turns, since the engineer's usage is
/// otherwise lost when its output flows back to the PM as a tool-result
/// string). `Deserialize` (#2060) lets `tcode`'s CLI thin client parse a
/// `session.get_transcript` JSON-RPC result straight back into
/// `Vec<TurnRecord>`.
/// Test: `run_task::tests::end_to_end_pm_delegates_to_engineer`,
/// `verify_gate::tests::pm_gate_satisfied_when_engineer_ran_tests`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TurnRecord {
    /// Agent label that produced this turn (e.g. `"pm"`, `"python-engineer"`).
    pub role: String,
    /// Resolved model slug used for this turn.
    pub model: String,
    /// Assistant prose for the turn (empty string for tool-only turns).
    pub text: String,
    /// Names of tools the assistant called this turn, in order.
    pub tool_calls: Vec<String>,
    /// Whether this turn ran a `bash` command matching #2279's verify-before-
    /// finish test-command pattern. `#[serde(default)]` keeps a pre-#2279
    /// persisted transcript deserialising cleanly with this flag `false`.
    #[serde(default)]
    pub ran_test_command: bool,
    /// Token usage the provider reported for this turn.
    pub usage: TokenUsage,
}

/// Shared, thread-safe transcript accumulator.
///
/// Why: The PM and engineer recorders run on the same tokio runtime and both
/// append to one ordered transcript; an `Arc<Mutex<…>>` is the minimal shared
/// sink that keeps turn order without global state.
/// What: A type alias for the shared vector of `TurnRecord`s.
/// Test: Exercised by every `run_task` end-to-end test.
pub type SharedTranscript = Arc<Mutex<Vec<TurnRecord>>>;

/// A transparent `InferenceAdapter` that records each turn into a shared transcript.
///
/// Why: Captures the conversation for the run report without modifying the agent
/// loop. Wrapping the shared client means PM and engineer turns land in one
/// ordered transcript while usage still accrues on the inner client.
/// What: Forwards `chat` to the inner client, then appends a `TurnRecord` tagged
/// with this wrapper's `role` and the request's model. A poisoned transcript
/// lock is treated as a no-op record (the run must not panic mid-flight).
/// Test: `run_task::tests::end_to_end_pm_delegates_to_engineer`.
///
/// `Clone` is cheap (an `Arc`, a `String`, and an `Arc`) and exists so
/// [`Self::chat_stream`]'s `'static` stream closure can carry the recorder's
/// transcript handle past the borrow of `&self` (#4425).
#[derive(Clone)]
pub struct RecordingLlmClient {
    inner: Arc<dyn InferenceAdapter>,
    role: String,
    transcript: SharedTranscript,
}

impl RecordingLlmClient {
    /// Wrap `inner`, tagging recorded turns with `role`.
    ///
    /// Why: Each agent (PM, engineer) needs its own labelled recorder sharing the
    /// same transcript sink so the report can distinguish their turns.
    /// What: Stores the inner client, the role label, and the shared transcript.
    /// Test: `run_task::tests::end_to_end_pm_delegates_to_engineer`.
    pub fn new(
        inner: Arc<dyn InferenceAdapter>,
        role: impl Into<String>,
        transcript: SharedTranscript,
    ) -> Self {
        Self {
            inner,
            role: role.into(),
            transcript,
        }
    }

    /// Append one completed turn to the shared transcript.
    ///
    /// Why (#4425): the recorder now decorates BOTH the blocking and the
    /// streaming call. Extracting the recording step means a streamed turn is
    /// recorded by the same code — and therefore identically — rather than by a
    /// second copy that could drift (e.g. one of them forgetting the #2279
    /// test-command flag).
    /// What: derives the record from `req`/`resp` and pushes it. A poisoned
    /// transcript lock drops the record rather than aborting the run.
    /// Test: `run_task::tests::end_to_end_pm_delegates_to_engineer`,
    /// `run_task::tests::transcript_records_resolved_model_not_requested_slug`,
    /// `run_task::tests::recorder_flags_ran_test_command_on_matching_bash_call`.
    fn record_turn(&self, req: &ChatRequest, resp: &ChatResponse) {
        let calls = resp.first_tool_calls();
        let ran_test_command = calls
            .iter()
            .filter_map(crate::verify_gate::bash_command_from_call)
            .any(|cmd| crate::verify_gate::is_test_command(&cmd));
        let tool_calls = calls.iter().map(|c| c.function.name.clone()).collect();
        let record = TurnRecord {
            role: self.role.clone(),
            model: crate::llm::resolved_model(resp, &req.model).to_string(),
            text: resp.first_text().unwrap_or_default(),
            tool_calls,
            ran_test_command,
            usage: crate::llm::token_usage(resp),
        };

        // A poisoned lock must not abort the run; drop the record rather than panic.
        if let Ok(mut guard) = self.transcript.lock() {
            guard.push(record);
        }
    }
}

#[async_trait]
impl InferenceAdapter for RecordingLlmClient {
    // #4425: a transparent decorator must report the WRAPPED backend's identity,
    // never its own.
    crate::llm::delegating_adapter_identity!(inner);

    /// Forward the chat call and record the resulting turn.
    ///
    /// Why: The transcript must reflect exactly what the model returned; recording
    /// after the call (on success) keeps the report faithful and never fabricates
    /// turns for failed requests.
    /// What: Calls the inner client; on `Ok`, appends a `TurnRecord` with the
    /// RESOLVED model slug (#1475 bug 2 — `resp.resolved_model(&req.model)`,
    /// which prefers the provider-reported model and falls back to the
    /// requested slug when the response omits one), the response's first
    /// text, its tool-call names, and (#2279) whether any of those calls was
    /// a matching `bash` test invocation (`ran_test_command`, computed via
    /// `crate::verify_gate::bash_command_from_call` +
    /// `crate::verify_gate::is_test_command` — the SAME predicate the
    /// engineer's own in-transcript gate uses); returns the response
    /// unchanged. Errors propagate untouched (and are not recorded).
    /// Test: `run_task::tests::end_to_end_pm_delegates_to_engineer`,
    /// `run_task::tests::transcript_records_resolved_model_not_requested_slug`,
    /// `run_task::tests::recorder_flags_ran_test_command_on_matching_bash_call`.
    async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse, InferenceError> {
        let resp = self.inner.chat(req).await?;
        self.record_turn(req, &resp);
        Ok(resp)
    }

    /// Forward the STREAMING chat call, recording the turn once it completes.
    ///
    /// Why (#4425): without this override the trait's default would buffer via
    /// [`Self::chat`] — and since `run_task` and `task::executor` wrap EVERY
    /// production client in this recorder, inheriting the default would have
    /// silently disabled streaming on exactly the paths that matter, leaving
    /// streaming working only in tests. The transcript must still be recorded,
    /// which is why this wraps the stream rather than merely forwarding it.
    /// What: delegates to the inner adapter's `chat_stream`, then re-emits every
    /// event unchanged while folding it into a [`StreamAssembly`]; when the
    /// stream ends the assembled turn is recorded exactly as the blocking path
    /// records it. A stream dropped early (cancellation) records nothing —
    /// matching `chat`, which records nothing on error. One difference is
    /// inherent to the wire: a streamed turn carries no resolved-model field
    /// (the SSE terminal event reports finish reason + usage only), so the
    /// recorded model is the REQUESTED slug — the same value
    /// [`crate::llm::resolved_model`] falls back to whenever a provider omits
    /// it, so the record's shape is unchanged.
    /// Test: `run_task::tests` cover the blocking path;
    /// `crate::agent_loop::tests::sink_events` covers the streamed path.
    async fn chat_stream(&self, req: &ChatRequest) -> Result<ChatStream, InferenceError> {
        let inner = self.inner.chat_stream(req).await?;
        let this = self.clone();
        let req = req.clone();
        let mut assembly = StreamAssembly::new();
        Ok(Box::pin(inner.map(move |event| {
            if let Ok(ev) = &event {
                assembly.push(ev.clone());
                if matches!(ev, ChatStreamEvent::Done(_)) {
                    let resp = std::mem::take(&mut assembly)
                        .into_response(String::new(), req.model.clone());
                    this.record_turn(&req, &resp);
                }
            }
            event
        })))
    }
}
