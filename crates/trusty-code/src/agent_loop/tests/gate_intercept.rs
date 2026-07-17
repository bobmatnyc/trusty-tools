//! End-to-end agent-loop tests for the #2279 verify-before-finish gate's
//! block → nudge → retry intercept path.
//!
//! Why: The intercept — a premature `finish_task` REJECTED as a recoverable
//! retry, the model NUDGED, then RECOVERING on a later verified finish — has
//! never fired in any live bake-off run, because a healthy agent runs the
//! named tests proactively and so its first `finish_task` already satisfies
//! the gate. These deterministic scripted-LLM tests force the premature
//! finish and assert the intercept directly, giving the path a permanent
//! regression guard it otherwise lacks. Split into this focused child module
//! (from `agent_loop::tests`) to keep the parent test file under its SLOC cap
//! while reusing its scripted-LLM harness verbatim via `use super::*`.
//! What: Reuses the parent module's `ScriptedLlm`, fixture builders, and
//! `make_loop`/`registry_with_finish_task_and_bash` helpers; covers the
//! trip-then-recover path (with an explicit assertion that the nudge is fed
//! back to the model), the "no test command named" inert case, and the
//! "tests already ran" first-attempt-pass negative control.
//! Test: this module is itself the test surface (includes
//! `finish_gate_trip_logs_warn`, the #2857 observability guard).

use super::*;

/// An attached [`crate::verify_gate::default_finish_gate`] rejects a
/// premature `finish_task` with a RECOVERABLE retry (not a terminal error)
/// when the task names a runnable test command that was never invoked, and
/// the loop recovers once the named tests actually run.
///
/// Why: This is #2279's core acceptance criterion — a `finish_task` call
/// that would otherwise have terminated the loop after turn 1 must instead
/// give the model another turn to run the named tests, then succeed on a
/// LATER `finish_task` call once the gate is satisfied.
/// What: Script [finish_task (premature), bash("pytest tests/ -v"),
/// finish_task (now valid)]; attach the default gate; assert the run
/// completes only after all THREE chat calls (proving turn 1's finish was
/// rejected and turn 3's succeeded), with the final structured summary from
/// the second `finish_task` call. Crucially, the block→nudge half of the
/// intercept is asserted DIRECTLY, not merely inferred from the call count:
/// the rejected finish is fed back to the model as a `tool`-role recoverable
/// result carrying the nudge, so the recovery turn's own request must
/// contain it — this is the exact intercept path no live bake-off run has
/// ever surfaced (agents always run tests proactively, so their first
/// `finish_task` already satisfies the gate).
/// Test: this test.
#[tokio::test]
async fn finish_gate_trips_and_recovers() {
    let llm = Arc::new(ScriptedLlm::from_json(&[
        finish_task_call_response(
            "call-premature",
            r#"{"status": "completed", "summary": "done (premature)"}"#,
        ),
        bash_call_response("call-bash", "pytest tests/ -v"),
        finish_task_call_response(
            "call-verified",
            r#"{"status": "completed", "summary": "done (verified)"}"#,
        ),
    ]));
    let registry = registry_with_finish_task_and_bash();
    let agent = make_loop(llm.clone(), registry, AgentLoopConfig::default())
        .with_finish_gate(crate::verify_gate::default_finish_gate());

    let out = agent
        .run(
            "system prompt",
            "implement the parser; run `pytest tests/ -v` before finishing",
        )
        .await
        .expect("loop should recover and terminate on the verified finish call");

    assert_eq!(
        llm.calls(),
        3,
        "turn 1's premature finish must be rejected, turn 2 runs the named \
         tests, turn 3's finish must succeed"
    );

    // The intercept must be OBSERVABLE, not merely inferred from the call
    // count. The rejected finish is downgraded to a `tool`-role recoverable
    // result carrying the nudge and pushed onto the transcript, so the SECOND
    // request the model receives (turn 2, the recovery turn) must contain it.
    let turn2_request = &llm.requests()[1];
    let nudge_present = turn2_request.iter().any(|m| {
        m.role == "tool"
            && m.content.as_deref().is_some_and(|c| {
                c.contains("finish_task rejected") && c.contains("Run the named test suite")
            })
    });
    assert!(
        nudge_present,
        "turn 2's request must carry the injected block→retry nudge as a tool \
         result; messages: {turn2_request:?}"
    );

    // The premature finish's summary must NEVER surface as the run's output —
    // the block discarded it in favour of the later verified finish.
    assert_ne!(out.summary.as_deref(), Some("done (premature)"));
    assert_eq!(out.summary.as_deref(), Some("done (verified)"));
    assert_eq!(out.content, "Task completed: done (verified)");
}

