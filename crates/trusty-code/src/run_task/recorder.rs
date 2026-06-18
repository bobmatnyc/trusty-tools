//! Transcript-recording `LlmClientTrait` decorator (#1034).
//!
//! Why: `AgentLoop::run` returns only the final `AgentOutput` (content + usage);
//! it does not expose the per-turn conversation. The `run-task` command must
//! emit a structured transcript of every PM and engineer turn (model + assistant
//! text + tool calls). Rather than change the loop's signature, we wrap the
//! shared `LlmClientTrait` in a recorder that observes each request/response
//! pair and appends a `TurnRecord`. The wrapper is transparent — it forwards to
//! the inner client unchanged — so the real OpenRouter client and the offline
//! mock both work behind it.
//! What: `RecordingLlmClient` holds an inner `Arc<dyn LlmClientTrait>`, a role
//! label (`"pm"` / `"python-engineer"`), and a shared `Arc<Mutex<Vec<TurnRecord>>>`.
//! Each `chat` call records the resolved model and the resulting assistant text
//! plus tool-call names, then returns the inner response verbatim.
//! Test: `run_task::tests` assert the recorded transcript carries both roles, and
//! the per-run engineer model slug.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::Serialize;

use crate::llm::{ChatRequest, ChatResponse, LlmClientTrait, LlmError};
use crate::perf::TokenUsage;

/// One recorded assistant turn for the run transcript.
///
/// Why: The transcript output needs a machine- and human-renderable shape that
/// captures who spoke (role), with which model, what text, and which tools it
/// invoked — enough to reconstruct the PM→engineer interaction without leaking
/// the full raw message history.
/// What: `role` is the agent label; `model` is the resolved slug for that turn;
/// `text` is the assistant prose (empty for tool-only turns); `tool_calls` lists
/// the invoked tool names in order; `usage` is the token usage the provider
/// reported for this turn (the per-turn unit the run aggregates over both PM and
/// engineer turns, since the engineer's usage is otherwise lost when its output
/// flows back to the PM as a tool-result string).
/// Test: `run_task::tests::transcript_has_pm_and_engineer_turns`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TurnRecord {
    /// Agent label that produced this turn (e.g. `"pm"`, `"python-engineer"`).
    pub role: String,
    /// Resolved model slug used for this turn.
    pub model: String,
    /// Assistant prose for the turn (empty string for tool-only turns).
    pub text: String,
    /// Names of tools the assistant called this turn, in order.
    pub tool_calls: Vec<String>,
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

/// A transparent `LlmClientTrait` that records each turn into a shared transcript.
///
/// Why: Captures the conversation for the run report without modifying the agent
/// loop. Wrapping the shared client means PM and engineer turns land in one
/// ordered transcript while usage still accrues on the inner client.
/// What: Forwards `chat` to the inner client, then appends a `TurnRecord` tagged
/// with this wrapper's `role` and the request's model. A poisoned transcript
/// lock is treated as a no-op record (the run must not panic mid-flight).
/// Test: `run_task::tests::transcript_has_pm_and_engineer_turns`.
pub struct RecordingLlmClient {
    inner: Arc<dyn LlmClientTrait>,
    role: String,
    transcript: SharedTranscript,
}

impl RecordingLlmClient {
    /// Wrap `inner`, tagging recorded turns with `role`.
    ///
    /// Why: Each agent (PM, engineer) needs its own labelled recorder sharing the
    /// same transcript sink so the report can distinguish their turns.
    /// What: Stores the inner client, the role label, and the shared transcript.
    /// Test: `run_task::tests::transcript_has_pm_and_engineer_turns`.
    pub fn new(
        inner: Arc<dyn LlmClientTrait>,
        role: impl Into<String>,
        transcript: SharedTranscript,
    ) -> Self {
        Self {
            inner,
            role: role.into(),
            transcript,
        }
    }
}

#[async_trait]
impl LlmClientTrait for RecordingLlmClient {
    /// Forward the chat call and record the resulting turn.
    ///
    /// Why: The transcript must reflect exactly what the model returned; recording
    /// after the call (on success) keeps the report faithful and never fabricates
    /// turns for failed requests.
    /// What: Calls the inner client; on `Ok`, appends a `TurnRecord` with the
    /// request model, the response's first text, and its tool-call names; returns
    /// the response unchanged. Errors propagate untouched (and are not recorded).
    /// Test: `run_task::tests::transcript_has_pm_and_engineer_turns`.
    async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse, LlmError> {
        let resp = self.inner.chat(req).await?;

        let tool_calls = resp
            .first_tool_calls()
            .iter()
            .map(|c| c.function.name.clone())
            .collect();
        let record = TurnRecord {
            role: self.role.clone(),
            model: req.model.clone(),
            text: resp.first_text().unwrap_or_default(),
            tool_calls,
            usage: resp.clone().token_usage(),
        };

        // A poisoned lock must not abort the run; drop the record rather than panic.
        if let Ok(mut guard) = self.transcript.lock() {
            guard.push(record);
        }

        Ok(resp)
    }
}
