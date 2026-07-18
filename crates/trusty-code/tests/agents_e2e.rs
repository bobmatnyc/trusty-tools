//! API-driven end-to-end test for DOC-39 §5.4's `session.get_agents` live
//! agent-roster RPC.
//!
//! Why: `session::registry::agents::get_agents`'s unit tests (and
//! `session::protocol::protocol_agents`'s handler tests) seed roster state
//! directly via `SessionRegistry::record_tool_started`/`record_tool_finished`
//! — real registry plumbing, but never proven against an actual PM ->
//! delegated-engineer run driven through a real `tcode serve --stdio`
//! process, which is what a genuine client sees. This test drives the REAL
//! daemon (never `trusty_code`'s Rust API directly — the same black-box
//! discipline `tests/task_e2e.rs`/`tests/readiness_e2e.rs` follow), using
//! `TCODE_MOCK_LLM=echo` for deterministic, offline `task.run` execution.
//! What: [`completed_run_reports_pm_and_delegated_engineer_in_the_roster`]
//! runs a task to completion (the `pm.md` fixture always delegates to
//! `python-engineer`, per `tests/task_e2e.rs`'s shared `project_with_agents`
//! fixture and `EchoLlmClient`'s fixed script), then calls
//! `session.get_agents` and asserts the roster contains BOTH the PM's own
//! spawn and the delegated engineer's spawn, each with a real (non-empty)
//! `agent_id` and `state: "idle"` once the run has finished.
//! [`never_run_session_returns_an_empty_roster_over_the_wire`] proves the
//! other half: a session that never ran a task has recorded no tool
//! attribution, so the roster is `[]`, not an error.
//! Test: this file IS the test; see `support` for the process/protocol
//! plumbing shared with `session_e2e.rs`/`task_e2e.rs`/`readiness_e2e.rs`.

mod support;

use serde_json::json;
use support::{StdioSession, project_with_agents, run_task_to_completion};

/// A completed PM -> delegated-engineer run must report both spawns in
/// `session.get_agents`'s roster, each with a real per-spawn `agent_id` and
/// `state: "idle"` (no tool call left in flight once the run is done).
#[tokio::test]
async fn completed_run_reports_pm_and_delegated_engineer_in_the_roster() {
    let project = project_with_agents();
    let mut daemon = StdioSession::spawn_with_mock_llm(project.path());

    let session_id = run_task_to_completion(&mut daemon, "say hi").await;

    let agents_resp = daemon
        .call(3, "session.get_agents", json!({"session_id": session_id}))
        .await;
    assert!(
        agents_resp["error"].is_null(),
        "get_agents failed: {agents_resp}"
    );
    let agents = agents_resp["result"]["agents"]
        .as_array()
        .expect("get_agents must return an agents array");

    let names: Vec<&str> = agents
        .iter()
        .map(|a| a["name"].as_str().expect("name must be a string"))
        .collect();
    assert!(
        names.contains(&"pm"),
        "roster must include the PM's own spawn: {agents:?}"
    );
    assert!(
        names.contains(&"python-engineer"),
        "roster must include the delegated engineer's spawn: {agents:?}"
    );

    for agent in agents {
        assert!(
            !agent["agent_id"]
                .as_str()
                .expect("agent_id must be a string")
                .is_empty(),
            "every roster entry must carry a real agent_id: {agent}"
        );
        assert_eq!(
            agent["state"], "idle",
            "a finished run must leave every agent idle, not running: {agent}"
        );
        // §5.4's acknowledged deferrals — see `session::registry::agents`'s
        // module docs for why these are not yet foldable from real state.
        assert!(agent["model"].is_null());
        assert!(agent["task"].is_null());
        assert_eq!(agent["todos"].as_array().unwrap().len(), 0);
        assert_eq!(agent["files_changed"].as_array().unwrap().len(), 0);
    }
}

/// A session with no `task.run` ever executed against it has recorded no
/// tool attribution, so `session.get_agents` must return an empty roster —
/// not an error.
#[tokio::test]
async fn never_run_session_returns_an_empty_roster_over_the_wire() {
    let mut daemon = StdioSession::spawn();

    let create_resp = daemon
        .call(1, "session.create", json!({"task": "never runs a task"}))
        .await;
    assert!(
        create_resp["error"].is_null(),
        "session.create failed: {create_resp}"
    );
    let session_id = create_resp["result"]["id"]
        .as_str()
        .expect("session.create must return an id")
        .to_string();

    let agents_resp = daemon
        .call(2, "session.get_agents", json!({"session_id": session_id}))
        .await;
    assert!(
        agents_resp["error"].is_null(),
        "get_agents failed: {agents_resp}"
    );
    assert_eq!(agents_resp["result"], json!({"agents": []}));
}
