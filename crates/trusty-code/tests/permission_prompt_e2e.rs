//! API-driven end-to-end test for the permission prompt's round trip
//! (#3422): a real `tcode serve --stdio` daemon suspends a tool call on an
//! `ask` rule, publishes `permission_requested`, and releases the call when a
//! client answers over `session.permission.respond`.
//!
//! Why: #7948 shipped the daemon half and #3422 the TUI half, each proven by
//! its own unit tests on its own side of the socket. Both suites can pass
//! while the integrated behaviour is broken — a field renamed on the wire, an
//! event never published, a `request_id` that answers nothing. Only a run
//! through the real binary can tell, which is what the vision spec's §9
//! "100% CLI/API Testable" requirement asks for (see `tests/task_e2e.rs`'s
//! module docs for the same rationale).
//! What: [`permission_ask_suspends_a_tool_call_until_a_client_allows_it`]
//! drives a `task.run` whose delegated engineer's `bash` call matches an
//! `ask` rule, observes `permission_requested` over `session.attach`, answers
//! `allow_once`, then observes `permission_resolved` and the call actually
//! dispatching (`tool_finished`) before the session finishes.
//! [`permission_deny_refuses_the_call_and_the_run_continues`] answers `deny`
//! on the same fixture and asserts the refusal is recorded and recoverable —
//! the run still reaches `session_done`, because #7948 turns a refusal into a
//! tool result rather than a crash.
//! Test: this file IS the test; `support` holds the process/protocol
//! plumbing shared with `task_e2e.rs`.

mod support;

use serde_json::{Value, json};
use support::{StdioSession, find_response, find_session_event};

/// How many `read_lines` rounds [`pump_until`] will take before giving up.
/// The mock LLM is finite and deterministic, so this is a regression backstop,
/// not a tuning knob.
const MAX_PUMP_ROUNDS: usize = 30;

/// The JSON-RPC id this file uses for `session.permission.respond`. Ids 1 and
/// 2 are `task.run` and `session.attach`.
const RESPOND_REQUEST_ID: i64 = 3;

/// A throwaway project whose delegated engineer must ASK before running
/// `bash`, and whose PM is unconstrained.
///
/// Why: the `TCODE_MOCK_LLM=echo` script has the PM delegate and the engineer
/// run exactly one `bash` call, so a single `bash: ask` rule on the engineer
/// puts one — and only one — request in front of the client.
fn project_with_an_ask_rule() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("project tempdir");
    let agents = tmp.path().join(".claude").join("agents");
    std::fs::create_dir_all(&agents).expect("mkdir agents");
    std::fs::write(
        agents.join("pm.md"),
        "---\nname: pm\nmodel: openai/gpt-4o-mini\n---\n\nYou are the PM. Delegate work to python-engineer.\n",
    )
    .expect("write pm.md");
    std::fs::write(
        agents.join("python-engineer.md"),
        "---\nname: python-engineer\nmodel: deepseek/deepseek-chat\npermissions:\n  bash: ask\n---\n\nYou are a Python engineer.\n",
    )
    .expect("write python-engineer.md");
    tmp
}

/// Everything one `pump_until` round observed: session events in arrival
/// order, plus any JSON-RPC responses that arrived interleaved with them.
#[derive(Default)]
struct Observed {
    events: Vec<Value>,
    responses: Vec<Value>,
}

impl Observed {
    fn kinds(&self) -> Vec<&str> {
        self.events
            .iter()
            .filter_map(|e| e["kind"].as_str())
            .collect()
    }

    fn first_of(&self, kind: &str) -> Option<&Value> {
        self.events.iter().find(|e| e["kind"] == kind)
    }
}

/// Read NDJSON lines into `observed` until an envelope of `kind` has been
/// seen, returning it.
///
/// Why: a response and a notification can arrive in either order on this
/// wire, and `StdioSession::call` discards the notifications it skips — which
/// would throw away the very `permission_requested` this test exists to
/// observe. Classifying every line and keeping both halves is the only way to
/// answer a request and watch its resolution in the same stream.
async fn pump_until(
    daemon: &mut StdioSession,
    session_id: &str,
    observed: &mut Observed,
    kind: &str,
) -> Value {
    for round in 0..MAX_PUMP_ROUNDS {
        if let Some(found) = observed.first_of(kind) {
            return found.clone();
        }
        let lines = daemon.read_lines(20).await;
        assert!(
            !lines.is_empty(),
            "no further daemon output while waiting for `{kind}` (round {round}); \
             kinds so far: {:?}",
            observed.kinds()
        );
        for line in &lines {
            if let Some(envelope) = find_session_event(line, session_id) {
                observed.events.push(envelope);
            } else if let Some(response) = find_response(line, RESPOND_REQUEST_ID) {
                observed.responses.push(response);
            }
        }
    }
    panic!(
        "gave up waiting for `{kind}` after {MAX_PUMP_ROUNDS} rounds; kinds so far: {:?}",
        observed.kinds()
    );
}

