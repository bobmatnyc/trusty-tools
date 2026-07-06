//! API-driven end-to-end test for #2056's `task.run` — the M1
//! control-plane cut line: a task actually EXECUTES through the daemon and
//! produces LIVE tool events via the #2055 emission plumbing.
//!
//! Why: the vision spec's Testability requirement (§9, "100% CLI/API
//! Testable") mandates that #2056's slice be validated by spawning the REAL
//! `tcode serve` daemon and driving it over the actual wire protocol — never
//! by calling into `trusty_code`'s Rust API directly (see
//! `tests/session_e2e.rs`'s module docs for the same rationale, applied
//! there to #2054/#2055). Using a live LLM would make this test
//! nondeterministic and require `OPENROUTER_API_KEY` in CI, so this drives
//! the daemon with `TCODE_MOCK_LLM=echo` (#2056's offline `EchoLlmClient`)
//! — a real subprocess, a real JSON-RPC wire, a real in-process PM ->
//! engineer delegation, just a scripted model underneath.
//! What: [`task_run_streams_live_tool_events_to_completion`] provisions a
//! throwaway project with `pm.toml` + `python-engineer.toml`, calls
//! `task.run`, `session.attach`es (the mock run may already have finished by
//! the time this fires — the ring-buffer replay covers that race either
//! way, mirroring `session_e2e.rs`'s own read-until-terminal pattern), and
//! asserts the observed event kinds include the PM's `delegate_to_agent`
//! dispatch AND the delegated engineer's own `bash` dispatch (proving #2056
//! requirement 4 — a delegated sub-agent's activity is observable too) before
//! the session lands on `status: "finished"`.
//! [`task_run_rejects_a_second_overlapping_run`] proves the "no overlapping
//! runs per session" guarantee over the real wire, not just at the
//! `SessionRegistry` unit level.
//! Test: this file IS the test; see `support` for the process/protocol
//! plumbing shared with `session_e2e.rs`.

mod support;

use serde_json::json;
use support::{StdioSession, find_session_event};

/// Provision a throwaway project with `.claude/agents/{pm,python-engineer}.toml`.
fn project_with_agents() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("project tempdir");
    let agents = tmp.path().join(".claude").join("agents");
    std::fs::create_dir_all(&agents).expect("mkdir agents");
    std::fs::write(
        agents.join("pm.toml"),
        "[agent]\nname = \"pm\"\nmodel = \"openai/gpt-4o-mini\"\n[system_prompt]\ncontent = \"You are the PM. Delegate work to python-engineer.\"\n",
    )
    .expect("write pm.toml");
    std::fs::write(
        agents.join("python-engineer.toml"),
        "[agent]\nname = \"python-engineer\"\nmodel = \"deepseek/deepseek-chat\"\n[system_prompt]\ncontent = \"You are a Python engineer.\"\n",
    )
    .expect("write python-engineer.toml");
    tmp
}

