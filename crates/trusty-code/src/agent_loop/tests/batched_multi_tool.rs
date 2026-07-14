//! Regression guards for BATCHED multi-tool-per-turn dispatch (round-trip
//! reduction).
//!
//! Why: The BASE preamble (v1.5.0) now invites the model to batch connected,
//! independent tool calls into a SINGLE turn to cut agent round-trips (the L1
//! bake-off measured tcode at 33 turns vs claude-code's 15, root-caused to the
//! old "one tool call, then wait" instruction). The loop's `dispatch_all` has
//! always executed and answered N tool calls per turn; these tests pin that
//! behaviour so a future preamble or loop change cannot silently drop, reorder,
//! or mis-correlate same-turn calls. Split into this focused child module (from
//! `agent_loop::tests`) to keep the parent test file under its SLOC cap while
//! reusing its scripted-LLM harness verbatim via `use super::*`.
//! What: Reuses the parent module's `ScriptedLlm`, `stop_response`,
//! `registry_with_echo`, and `make_loop` helpers; adds a `multi_echo_response`
//! fixture that packs N `echo` calls into one assistant choice; covers the
//! all-dispatch happy path and the partial-failure (one bad call, siblings
//! still answered, loop continues) path.
//! Test: this module is itself the test surface.

use super::*;

/// Build a response fixture carrying MULTIPLE `echo` tool calls in a single
/// assistant choice.
///
/// Why: The batched-multi-tool regression guards need one `ChatResponse` whose
/// single choice contains N tool calls, to prove `dispatch_all` executes and
/// answers every call in one turn. Each `(call_id, arguments)` pair is emitted
/// verbatim so a caller can mix well-formed and deliberately malformed argument
/// strings for the partial-failure case.
/// What: Emits one assistant message with a `tool_calls` array built from the
/// provided `(id, raw_arguments)` pairs, all naming the `echo` tool, with
/// `finish_reason == "tool_calls"`.
/// Test: the two tests in this module.
fn multi_echo_response(calls: &[(&str, &str)]) -> Value {
    let tool_calls: Vec<Value> = calls
        .iter()
        .map(|(id, raw_arguments)| {
            json!({
                "id": id,
                "type": "function",
                "function": { "name": "echo", "arguments": raw_arguments }
            })
        })
        .collect();
    json!({
        "id": "gen-multi",
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": tool_calls
            },
            "finish_reason": "tool_calls"
        }],
        "usage": { "prompt_tokens": 15, "completion_tokens": 12, "total_tokens": 27 }
    })
}