/// Start a run against [`project_with_an_ask_rule`] and read up to the
/// suspended call, returning the daemon, the session id, everything observed
/// so far, and the `request_id` to answer.
async fn run_until_suspended(
    project: &std::path::Path,
) -> (StdioSession, String, Observed, String) {
    let mut daemon = StdioSession::spawn_with_mock_llm(project);

    let run_resp = daemon
        .call(1, "task.run", json!({"task_description": "say hi"}))
        .await;
    assert!(run_resp["error"].is_null(), "task.run failed: {run_resp}");
    let session_id = run_resp["result"]["session_id"]
        .as_str()
        .expect("task.run must return a session_id")
        .to_string();

    let attach_resp = daemon
        .call(2, "session.attach", json!({"session_id": &session_id}))
        .await;
    assert!(
        attach_resp["error"].is_null(),
        "attach failed: {attach_resp}"
    );
    let mut observed = Observed {
        events: attach_resp["result"]["events"]
            .as_array()
            .expect("attach must return a replay events array")
            .clone(),
        responses: Vec::new(),
    };

    // An `ask` parks the engineer's tool call inside the gate for up to five
    // minutes, so this cannot race past the request: the run physically
    // cannot finish until this client answers.
    let requested = pump_until(
        &mut daemon,
        &session_id,
        &mut observed,
        "permission_requested",
    )
    .await;

    let event = &requested["event"];
    assert_eq!(event["tool"], "bash", "the suspended call: {event}");
    assert_eq!(
        event["agent"], "python-engineer",
        "the asking agent: {event}"
    );
    assert_eq!(event["rule"], "bash", "the matched rule: {event}");
    assert!(
        event["subject"]
            .as_str()
            .expect("subject is a string")
            .contains("echo hello-from-mock-engineer"),
        "the redacted subject must name the command: {event}"
    );
    let request_id = event["request_id"]
        .as_str()
        .expect("permission_requested must carry a request_id")
        .to_string();

    assert!(
        !observed.kinds().contains(&"tool_finished"),
        "the call must NOT have dispatched before the answer; kinds: {:?}",
        observed.kinds()
    );

    (daemon, session_id, observed, request_id)
}

/// Answer `request_id` with `decision` over `session.permission.respond` and
/// assert the daemon accepted it.
async fn respond(
    daemon: &mut StdioSession,
    session_id: &str,
    observed: &mut Observed,
    request_id: &str,
    decision: &str,
) {
    daemon
        .write_request(
            RESPOND_REQUEST_ID,
            "session.permission.respond",
            json!({
                "session_id": session_id,
                "request_id": request_id,
                "decision": decision,
                "pattern": Value::Null,
            }),
        )
        .await;

    let resolved = pump_until(daemon, session_id, observed, "permission_resolved").await;
    assert_eq!(
        resolved["event"]["decision"], decision,
        "the daemon must resolve with the answer this client sent: {resolved}"
    );
    assert_eq!(
        resolved["event"]["source"], "client",
        "the resolution must be attributed to the client, not a timeout: {resolved}"
    );
    assert_eq!(
        resolved["event"]["request_id"], request_id,
        "the resolution must close the request that was opened: {resolved}"
    );

    let accepted = observed
        .responses
        .iter()
        .find(|r| r["id"] == RESPOND_REQUEST_ID)
        .unwrap_or_else(|| panic!("no response to session.permission.respond"));
    assert!(
        accepted["error"].is_null(),
        "session.permission.respond failed: {accepted}"
    );
    assert_eq!(accepted["result"]["accepted"], true, "{accepted}");
}

/// The #3422 round trip: a suspended call, a client answer, and the call
/// actually running afterwards.
#[tokio::test]
async fn permission_ask_suspends_a_tool_call_until_a_client_allows_it() {
    let project = project_with_an_ask_rule();
    let (mut daemon, session_id, mut observed, request_id) =
        run_until_suspended(project.path()).await;

    respond(
        &mut daemon,
        &session_id,
        &mut observed,
        &request_id,
        "allow_once",
    )
    .await;

    // The whole point of allowing: the call the gate held must now dispatch.
    let finished = pump_until(&mut daemon, &session_id, &mut observed, "tool_finished").await;
    assert_eq!(finished["event"]["tool"], "bash", "{finished}");
    assert_eq!(
        finished["event"]["success"], true,
        "an allowed call must run for real: {finished}"
    );

    pump_until(&mut daemon, &session_id, &mut observed, "session_done").await;
}

