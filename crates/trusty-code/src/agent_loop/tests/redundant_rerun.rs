//! End-to-end agent-loop tests for the #2682 redundant full-suite re-run
//! suppression path.
//!
//! Why: `redundant_run::tests` prove the pure predicate in isolation; these
//! tests prove the WIRING — that `AgentLoop::dispatch_all`, once
//! `with_redundant_run_suppression` is attached, actually short-circuits a
//! redundant `bash` test re-run with the sentinel result instead of spawning
//! the suite, and that a genuinely-needed post-change run still fires. Split
//! into this focused child module (like the sibling `gate_intercept`) to keep
//! the parent test file under its SLOC cap while reusing its scripted-LLM
//! harness verbatim via `use super::*`.
//! What: Reuses the parent module's `ScriptedLlm`, `bash_call_response`,
//! `registry_with_finish_task_and_bash`, and `make_loop` helpers. The test
//! command is `echo cargo test` — it matches `verify_gate::is_test_command`
//! (it contains the `cargo test` shape) yet, unlike a real `cargo test`, exits
//! 0 deterministically with no dependency on an installed test runner.
//! Test: this module is itself the test surface (includes
//! `redundant_rerun_suppression_logs_info`, the #2857 observability guard).

use super::*;

/// Build a response fixture in which the assistant calls an arbitrary tool by
/// name with `{}` arguments — used here to inject a `write_file`-named call
/// (a code-mutating tool) between two test runs. The call need not dispatch
/// successfully: it lands in the transcript via `push_response` regardless, and
/// the redundancy fold classifies it by NAME, so an unregistered tool that
/// reports a recoverable error still correctly invalidates the passing
/// baseline.
fn named_tool_call_response(call_id: &str, tool: &str) -> serde_json::Value {
    serde_json::json!({
        "id": "gen-tool",
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": call_id,
                    "type": "function",
                    "function": { "name": tool, "arguments": "{}" }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": { "prompt_tokens": 6, "completion_tokens": 4, "total_tokens": 10 }
    })
}

/// Whether any request in `requests` carries a `tool`-role message answering
/// `call_id` whose content satisfies `pred`.
fn tool_result_matches(
    requests: &[Vec<crate::llm::ChatMessage>],
    call_id: &str,
    pred: impl Fn(&str) -> bool,
) -> bool {
    requests.iter().flatten().any(|m| {
        m.role == "tool"
            && m.tool_call_id.as_deref() == Some(call_id)
            && m.content.as_deref().is_some_and(&pred)
    })
}

