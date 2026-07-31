//! [`SoakEchoLlmClient`] — the scripted `InferenceAdapter` behind
//! `TCODE_MOCK_LLM=echo-soak` (issue #3869, epic #3866 Slice C). Split out
//! of `task::mock_llm` (`#[path = "mock_llm_soak.rs"] mod mock_llm_soak;`)
//! purely to keep that file under the crate's 500-SLOC cap — see
//! `session::registry`'s `events`/`memory_sink_ext` split for the identical
//! precedent. `use super::*` below pulls in every type this file needs from
//! the parent (`ChatRequest`, `ChatResponse`, `InferenceAdapter`, `InferenceError`,
//! `AtomicUsize`, `Ordering`, `async_trait`, `Value`, `json`).
//!
//! Why: the compression-effectiveness soak harness
//! (`crates/trusty-code/scripts/compression_soak.py`) drives 200+ PM turns
//! by calling `task.run(session_id=..., ...)` repeatedly against ONE
//! persistent session — `task::protocol::task_run` rebuilds the
//! `Arc<dyn InferenceAdapter>` fresh on EVERY call (`build_llm_client()`), so
//! this client's script only ever needs to cover ONE call's worth of PM
//! turns, not the whole soak. Every other scripted client in
//! `task::mock_llm` ends its final turn with an explicit `finish_task`/
//! delegate hand-off; this one deliberately ends with a bare `stop` (no
//! tool calls) — the ONLY way a `task.run` call completes with
//! `SessionStatus::Finished` rather than `Failed`
//! (`session::registry::SessionRegistry::begin_execution` permanently
//! rejects further `task.run` calls once a session is `Failed`/`Cancelled`/
//! `DeadlineExceeded` — only `Finished` sessions may resume, see that
//! method's docs), which is what lets the harness call `task.run` on the
//! SAME `session_id` dozens of times in a row.
//! What: [`SoakEchoLlmClient`] plays a FIXED 7-response script per
//! construction (i.e. per `task.run` call): three `set_goal`/`clear_goal`
//! pairs — the third `set_goal` carries a deliberately OVERSIZED `text`
//! argument (~8 KB) to exercise `cadence::enforce_budget`'s CONTINUOUS,
//! every-turn enforcement path (issue #3869's explicit requirement — a soak
//! that never produces an oversized turn doesn't test the mechanism the
//! epic cared about most) — then a final bare `stop`. 7 turns stays safely
//! under `AgentLoopConfig::default().max_turns` (8), so the loop always
//! completes via the natural "no tool calls" path, never `TurnCapExceeded`.
//! Test: `tests::soak_script_ends_in_a_resumable_stop_and_has_one_oversized_turn`.

use super::*;

/// Approximate byte length of the deliberately OVERSIZED `set_goal` `text`
/// argument [`SoakEchoLlmClient`] emits partway through its script — big
/// enough to move `cadence::estimate_total_tokens` without single-handedly
/// blowing the whole context window in one turn.
const SOAK_OVERSIZED_TEXT_LEN: usize = 8_000;

/// A deterministic, offline `InferenceAdapter` for issue #3869's
/// compression-effectiveness soak harness (epic #3866 Slice C).
///
/// Why: see the module docs above.
/// What: an atomic cursor `idx`, incremented every call within ONE
/// construction (one per `task.run` call — see module docs): calls 0/2/4
/// are `set_goal` on slot `1 + (idx / 2)`, calls 1/3 are `clear_goal` on the
/// matching slot, call 4's `set_goal` carries the oversized text, and call 6
/// is a bare `stop`. Calls 5 clears the slot the oversized text just used.
/// Running past call 6 returns an `InferenceError` (script exhausted) rather than
/// panicking, mirroring every other client in the parent module — the
/// harness must never call `chat` an 8th time in one `task.run`.
/// Test: `tests::soak_script_ends_in_a_resumable_stop_and_has_one_oversized_turn`.
pub struct SoakEchoLlmClient {
    cursor: AtomicUsize,
}

impl SoakEchoLlmClient {
    /// Construct a fresh client at the start of its script.
    pub fn new() -> Self {
        Self {
            cursor: AtomicUsize::new(0),
        }
    }
}

impl Default for SoakEchoLlmClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl InferenceAdapter for SoakEchoLlmClient {
    crate::llm::mock_adapter_identity!("mock-soak-echo");

