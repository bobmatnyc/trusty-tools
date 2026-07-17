//! End-to-end acceptance proof for DOC-39 AC-13.1/13.2 (stable per-spawn
//! `agent_id`) — the mandatory API-driven e2e gate for this slice.
//!
//! Why: the bug this slice fixes is that two delegated sub-agents sharing
//! one `agent_name` (e.g. two `python-engineer` delegations within one PM
//! fan-out) were indistinguishable on the `session.attach` event stream —
//! `tools::delegate` spawned purely by `agent_name`, with no per-spawn
//! identity. Unit tests already pin this at the `agent_loop`/`runner` layers
//! — `agent_loop::tests::sink_events::sequentially_spawned_same_named_loops_get_distinct_agent_ids`
//! and `runner::tests::sequential_delegations_to_same_named_agent_get_distinct_ids`
//! prove it for STRICTLY ORDERED spawns, and
//! `runner::tests::concurrently_delegated_same_named_agents_get_distinct_ids_under_tokio_join`
//! proves it for GENUINELY overlapping (`tokio::join!`-raced) ones — but
//! Bob's standing testability directive requires the capability also be
//! proven reachable over the REAL wire, against the real (subprocess) daemon
//! — this is that proof. Uses `TCODE_MOCK_LLM=echo-fanout`
//! ([`trusty_code::task::mock_llm::FanoutEchoLlmClient`]) for a deterministic,
//! key-free run that scripts the PM fanning out to `python-engineer` TWICE in
//! one turn.
//!
//! NOTE (code-critic MEDIUM, honesty about what this specific test exercises):
//! the PM's own `agent_loop::dispatch_all` dispatches the two
//! `delegate_to_agent` tool calls from that one turn SEQUENTIALLY (a `for`
//! loop over `tool_calls`, each fully `.await`ed before the next starts) —
//! there is no concurrent-execution primitive in the loop itself, so this
//! e2e test does NOT exercise a wall-clock race between the two engineer
//! spawns. What it DOES prove, which the in-process unit tests cannot,
//! is that the fix is wired correctly end-to-end over the real JSON-RPC
//! wire: the daemon, the session registry, and the SSE/stdio event
//! plumbing all carry `agent_id` through untouched from mint to wire. The
//! wall-clock-race property is what
//! `runner::tests::concurrently_delegated_same_named_agents_get_distinct_ids_under_tokio_join`
//! proves instead.
//! What: spawns the real `tcode serve --stdio` binary, runs `task.run` +
//! `session.attach`, drains the replay + live event stream to
//! `session_done`, then asserts the two `tool_started` events for tool
//! `"bash"` are BOTH attributed to `agent == "python-engineer"` but carry
//! DISTINCT, non-empty `agent_id`s — the AC-13 acceptance criterion, stated
//! directly against the wire shape a UI client actually receives.
//! Test: this module is itself the test surface.

mod support;

use serde_json::{Value, json};
use support::{StdioSession, find_session_event, project_with_agents};