/// A single assistant turn carrying MULTIPLE tool calls dispatches and answers
/// ALL of them, each result correlated to its `tool_call_id`, in emission order.
///
/// Why: The BASE preamble (v1.5.0) now invites the model to batch connected,
/// independent tool calls into one turn to cut round-trips; the loop's
/// `dispatch_all` has always executed N calls per turn, and this pins that
/// behaviour so a future preamble/loop change cannot silently drop or reorder
/// same-turn calls. Regression guard for the round-trip-reduction lever.
/// What: Scripts ONE response with three `echo` calls, then a stop. Runs via
/// `run_with_transcript` so the resulting transcript can be inspected directly.
/// Asserts exactly two chat calls were made (one batch turn + one stop), and
/// that the transcript's three `tool` messages carry the right `tool_call_id`
/// and echoed content in the exact order the calls were emitted.
/// Test: this test.
#[tokio::test]
async fn batched_multi_tool_calls_all_dispatch_in_one_turn() {
    let llm = Arc::new(ScriptedLlm::from_json(&[
        multi_echo_response(&[
            ("call_a", r#"{"text":"alpha"}"#),
            ("call_b", r#"{"text":"beta"}"#),
            ("call_c", r#"{"text":"gamma"}"#),
        ]),
        stop_response("all three echoed"),
    ]));
    let registry = registry_with_echo(false);
    let agent = make_loop(llm.clone(), registry, AgentLoopConfig::default());

    let mut transcript = Transcript::seed("system", "echo three things at once");
    let out = agent
        .run_with_transcript(&mut transcript, "echo three things at once")
        .await
        .expect("batched turn should complete");

    assert_eq!(out.content, "all three echoed");
    assert_eq!(
        llm.calls(),
        2,
        "three tool calls must be answered within a SINGLE turn, then one stop"
    );

    // The three tool-result messages, in transcript (emission) order.
    let tool_msgs: Vec<crate::llm::ChatMessage> = transcript
        .messages()
        .into_iter()
        .filter(|m| m.role == "tool")
        .collect();
    assert_eq!(
        tool_msgs.len(),
        3,
        "every call in the batch must produce exactly one tool result"
    );

    let seen: Vec<(Option<&str>, Option<&str>)> = tool_msgs
        .iter()
        .map(|m| (m.tool_call_id.as_deref(), m.content.as_deref()))
        .collect();
    assert_eq!(
        seen,
        vec![
            (Some("call_a"), Some("echo: alpha")),
            (Some("call_b"), Some("echo: beta")),
            (Some("call_c"), Some("echo: gamma")),
        ],
        "each tool result must correlate to its call_id and preserve emission order"
    );
}

/// When one call in a batch fails, the loop still returns real results for the
/// other calls plus an error result for the failed one, and continues the turn.
///
/// Why: Partial failure inside a batch must not abort the whole turn — the model
/// needs every sibling result AND the failure to decide its next step. This pins
/// the preamble's promise that "if one call in a batch fails, you still receive
/// results for every other call plus the failure".
/// What: Scripts one response with three `echo` calls where call 2 carries
/// syntactically malformed JSON arguments (a dispatch-time validation error),
/// then a stop. Asserts the loop still completes in two chat calls, that calls 1
/// and 3 produced their real echoed results, and that call 2's tool result
/// carries an error message (not a fabricated success and not a dropped call).
/// Test: this test.
#[tokio::test]
async fn batched_partial_failure_still_answers_siblings_and_continues() {
    let llm = Arc::new(ScriptedLlm::from_json(&[
        multi_echo_response(&[
            ("call_1", r#"{"text":"first"}"#),
            ("call_2", "{not valid json"),
            ("call_3", r#"{"text":"third"}"#),
        ]),
        stop_response("continued past the partial failure"),
    ]));
    let registry = registry_with_echo(false);
    let agent = make_loop(llm.clone(), registry, AgentLoopConfig::default());

    let mut transcript = Transcript::seed("system", "batch with one bad call");
    let out = agent
        .run_with_transcript(&mut transcript, "batch with one bad call")
        .await
        .expect("a single failed call must not abort the turn");

    assert_eq!(out.content, "continued past the partial failure");
    assert_eq!(
        llm.calls(),
        2,
        "the loop must continue past the partial failure, not abort the turn"
    );

    let tool_msgs: Vec<crate::llm::ChatMessage> = transcript
        .messages()
        .into_iter()
        .filter(|m| m.role == "tool")
        .collect();
    assert_eq!(
        tool_msgs.len(),
        3,
        "all three calls — including the failed one — must produce a tool result"
    );

    assert_eq!(tool_msgs[0].tool_call_id.as_deref(), Some("call_1"));
    assert_eq!(tool_msgs[0].content.as_deref(), Some("echo: first"));

    assert_eq!(tool_msgs[2].tool_call_id.as_deref(), Some("call_3"));
    assert_eq!(tool_msgs[2].content.as_deref(), Some("echo: third"));

    // Call 2 gets an error tool result, correlated to its id — not a success,
    // not a dropped call.
    assert_eq!(tool_msgs[1].tool_call_id.as_deref(), Some("call_2"));
    let failed = tool_msgs[1].content.as_deref().unwrap_or("");
    assert!(
        !failed.is_empty() && failed != "echo: ",
        "the failed call's result must carry a real error message, got {failed:?}"
    );
    assert!(
        !failed.starts_with("echo: "),
        "the failed call must NOT be reported as a successful echo, got {failed:?}"
    );
}