    async fn chat(&self, _req: &ChatRequest) -> Result<ChatResponse, InferenceError> {
        let idx = self.cursor.fetch_add(1, Ordering::SeqCst);
        let fixture = match idx {
            0 => soak_goal_tool_call("set_goal", idx, 1, "soak progress marker 0"),
            1 => soak_clear_goal_tool_call(idx, 1),
            2 => soak_goal_tool_call("set_goal", idx, 2, "soak progress marker 2"),
            3 => soak_clear_goal_tool_call(idx, 2),
            4 => soak_goal_tool_call("set_goal", idx, 3, &"x".repeat(SOAK_OVERSIZED_TEXT_LEN)),
            5 => soak_clear_goal_tool_call(idx, 3),
            6 => stop_fixture(),
            _ => {
                return Err(InferenceError::MissingConfig(format!(
                    "SoakEchoLlmClient script exhausted at call {idx}"
                )));
            }
        };
        serde_json::from_value(fixture).map_err(|e| {
            InferenceError::MissingConfig(format!(
                "SoakEchoLlmClient: invalid scripted fixture: {e}"
            ))
        })
    }
}

/// Build a `set_goal(slot, text)` tool-call fixture for [`SoakEchoLlmClient`].
fn soak_goal_tool_call(tool_name: &str, idx: usize, slot: usize, text: &str) -> Value {
    json!({
        "id": "mock-soak-set",
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": format!("call-soak-{idx}"),
                    "type": "function",
                    "function": {
                        "name": tool_name,
                        "arguments": json!({"slot": slot, "text": text}).to_string()
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 15, "completion_tokens": 5, "total_tokens": 20}
    })
}

/// Build a `clear_goal(slot)` tool-call fixture for [`SoakEchoLlmClient`].
fn soak_clear_goal_tool_call(idx: usize, slot: usize) -> Value {
    json!({
        "id": "mock-soak-clear",
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": format!("call-soak-{idx}"),
                    "type": "function",
                    "function": {
                        "name": "clear_goal",
                        "arguments": json!({"slot": slot}).to_string()
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
    })
}

/// The final, no-tool-calls fixture — the ONLY way this script's `task.run`
/// call completes with `SessionStatus::Finished` (resumable) rather than
/// `Failed` (permanently terminal). See module docs.
fn stop_fixture() -> Value {
    json!({
        "id": "mock-soak-stop",
        "choices": [{
            "message": {"role": "assistant", "content": "soak call complete", "tool_calls": []},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The script must be 7 calls long, ending in a bare `stop`, with
    /// exactly one oversized `set_goal` argument along the way — the exact
    /// shape `compression_soak.py` relies on to keep resuming the same
    /// session (issue #3869).
    #[tokio::test]
    async fn soak_script_ends_in_a_resumable_stop_and_has_one_oversized_turn() {
        let client = SoakEchoLlmClient::new();
        let req = ChatRequest {
            model: "mock".to_string(),
            messages: vec![],
            temperature: None,
            max_tokens: None,
            tools: None,
            tool_choice: None,
            usage: None,
            stop: None,
        };

        let mut oversized_count = 0;
        for i in 0..6 {
            let resp = client
                .chat(&req)
                .await
                .unwrap_or_else(|e| panic!("call {i}: {e}"));
            let calls = resp.first_tool_calls();
            assert_eq!(calls.len(), 1, "call {i} must be exactly one tool call");
            if calls[0].function.name == "set_goal" {
                let args: Value =
                    serde_json::from_str(&calls[0].function.arguments).expect("valid json args");
                if args["text"].as_str().unwrap_or("").len() >= SOAK_OVERSIZED_TEXT_LEN {
                    oversized_count += 1;
                }
            }
        }
        assert_eq!(oversized_count, 1, "expected exactly one oversized turn");

        // 7th call (idx 6): bare stop, no tool calls.
        let stop = client.chat(&req).await.expect("call 6 (stop)");
        assert!(
            stop.first_tool_calls().is_empty(),
            "final turn must have no tool calls"
        );

        // An 8th call must error, never panic or silently repeat.
        let err = client.chat(&req).await;
        assert!(
            err.is_err(),
            "the script must not silently repeat past its end"
        );
    }
}
