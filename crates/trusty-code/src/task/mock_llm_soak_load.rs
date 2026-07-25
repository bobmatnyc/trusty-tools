//! [`SoakLoadEchoLlmClient`] — the scripted `LlmClientTrait` behind
//! `TCODE_MOCK_LLM=echo-soak-load` (epic #3866, follow-up to #3869/PR #3887).
//! Sibling of [`super::mock_llm_soak::SoakEchoLlmClient`], same 7-turn/call,
//! resumable-`stop`-ending shape (see that module's docs for why 7, not 8,
//! and why the client must be re-constructed per `task.run` call) — the
//! ONLY difference is the SIZE of every non-`clear_goal` turn's argument.
//!
//! Why: PR #3887's soak (issue #3869) proved the #2346 cadence mechanism
//! *functions* (28 fires, 0 threshold-compaction events, working-context
//! floor 94-95%) but its own report flagged, as the single biggest caveat,
//! that it never got anywhere near the 60%-floor boundary: `SoakEchoLlmClient`
//! carries five near-empty turns (~20-30 bytes) and exactly ONE ~8 KB
//! oversized turn per call — peak overhead measured ~7% of a 200K window,
//! nowhere near the 80K-token/40%-overhead cap the epic's guarantee is
//! actually about. This client is the follow-up soak that report called for:
//! every `set_goal` turn (not one in six) now carries a payload sized to
//! approximate a REAL tool-output magnitude a long coding session
//! accumulates — a multi-file `git diff`, a `grep`/search-result dump, a
//! `cargo test` failure log — so a single `task.run` call's active zone
//! alone approaches or exceeds the 80K-token cap on a 200K window, forcing
//! [`cadence::enforce_budget`]'s continuous per-turn path to actually
//! compact on (most) turns, not just once per soak.
//! What: same 3x (`set_goal`/`clear_goal`) pair + bare `stop` shape as
//! [`super::mock_llm_soak::SoakEchoLlmClient`], but the three `set_goal`
//! payloads are [`LOAD_PAYLOAD_BYTES`] — representative sizes for
//! (in order) a large `git diff`, a `grep -r`-sized result dump, and a
//! `cargo test` failure log, chosen so their SUM (~172K estimated tokens at
//! this crate's chars/4 heuristic) is well past the 80K-token cap
//! (`CadenceConfig::default().overhead_cap_tokens(200_000)`) within ONE
//! call, not just cumulatively across the whole soak. Empirically (see the
//! epic #3866 load-soak evidence this client produced) this size drives the
//! measured working-context floor down to exactly the 60% target boundary
//! (60-61% on a 245-turn run, 126 cadence-fire samples) — i.e. it is sized
//! to find the guarantee's edge, not comfortably clear it. A follow-up,
//! deliberately MORE extreme payload (~230K tokens/call) was used
//! exploratively (not shipped as the default here) and reproducibly drove
//! the floor down to 48% — a genuine breach of the 60% target — while the
//! independent threshold/fallback compactor still never fired and session
//! fidelity (goal state, transcript, resumability) still held; see the
//! evidence doc for the full writeup of both runs.
//! Test: `tests::soak_load_script_ends_in_a_resumable_stop_and_sums_past_the_default_cap`.

use super::*;

/// Per-turn `set_goal` payload sizes (bytes), in script order — representative
/// of a large `git diff`, a `grep`/search-result dump, and a `cargo test`
/// failure log respectively. Chosen so the three SUM
/// (`LOAD_PAYLOAD_BYTES.iter().sum::<usize>() / 4` estimated tokens) exceeds
/// `CadenceConfig::default().overhead_cap_tokens(200_000)` (80,000) — see
/// module docs for the empirical floor this size drives (60-61%, right at
/// the epic's target boundary).
const LOAD_PAYLOAD_BYTES: [usize; 3] = [160_000, 230_000, 300_000];

/// A deterministic, offline `LlmClientTrait` driving load-realistic
/// per-turn payload sizes for the compression-effectiveness follow-up soak
/// (epic #3866).
///
/// Why/What: see module docs. Identical control flow to
/// [`super::mock_llm_soak::SoakEchoLlmClient`] — only the argument sizes
/// differ.
/// Test: `tests::soak_load_script_ends_in_a_resumable_stop_and_sums_past_the_default_cap`.
pub struct SoakLoadEchoLlmClient {
    cursor: AtomicUsize,
}

impl SoakLoadEchoLlmClient {
    /// Construct a fresh client at the start of its script.
    pub fn new() -> Self {
        Self {
            cursor: AtomicUsize::new(0),
        }
    }
}

impl Default for SoakLoadEchoLlmClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl LlmClientTrait for SoakLoadEchoLlmClient {
    async fn chat(&self, _req: &ChatRequest) -> Result<ChatResponse, LlmError> {
        let idx = self.cursor.fetch_add(1, Ordering::SeqCst);
        let fixture = match idx {
            0 => load_goal_tool_call(idx, 1, LOAD_PAYLOAD_BYTES[0]),
            1 => load_clear_goal_tool_call(idx, 1),
            2 => load_goal_tool_call(idx, 2, LOAD_PAYLOAD_BYTES[1]),
            3 => load_clear_goal_tool_call(idx, 2),
            4 => load_goal_tool_call(idx, 3, LOAD_PAYLOAD_BYTES[2]),
            5 => load_clear_goal_tool_call(idx, 3),
            6 => load_stop_fixture(),
            _ => {
                return Err(LlmError::MissingConfig(format!(
                    "SoakLoadEchoLlmClient script exhausted at call {idx}"
                )));
            }
        };
        serde_json::from_value(fixture).map_err(|e| {
            LlmError::MissingConfig(format!(
                "SoakLoadEchoLlmClient: invalid scripted fixture: {e}"
            ))
        })
    }
}