/// Drive `task.run` -> `session.attach` -> read-until-`session_done` using
/// the fan-out mock LLM script, returning every session event's full JSON
/// envelope (not just its `kind`, unlike
/// `support::run_task_to_completion` — this test needs each envelope's
/// `agent`/`agent_id` fields, not just the kind string).
async fn run_fanout_task_to_completion(daemon: &mut StdioSession) -> Vec<Value> {
    let run_resp = daemon
        .call(
            1,
            "task.run",
            json!({"task_description": "fan out to two same-named engineers"}),
        )
        .await;
    assert!(run_resp["error"].is_null(), "task.run failed: {run_resp}");
    let session_id = run_resp["result"]["session_id"]
        .as_str()
        .expect("task.run must return a session_id")
        .to_string();

    let attach_resp = daemon
        .call(2, "session.attach", json!({"session_id": session_id}))
        .await;
    assert!(
        attach_resp["error"].is_null(),
        "attach failed: {attach_resp}"
    );
    let mut envelopes: Vec<Value> = attach_resp["result"]["events"]
        .as_array()
        .expect("attach must return a replay events array")
        .clone();

    let mut iterations = 0;
    let is_done = |envs: &[Value]| {
        envs.iter()
            .any(|e| e["kind"].as_str() == Some("session_done"))
    };
    while !is_done(&envelopes) {
        iterations += 1;
        assert!(
            iterations < 20,
            "gave up waiting for session_done after {iterations} read rounds; \
             envelopes so far: {envelopes:?}"
        );
        let lines = daemon.read_lines(20).await;
        assert!(
            !lines.is_empty(),
            "timed out waiting for more events; envelopes so far: {envelopes:?}"
        );
        for line in &lines {
            if let Some(envelope) = find_session_event(line, &session_id) {
                envelopes.push(envelope);
            }
        }
    }

    envelopes
}

/// AC-13's direct acceptance proof: two SEQUENTIALLY-DISPATCHED delegations
/// (from one PM fan-out turn) to the SAME `agent_name` must carry DISTINCT
/// `agent_id`s on the real event stream, over the real wire.
///
/// Why: this is the exact regression #2862 left open, proven over the real
/// JSON-RPC wire against a real (subprocess) daemon — not just in-process
/// unit tests — per the mandatory API-driven e2e gate. See the module docs'
/// NOTE for why this specific test is sequential (the PM's own
/// `dispatch_all` dispatches one delegation at a time) rather than a
/// wall-clock race — that race property is proven separately at the
/// `runner` layer.
/// What: spawns the daemon with `TCODE_MOCK_LLM=echo-fanout`, runs the
/// scripted fan-out task to completion, filters the merged replay+live event
/// stream to `tool_started` events for tool `"bash"` (the delegated
/// engineers' own tool calls, as opposed to the PM's `delegate_to_agent`
/// calls), and asserts: exactly two such events, both attributed to
/// `agent == "python-engineer"`, with two DIFFERENT non-empty `agent_id`
/// values.
/// Test: this test.
#[tokio::test]
async fn fanned_out_same_named_delegations_get_distinct_agent_ids_over_the_wire() {
    let project = project_with_agents();
    let mut daemon = StdioSession::spawn_with_mock_llm_variant(
        project.path(),
        trusty_code::task::mock_llm::MOCK_LLM_ECHO_FANOUT,
    );

    let envelopes = run_fanout_task_to_completion(&mut daemon).await;

    let bash_starts: Vec<&Value> = envelopes
        .iter()
        .filter(|e| {
            e["kind"].as_str() == Some("tool_started")
                && e["event"]["tool"].as_str() == Some("bash")
        })
        .collect();

    assert_eq!(
        bash_starts.len(),
        2,
        "expected exactly two delegated bash tool_started events (one per \
         engineer spawn); got: {bash_starts:?}\nall envelopes: {envelopes:?}"
    );

    for ev in &bash_starts {
        assert_eq!(
            ev["event"]["agent"].as_str(),
            Some("python-engineer"),
            "both delegated bash calls must attribute to the SAME agent name: {ev:?}"
        );
    }

    let agent_id_a = bash_starts[0]["event"]["agent_id"]
        .as_str()
        .expect("agent_id must be a string");
    let agent_id_b = bash_starts[1]["event"]["agent_id"]
        .as_str()
        .expect("agent_id must be a string");

    assert!(!agent_id_a.is_empty(), "agent_id must not be empty");
    assert!(!agent_id_b.is_empty(), "agent_id must not be empty");
    assert_ne!(
        agent_id_a, agent_id_b,
        "DOC-39 AC-13: two delegations to the SAME agent_name must mint \
         DISTINCT agent_ids so a UI client can tell them apart on the event \
         stream — agent alone cannot, since both are \"python-engineer\""
    );
}
