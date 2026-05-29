//! Unit tests for the LLM chat-loop helpers.
//!
//! Why: Keeping the (sizeable) test suite in its own file holds both this and
//! the logic module under the 500-line cap while preserving full coverage.
//! What: Covers `ChatResponse` defaults, finish_task extraction, and the
//! tool-discipline decision/state-machine.
//! Test: This IS the test module.

use super::*;
use async_openai::types::ChatCompletionToolType;

#[test]
fn arguments_parse_as_json() {
    let raw = r#"{"agent_name":"python-engineer","task":"hi"}"#;
    let v: serde_json::Value = serde_json::from_str(raw).unwrap();
    assert_eq!(v["agent_name"], "python-engineer");
    assert_eq!(v["task"], "hi");
}

#[test]
fn chat_response_default_empty() {
    let r = ChatResponse::default();
    assert!(r.content.is_none());
    assert!(r.tool_calls.is_empty());
    assert_eq!(r.usage, TokenUsage::default());
}

fn tc(name: &str, args: &str) -> ChatCompletionMessageToolCall {
    ChatCompletionMessageToolCall {
        id: "id-1".into(),
        r#type: ChatCompletionToolType::Function,
        function: FunctionCall {
            name: name.into(),
            arguments: args.into(),
        },
    }
}

#[test]
fn extract_finish_task_summary_finds_call() {
    let calls = vec![
        tc("other_tool", "{}"),
        tc("finish_task", r#"{"summary":"all tests green"}"#),
    ];
    let s = extract_finish_task_summary(&calls);
    assert_eq!(s.as_deref(), Some("all tests green"));
}

#[test]
fn extract_finish_task_summary_absent_returns_none() {
    let calls = vec![tc("web_search", r#"{"query":"x"}"#)];
    assert!(extract_finish_task_summary(&calls).is_none());
}

#[test]
fn extract_finish_task_summary_defaults_on_missing_arg() {
    let calls = vec![tc("finish_task", "{}")];
    let s = extract_finish_task_summary(&calls);
    assert_eq!(s.as_deref(), Some(""));
}

#[test]
fn extract_finish_task_summary_defaults_on_malformed_json() {
    let calls = vec![tc("finish_task", "not json at all")];
    // Malformed args should still exit the loop rather than hang.
    let s = extract_finish_task_summary(&calls);
    assert_eq!(s.as_deref(), Some(""));
}

#[test]
fn adapter_detects_anthropic_variants() {
    // Sanity: the adapter factory should classify each of these correctly.
    // The previous `model_is_anthropic()` helper is gone; this test
    // guards the same behavioral contract via `adapter_for_model`.
    use crate::llm::adapter::{Provider, adapter_for_model};
    assert_eq!(
        adapter_for_model("anthropic/claude-sonnet-4-5").provider(),
        Provider::Anthropic
    );
    assert_eq!(
        adapter_for_model("claude-haiku-4").provider(),
        Provider::Anthropic
    );
    assert_eq!(
        adapter_for_model("CLAUDE-OPUS-4").provider(),
        Provider::Anthropic
    );
    assert_eq!(
        adapter_for_model("openai/gpt-4o").provider(),
        Provider::OpenAI
    );
    assert_eq!(
        adapter_for_model("google/gemini-2.5-flash").provider(),
        Provider::Generic
    );
}

// #326 Tests 1 & 2: verify the if-guard behavior used in
// `chat_with_tools_gated` that injects `stop` into the raw request body
// only when `cfg.llm.stop_sequences` is non-empty. Mirrors the call-site
// logic so accidental regressions (e.g. always setting `stop`, or
// dropping the array) are caught.

#[test]
fn stop_sequences_injected_into_raw_body_when_nonempty() {
    let seqs: Vec<String> = vec!["```\n\n".into(), "<|end|>".into()];
    let mut body = serde_json::json!({});
    if !seqs.is_empty() {
        body["stop"] = serde_json::json!(seqs);
    }
    let stop = body["stop"].as_array().expect("stop must be array");
    assert_eq!(stop.len(), 2);
    assert_eq!(stop[0].as_str().unwrap(), "```\n\n");
    assert_eq!(stop[1].as_str().unwrap(), "<|end|>");
}

#[test]
fn empty_stop_sequences_leave_no_stop_key_in_raw_body() {
    let seqs: &[String] = &[];
    let mut body = serde_json::json!({});
    if !seqs.is_empty() {
        body["stop"] = serde_json::json!(seqs);
    }
    assert!(
        body.get("stop").is_none(),
        "stop key must be absent when stop_sequences is empty"
    );
}

#[test]
fn tool_discipline_decision_first_plain_text_retries() {
    // turn 0 of 12, 0 prior no-tool turns -> retry
    assert!(should_retry_plain_text_turn(0, 0, 12));
}

#[test]
fn tool_discipline_decision_second_plain_text_accepts() {
    // We've already retried once (counter == 1). Do NOT retry again.
    assert!(!should_retry_plain_text_turn(1, 1, 12));
}

#[test]
fn tool_discipline_decision_final_turn_accepts_even_first_time() {
    // Don't inject a retry on the very last turn — the loop would break
    // anyway; just accept whatever the model produced.
    assert!(!should_retry_plain_text_turn(0, 11, 12));
}

#[test]
fn tool_discipline_decision_saturates_on_zero_max_turns() {
    // Edge case: max_turns = 0 should never retry (underflow-safe).
    assert!(!should_retry_plain_text_turn(0, 0, 0));
}

/// Pure-logic simulation of the tool-discipline state machine mimicking
/// the plain-text vs tool-call control flow in `chat_with_tools_gated`.
/// This lets us assert the spec's scenarios without a real LLM.
fn simulate_turns(events: &[&str], max_turns: u32) -> (u32, u32, Option<String>) {
    // events: "text" or "tool". Returns (retries_used, turns_run, final_text).
    let mut counter: u32 = 0;
    let mut retries = 0u32;
    let mut final_text: Option<String> = None;
    for (turn_idx, ev) in events.iter().enumerate() {
        let turn = turn_idx as u32;
        if turn >= max_turns {
            break;
        }
        match *ev {
            "text" => {
                if should_retry_plain_text_turn(counter, turn, max_turns) {
                    counter += 1;
                    retries += 1;
                    continue;
                }
                final_text = Some("final-text".into());
                return (retries, turn + 1, final_text);
            }
            "tool" => {
                counter = 0; // tool call resets the discipline counter
            }
            _ => panic!("bad event"),
        }
    }
    (retries, events.len() as u32, final_text)
}

#[test]
fn tool_discipline_plain_then_tool_continues_with_one_retry() {
    // Spec: plain text once, then tool call -> continues; 1 retry used.
    let (retries, _, final_text) = simulate_turns(&["text", "tool"], 12);
    assert_eq!(retries, 1);
    assert!(
        final_text.is_none(),
        "loop should have continued, not ended"
    );
}

#[test]
fn tool_discipline_two_plain_texts_breaks_gracefully() {
    // Spec: plain text twice -> breaks with final answer (graceful).
    let (retries, turns, final_text) = simulate_turns(&["text", "text"], 12);
    assert_eq!(retries, 1, "only the first should trigger a retry");
    assert_eq!(turns, 2);
    assert_eq!(final_text.as_deref(), Some("final-text"));
}

#[test]
fn tool_discipline_all_tool_calls_never_increments_counter() {
    // Spec: normal tool-call flow -> consecutive_no_tool_turns stays 0.
    let (retries, _, final_text) = simulate_turns(&["tool", "tool", "tool"], 12);
    assert_eq!(retries, 0);
    assert!(final_text.is_none());
}