/// `task.run` -> `session.attach` must observe the FULL PM -> engineer tool
/// dispatch as live (or, in the replay burst, if the mock run already
/// finished) events, ending with the session `finished`.
#[tokio::test]
async fn task_run_streams_live_tool_events_to_completion() {
    let project = project_with_agents();
    let mut daemon = StdioSession::spawn_with_mock_llm(project.path());

    // 1. task.run — must return immediately (not block on the whole run).
    let run_resp = daemon
        .call(1, "task.run", json!({"task_description": "say hi"}))
        .await;
    assert!(run_resp["error"].is_null(), "task.run failed: {run_resp}");
    let session_id = run_resp["result"]["session_id"]
        .as_str()
        .expect("task.run must return a session_id")
        .to_string();
    assert_eq!(run_resp["result"]["status"], "running");

    // 2. session.attach — the replay burst alone may already contain the
    // whole run (the mock completes near-instantly with no network
    // latency), or only part of it, with the rest arriving live; either way
    // this collects the FULL set of event kinds observed for this session.
    let attach_resp = daemon
        .call(2, "session.attach", json!({"session_id": session_id}))
        .await;
    assert!(
        attach_resp["error"].is_null(),
        "attach failed: {attach_resp}"
    );
    let mut kinds: Vec<String> = attach_resp["result"]["events"]
        .as_array()
        .expect("attach must return a replay events array")
        .iter()
        .map(|e| {
            e["kind"]
                .as_str()
                .expect("kind must be a string")
                .to_string()
        })
        .collect();

    // 3. Keep reading live NDJSON lines until the terminal `session_done`
    // event has been observed — the mock script is finite and deterministic,
    // so this always terminates; `read_lines`' own bounded timeout is the
    // backstop against a genuine regression hanging the test.
    let mut iterations = 0;
    while !kinds.iter().any(|k| k == "session_done") {
        iterations += 1;
        assert!(
            iterations < 20,
            "gave up waiting for session_done after {iterations} read rounds; kinds so far: {kinds:?}"
        );
        let lines = daemon.read_lines(20).await;
        assert!(
            !lines.is_empty(),
            "timed out waiting for more events; kinds so far: {kinds:?}"
        );
        for line in &lines {
            if let Some(envelope) = find_session_event(line, &session_id) {
                kinds.push(
                    envelope["kind"]
                        .as_str()
                        .expect("kind must be a string")
                        .to_string(),
                );
            }
        }
    }

    // 4. The live tool-event stream (#2056's core deliverable) must show
    // BOTH the PM's `delegate_to_agent` dispatch and the delegated
    // engineer's own `bash` dispatch — proving in-process delegation's
    // activity is observable end-to-end, not just the top-level PM loop's.
    let tool_started = kinds.iter().filter(|k| *k == "tool_started").count();
    let tool_finished = kinds.iter().filter(|k| *k == "tool_finished").count();
    assert_eq!(
        tool_started, 2,
        "expected tool_started for delegate_to_agent + bash: {kinds:?}"
    );
    assert_eq!(
        tool_finished, 2,
        "expected tool_finished for delegate_to_agent + bash: {kinds:?}"
    );
    assert!(
        !kinds.iter().any(|k| k == "tool_error"),
        "the scripted mock run must complete without a fatal tool error: {kinds:?}"
    );

    // 5. session.status must reflect the terminal outcome.
    let status_resp = daemon
        .call(3, "session.status", json!({"session_id": session_id}))
        .await;
    assert!(
        status_resp["error"].is_null(),
        "status failed: {status_resp}"
    );
    assert_eq!(status_resp["result"]["status"], "finished");

    daemon.shutdown_via_eof_and_assert_clean_exit().await;
}

/// A second `task.run` against the SAME `session_id` while the first is
/// still executing must be rejected over the real wire — not just at the
/// `SessionRegistry` unit level (`task::executor::tests::spawn_task_run_rejects_second_overlapping_run`).
#[tokio::test]
async fn task_run_rejects_a_second_overlapping_run() {
    let project = project_with_agents();
    let mut daemon = StdioSession::spawn_with_mock_llm(project.path());

    // Use an explicit session_id (sessionful mode) so both calls target the
    // exact same session deterministically.
    let create_resp = daemon
        .call(1, "session.create", json!({"task": "e2e overlap task"}))
        .await;
    assert!(
        create_resp["error"].is_null(),
        "create failed: {create_resp}"
    );
    let session_id = create_resp["result"]["id"]
        .as_str()
        .expect("session.create must return an id")
        .to_string();

    let first = daemon
        .call(
            2,
            "task.run",
            json!({"task_description": "say hi", "session_id": session_id}),
        )
        .await;
    assert!(
        first["error"].is_null(),
        "first task.run must succeed: {first}"
    );

    let second = daemon
        .call(
            3,
            "task.run",
            json!({"task_description": "say hi again", "session_id": session_id}),
        )
        .await;
    assert!(
        second["error"].is_object(),
        "a second overlapping task.run must be rejected: {second}"
    );
    assert_eq!(second["error"]["code"], -32003);

    daemon.shutdown_via_eof_and_assert_clean_exit().await;
}