/// With suppression enabled, a SECOND identical test run over unchanged code is
/// short-circuited: the suite is never spawned and the model receives the
/// [`crate::redundant_run::REDUNDANT_RERUN_MESSAGE`] sentinel instead.
///
/// Why: This is #2682's core acceptance behaviour — the redundant "one final
/// run to confirm" pass must cost no real execution once the prior run passed
/// and nothing changed.
/// What: Script [bash (passes), bash (identical, redundant), finish_task] with
/// suppression attached; assert the first run's REAL result (`exit_code: 0`)
/// reached the model but the second call's result is the sentinel, never a
/// fresh `exit_code` line.
/// Test: this test.
#[tokio::test]
async fn redundant_test_rerun_is_suppressed() {
    let llm = Arc::new(ScriptedLlm::from_json(&[
        bash_call_response("b1", "echo cargo test"),
        bash_call_response("b2", "echo cargo test"),
        finish_task_call_response("f1", r#"{"status": "completed", "summary": "done"}"#),
    ]));
    let registry = registry_with_finish_task_and_bash();
    let agent = make_loop(llm.clone(), registry, AgentLoopConfig::default())
        .with_redundant_run_suppression();

    let out = agent
        .run("system prompt", "run cargo test, then finish")
        .await
        .expect("loop terminates on finish_task");

    assert_eq!(llm.calls(), 3, "both bash turns plus the finish turn run");
    assert_eq!(out.summary.as_deref(), Some("done"));

    let requests = llm.requests();
    // The FIRST run actually executed — its real bash output reached the model.
    assert!(
        tool_result_matches(&requests, "b1", |c| c.contains("exit_code: 0")),
        "the first test run must actually execute"
    );
    // The SECOND run was suppressed — the model saw the sentinel, not a fresh
    // execution result.
    assert!(
        tool_result_matches(&requests, "b2", |c| c
            == crate::redundant_run::REDUNDANT_RERUN_MESSAGE),
        "the redundant second run must be short-circuited with the sentinel"
    );
    assert!(
        !tool_result_matches(&requests, "b2", |c| c.contains("exit_code:")),
        "the suppressed run must NOT carry a real exit_code line"
    );
}

/// The suppression short-circuit fires an INFO-level log naming the
/// suppressed call (#2857).
///
/// Why: A suppressed rerun is a decision that silently changes the run's
/// outcome (a test invocation the model requested did NOT happen); it is the
/// exact "#2682 suppression sentinel: a test run that did NOT happen" case
/// this ticket names as an audited site. `info` (not `warn`) because
/// suppression is the expected, by-design outcome, not an anomaly.
/// What: Same redundant script as `redundant_test_rerun_is_suppressed`,
/// captured via `crate::test_support::begin_capture`/`captured_at_least` at
/// `Level::INFO`; asserts at least one captured message names the
/// suppression.
/// Test: this test.
#[tokio::test]
async fn redundant_rerun_suppression_logs_info() {
    crate::test_support::begin_capture();

    let llm = Arc::new(ScriptedLlm::from_json(&[
        bash_call_response("b1", "echo cargo test"),
        bash_call_response("b2", "echo cargo test"),
        finish_task_call_response("f1", r#"{"status": "completed", "summary": "done"}"#),
    ]));
    let registry = registry_with_finish_task_and_bash();
    let agent = make_loop(llm.clone(), registry, AgentLoopConfig::default())
        .with_redundant_run_suppression();

    agent
        .run("system prompt", "run cargo test, then finish")
        .await
        .expect("loop terminates on finish_task");

    let messages = crate::test_support::captured_at_least(tracing::Level::INFO);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("suppressing redundant full-suite re-run")),
        "expected an info-level suppression log, got: {messages:?}"
    );
}