/// Build a `set_goal(slot, text)` tool-call fixture carrying a
/// `payload_bytes`-sized, realistically-shaped body (mimics a real tool
/// output's line-oriented structure rather than a single repeated
/// character, matching how compressible a REAL diff/grep/test-log body
/// actually is — see [`synthetic_tool_output`]).
fn load_goal_tool_call(idx: usize, slot: usize, payload_bytes: usize) -> Value {
    let text = synthetic_tool_output(payload_bytes);
    json!({
        "id": "mock-soak-load-set",
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": format!("call-soak-load-{idx}"),
                    "type": "function",
                    "function": {
                        "name": "set_goal",
                        "arguments": json!({"slot": slot, "text": text}).to_string()
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 15, "completion_tokens": 5, "total_tokens": 20}
    })
}

/// Build a `clear_goal(slot)` tool-call fixture — deliberately tiny (mirrors
/// `super::mock_llm_soak::soak_clear_goal_tool_call`) so every large
/// contribution to the transcript comes from the `set_goal` turns above,
/// keeping the per-turn size attribution unambiguous when reading the
/// resulting `compression.jsonl`.
fn load_clear_goal_tool_call(idx: usize, slot: usize) -> Value {
    json!({
        "id": "mock-soak-load-clear",
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": format!("call-soak-load-{idx}"),
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

/// The final, no-tool-calls fixture — see
/// `super::mock_llm_soak::stop_fixture`'s identical rationale.
fn load_stop_fixture() -> Value {
    json!({
        "id": "mock-soak-load-stop",
        "choices": [{
            "message": {"role": "assistant", "content": "soak-load call complete", "tool_calls": []},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
    })
}

/// Synthesize `target_bytes` of line-oriented text approximating a real
/// tool output's shape (`diff`/`grep`/test-log lines average well under 200
/// chars each, with real repetition across lines — e.g. the same file path
/// or `FAIL`/`+`/`-` prefix recurring) rather than one giant repeated-
/// character blob.
///
/// Why: a single-character-repeated string (`"x".repeat(n)`, the ORIGINAL
/// `SoakEchoLlmClient`'s ~8 KB oversized-turn approach) is maximally
/// UNREPRESENTATIVE of what `estimate_tokens`'s chars/4 heuristic sees from
/// a real diff/log — real tool output is highly structured, line-oriented
/// text. This generator interleaves a handful of realistic line shapes
/// (unified-diff hunks, grep `path:line:match` rows, cargo test `FAILED`/
/// `assertion` lines) cycling deterministically until `target_bytes` is
/// reached, so the soak's payload has the same rough token-per-byte
/// character as the load profile it's meant to approximate.
fn synthetic_tool_output(target_bytes: usize) -> String {
    const LINE_TEMPLATES: [&str; 6] = [
        "+    let result = compute_budget(&transcript, cadence_cfg, keep_last_messages);\n",
        "-    let result = compute_budget(&transcript, keep_last_messages);\n",
        "src/agent_loop/cadence.rs:187: match maybe_cadence_compress(&mut transcript) {\n",
        "test agent_loop::cadence::tests::stays_under_budget_every_turn ... FAILED\n",
        "thread 'cadence::tests' panicked at src/agent_loop/cadence_tests.rs:412:9:\n",
        "assertion `left == right` failed: overhead_tokens exceeded cap_tokens\n",
    ];
    let mut out = String::with_capacity(target_bytes + 128);
    let mut i = 0usize;
    while out.len() < target_bytes {
        out.push_str(LINE_TEMPLATES[i % LINE_TEMPLATES.len()]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_loop::CadenceConfig;

    /// The script must be 7 calls long, ending in a bare `stop`, with the
    /// three `set_goal` payloads' estimated tokens (chars/4, this crate's
    /// `estimate_tokens` heuristic) summing to MORE than the default
    /// 200K-window/40%-overhead cap (80,000) — the exact property that
    /// makes this client "load-realistic" versus the original
    /// `SoakEchoLlmClient`'s single ~8 KB one-off turn. If this regresses
    /// (e.g. someone shrinks `LOAD_PAYLOAD_BYTES`), the whole point of this
    /// harness variant is silently lost, so it's asserted directly here
    /// rather than only discovered empirically against a live daemon.
    #[tokio::test]
    async fn soak_load_script_ends_in_a_resumable_stop_and_sums_past_the_default_cap() {
        let cap = CadenceConfig::default().overhead_cap_tokens(200_000);
        let total_estimated_tokens: usize = LOAD_PAYLOAD_BYTES.iter().map(|bytes| bytes / 4).sum();
        assert!(
            total_estimated_tokens > cap,
            "load payload sum ({total_estimated_tokens} tokens) must exceed the default \
             overhead cap ({cap} tokens) for this soak variant to be load-realistic"
        );

        let client = SoakLoadEchoLlmClient::new();
        let req = ChatRequest {
            model: "mock".to_string(),
            messages: vec![],
            temperature: None,
            max_tokens: None,
            tools: None,
            tool_choice: None,
            usage: None,
        };

        for i in 0..6 {
            let resp = client
                .chat(&req)
                .await
                .unwrap_or_else(|e| panic!("call {i}: {e}"));
            let calls = resp.first_tool_calls();
            assert_eq!(calls.len(), 1, "call {i} must be exactly one tool call");
        }

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