/// The gate's rejection (#2279) fires a WARN-level log naming the rejection
/// reason (#2857).
///
/// Why: A gate-rejected `finish_task` silently changes the run's outcome — an
/// extra turn is forced the model didn't ask for — so it must be diagnosable
/// from a single run's stderr, matching the ticket's organising principle
/// (an outcome-changing decision is at minimum `warn`).
/// What: Same premature-finish script as `finish_gate_trips_and_recovers`,
/// captured via `crate::test_support::begin_capture`/`captured_at_least` at
/// `Level::WARN`; asserts at least one captured message names the gate
/// interception.
/// Test: this test.
#[tokio::test]
async fn finish_gate_trip_logs_warn() {
    crate::test_support::begin_capture();

    let llm = Arc::new(ScriptedLlm::from_json(&[
        finish_task_call_response(
            "call-premature",
            r#"{"status": "completed", "summary": "done (premature)"}"#,
        ),
        bash_call_response("call-bash", "pytest tests/ -v"),
        finish_task_call_response(
            "call-verified",
            r#"{"status": "completed", "summary": "done (verified)"}"#,
        ),
    ]));
    let registry = registry_with_finish_task_and_bash();
    let agent = make_loop(llm.clone(), registry, AgentLoopConfig::default())
        .with_finish_gate(crate::verify_gate::default_finish_gate());

    agent
        .run(
            "system prompt",
            "implement the parser; run `pytest tests/ -v` before finishing",
        )
        .await
        .expect("loop should recover and terminate on the verified finish call");

    let messages = crate::test_support::captured_at_least(tracing::Level::WARN);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("gate intercepted finish_task")),
        "expected a warn-level gate-intercept log, got: {messages:?}"
    );
}

/// An attached [`crate::verify_gate::default_finish_gate`] is INERT — the
/// success path is unchanged — when the task never names a runnable test
/// command.
///
/// Why: #2279 explicitly defers general polyglot test-suite discovery; the
/// gate must not invent a requirement nothing ever stated, and existing
/// tasks with no test command must behave exactly as before this change.
/// What: Script a single valid `finish_task` call for a task that names no
/// test command; attach the default gate; assert it still terminates after
/// exactly ONE chat call, identically to
/// `explicit_finish_task_terminates_loop_with_structured_summary`.
/// Test: this test.
#[tokio::test]
async fn finish_gate_inert_without_named_test_command() {
    let llm = Arc::new(ScriptedLlm::from_json(&[finish_task_call_response(
        "call-finish",
        r#"{"status": "completed", "summary": "implemented the widget"}"#,
    )]));
    let registry = registry_with_finish_task_and_bash();
    let agent = make_loop(llm.clone(), registry, AgentLoopConfig::default())
        .with_finish_gate(crate::verify_gate::default_finish_gate());

    let out = agent
        .run("system prompt", "implement a widget")
        .await
        .expect("gate must stay inert when no test command is named");

    assert_eq!(llm.calls(), 1, "gate must not force an extra turn");
    assert_eq!(out.summary.as_deref(), Some("implemented the widget"));
}

/// The gate does NOT trip — and injects NO nudge — when the named tests were
/// ALREADY run in an earlier turn: `finish_task` then passes on its FIRST
/// attempt.
///
/// Why: Pins the gate's DISCRIMINATION end-to-end, the negative control
/// distinct from `finish_gate_inert_without_named_test_command` (the "no
/// test command named" inert case). Here the task DOES name `pytest`, so the
/// gate is ARMED — yet because a matching `bash` invocation already landed in
/// the transcript on turn 1, the turn-2 `finish_task` must be accepted
/// immediately, with no rejection nudge injected and no extra turn forced.
/// This is exactly the healthy shape every live bake-off run takes, and why
/// the block→retry intercept never fires there.
/// What: Script [bash("pytest tests/ -v"), finish_task]; attach the default
/// gate; assert the run completes after exactly TWO chat calls, the finish
/// summary is the model's own, and no request ever carried a rejection nudge.
/// Test: this test.
#[tokio::test]
async fn finish_gate_passes_first_attempt_when_named_tests_already_ran() {
    let llm = Arc::new(ScriptedLlm::from_json(&[
        bash_call_response("call-bash", "pytest tests/ -v"),
        finish_task_call_response(
            "call-finish",
            r#"{"status": "completed", "summary": "done (verified first try)"}"#,
        ),
    ]));
    let registry = registry_with_finish_task_and_bash();
    let agent = make_loop(llm.clone(), registry, AgentLoopConfig::default())
        .with_finish_gate(crate::verify_gate::default_finish_gate());

    let out = agent
        .run(
            "system prompt",
            "implement the parser; run `pytest tests/ -v` before finishing",
        )
        .await
        .expect("finish must be accepted on its first attempt once tests have run");

    assert_eq!(
        llm.calls(),
        2,
        "tests ran on turn 1, so turn 2's first finish must succeed with no extra turn"
    );
    assert_eq!(out.summary.as_deref(), Some("done (verified first try)"));

    // No request may EVER carry a rejection nudge — the gate stayed satisfied
    // the whole run, so the block→retry path must not have fired.
    let any_nudge = llm.requests().iter().flatten().any(|m| {
        m.role == "tool"
            && m.content
                .as_deref()
                .is_some_and(|c| c.contains("finish_task rejected"))
    });
    assert!(
        !any_nudge,
        "the gate must not inject a nudge when the named tests already ran"
    );
}