/// A test run AFTER a code-mutating tool call is NOT suppressed — the
/// genuinely-needed post-change run still fires for real.
///
/// Why: The acceptance criteria require "the single final run must still occur
/// after the last code change"; a `write_file`/`edit` between runs must defeat
/// suppression.
/// What: Script [bash (passes), write_file, bash (identical), finish_task] with
/// suppression attached; assert the second bash call produced a REAL
/// `exit_code: 0` result and NOT the sentinel.
/// Test: this test.
#[tokio::test]
async fn test_rerun_after_edit_is_not_suppressed() {
    let llm = Arc::new(ScriptedLlm::from_json(&[
        bash_call_response("b1", "echo cargo test"),
        named_tool_call_response("w1", "write_file"),
        bash_call_response("b2", "echo cargo test"),
        finish_task_call_response("f1", r#"{"status": "completed", "summary": "done"}"#),
    ]));
    let registry = registry_with_finish_task_and_bash();
    let agent = make_loop(llm.clone(), registry, AgentLoopConfig::default())
        .with_redundant_run_suppression();

    let out = agent
        .run("system prompt", "run cargo test, then finish")
        .await
        .expect("loop terminates on finish_task");

    assert_eq!(
        llm.calls(),
        4,
        "both bash turns, the write turn, and finish run"
    );
    assert_eq!(out.summary.as_deref(), Some("done"));

    let requests = llm.requests();
    // The post-write run must have actually executed — a real exit_code line,
    // never the suppression sentinel.
    assert!(
        tool_result_matches(&requests, "b2", |c| c.contains("exit_code: 0")),
        "a test run after a write_file must actually execute"
    );
    assert!(
        !tool_result_matches(&requests, "b2", |c| c
            == crate::redundant_run::REDUNDANT_RERUN_MESSAGE),
        "a post-change run must NOT be suppressed"
    );
}

/// A DIFFERENTLY-SCOPED second run is NOT suppressed — suppression requires
/// command IDENTITY, so a broader command that never actually ran still
/// executes (#2804 review).
///
/// Why: The stale-green bug the review caught — a narrow run passing must never
/// suppress a broader run. This is the end-to-end wiring proof that the
/// identity check reaches the dispatch loop.
/// What: Script [bash `echo cargo test` (passes), bash `echo cargo test
/// --workspace` (broader, different string), finish_task] with suppression
/// attached; assert the second run produced a REAL `exit_code: 0` result and
/// NOT the sentinel.
/// Test: this test.
#[tokio::test]
async fn differently_scoped_rerun_is_not_suppressed() {
    let llm = Arc::new(ScriptedLlm::from_json(&[
        bash_call_response("b1", "echo cargo test"),
        bash_call_response("b2", "echo cargo test --workspace"),
        finish_task_call_response("f1", r#"{"status": "completed", "summary": "done"}"#),
    ]));
    let registry = registry_with_finish_task_and_bash();
    let agent = make_loop(llm.clone(), registry, AgentLoopConfig::default())
        .with_redundant_run_suppression();

    let out = agent
        .run("system prompt", "run the tests, then finish")
        .await
        .expect("loop terminates on finish_task");

    assert_eq!(llm.calls(), 3, "both bash turns plus the finish turn run");
    assert_eq!(out.summary.as_deref(), Some("done"));

    let requests = llm.requests();
    // The broader second run must have actually executed — a real exit_code
    // line, never the suppression sentinel.
    assert!(
        tool_result_matches(&requests, "b2", |c| c.contains("exit_code: 0")),
        "a differently-scoped run must actually execute"
    );
    assert!(
        !tool_result_matches(&requests, "b2", |c| c
            == crate::redundant_run::REDUNDANT_RERUN_MESSAGE),
        "a differently-scoped run must NOT be suppressed"
    );
}

/// With suppression DISABLED (the default), an identical re-run is NOT
/// short-circuited — the opt-in flag is the only thing that activates the
/// behaviour.
///
/// Why: Every existing call site that never opts in must be a pure no-op; this
/// is the negative control proving suppression is genuinely gated on the
/// builder flag, not always-on.
/// What: The same redundant script as `redundant_test_rerun_is_suppressed` but
/// WITHOUT `with_redundant_run_suppression`; assert the second run executed for
/// real (no sentinel).
/// Test: this test.
#[tokio::test]
async fn rerun_not_suppressed_when_flag_absent() {
    let llm = Arc::new(ScriptedLlm::from_json(&[
        bash_call_response("b1", "echo cargo test"),
        bash_call_response("b2", "echo cargo test"),
        finish_task_call_response("f1", r#"{"status": "completed", "summary": "done"}"#),
    ]));
    let registry = registry_with_finish_task_and_bash();
    // NOTE: no `.with_redundant_run_suppression()`.
    let agent = make_loop(llm.clone(), registry, AgentLoopConfig::default());

    agent
        .run("system prompt", "run cargo test, then finish")
        .await
        .expect("loop terminates on finish_task");

    let requests = llm.requests();
    assert!(
        tool_result_matches(&requests, "b2", |c| c.contains("exit_code: 0")),
        "without the opt-in flag the second run must execute for real"
    );
    assert!(
        !tool_result_matches(&requests, "b2", |c| c
            == crate::redundant_run::REDUNDANT_RERUN_MESSAGE),
        "the sentinel must never appear when suppression is disabled"
    );
}