/// The refusal half: `deny` is recorded and the run continues, because #7948
/// turns a refusal into a recoverable tool result rather than an abort.
#[tokio::test]
async fn permission_deny_refuses_the_call_and_the_run_continues() {
    let project = project_with_an_ask_rule();
    let (mut daemon, session_id, mut observed, request_id) =
        run_until_suspended(project.path()).await;

    respond(&mut daemon, &session_id, &mut observed, &request_id, "deny").await;

    pump_until(&mut daemon, &session_id, &mut observed, "session_done").await;

    let dispatched = observed.events.iter().any(|e| {
        e["kind"] == "tool_finished"
            && e["event"]["tool"] == "bash"
            && e["event"]["success"] == true
    });
    assert!(
        !dispatched,
        "a denied `bash` must never dispatch successfully; kinds: {:?}",
        observed.kinds()
    );
}

/// #8184 SAFETY GATE, end to end: a DEFAULT `session.create`d session runs the
/// STOCK `pm` solo, and its own `bash` call suspends on the #3422 prompt.
///
/// Why: #8184 made the interactive default an agent that runs shell in the
/// user's real project root. The two tests above prove the prompt works for a
/// hand-written fixture card; only a run against the SHIPPED roster proves a
/// user actually gets prompted. The project deliberately ships no
/// `.claude/agents`, so `resolve_agent` falls back to the embedded `pm.md`.
/// What: `session.create` with no `delegate` (the TUI's own call), `attach`
/// BEFORE the run so a prompter is watching when the gate evaluates (#8100),
/// then `task.run` against that session. The `echo` mock's scripted
/// `delegate_to_agent` call has no tool to land on in a solo registry, so its
/// second scripted response — the `bash` call — is issued by `pm` itself.
/// Answers `deny` and asserts the command never ran. FAILS without `pm.md`'s
/// `permissions:` block: nothing suspends and `bash` dispatches unprompted.
/// Test: this test.
#[tokio::test]
async fn stock_solo_session_asks_before_the_pm_runs_bash() {
    let project = tempfile::tempdir().expect("project tempdir");
    let mut daemon = StdioSession::spawn_with_mock_llm(project.path());

    let create_resp = daemon
        .call(1, "session.create", json!({"task": "tcode tui session"}))
        .await;
    assert!(
        create_resp["error"].is_null(),
        "session.create failed: {create_resp}"
    );
    assert_eq!(
        create_resp["result"]["no_delegate"], true,
        "the default session must be solo: {create_resp}"
    );
    let session_id = create_resp["result"]["id"]
        .as_str()
        .expect("session.create must return an id")
        .to_string();

    let attach_resp = daemon
        .call(2, "session.attach", json!({"session_id": &session_id}))
        .await;
    assert!(
        attach_resp["error"].is_null(),
        "attach failed: {attach_resp}"
    );
    let mut observed = Observed {
        events: attach_resp["result"]["events"]
            .as_array()
            .expect("attach must return a replay events array")
            .clone(),
        responses: Vec::new(),
    };

    let run_resp = daemon
        .call(
            4,
            "task.run",
            json!({"task_description": "say hi", "session_id": &session_id}),
        )
        .await;
    assert!(run_resp["error"].is_null(), "task.run failed: {run_resp}");

    let requested = pump_until(
        &mut daemon,
        &session_id,
        &mut observed,
        "permission_requested",
    )
    .await;
    let event = &requested["event"];
    assert_eq!(event["tool"], "bash", "the suspended call: {event}");
    assert_eq!(
        event["agent"], "pm",
        "the STOCK top-level agent must be the one asking: {event}"
    );
    let request_id = event["request_id"]
        .as_str()
        .expect("permission_requested must carry a request_id")
        .to_string();

    respond(&mut daemon, &session_id, &mut observed, &request_id, "deny").await;
    pump_until(&mut daemon, &session_id, &mut observed, "session_done").await;

    let dispatched = observed.events.iter().any(|e| {
        e["kind"] == "tool_finished"
            && e["event"]["tool"] == "bash"
            && e["event"]["success"] == true
    });
    assert!(
        !dispatched,
        "a denied `bash` must never dispatch; kinds: {:?}",
        observed.kinds()
    );
}
