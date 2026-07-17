//! End-to-end acceptance proof for DOC-39 Slice C (recalled TEXT + `run_id`
//! on `Event::MemoryRecalled`) — the mandatory API-driven e2e gate for this
//! slice (Bob's standing testability directive).
//!
//! Why: the "what memory drove this / what was held back" UI surface (e.g.
//! "PKCE required… 41% · held") needs the ACTUAL recalled text of a held-back
//! result, not just its score and the `injected: false` flag —
//! `tools::recall_session::recall_telemetry` and `events::RecalledMemory`'s
//! new `text`/`run_id` fields are already covered by unit tests
//! (`tools::recall_session::tests::telemetry_carries_recalled_text_and_run_id`,
//! `events_tests::recalled_memory_round_trips_through_json`), but those never
//! drive the REAL wire: the real `tcode serve --stdio` binary, a real
//! `recall_session` tool call, a real (mocked) trusty-memory HTTP backend,
//! and the real `session.attach` event stream. This is that proof.
//!
//! Uses `TCODE_MOCK_LLM=echo-recall`
//! ([`trusty_code::task::mock_llm::RecallEchoLlmClient`]) so the PM issues
//! exactly one `recall_session` tool call, no live model/API key required,
//! and `TRUSTY_MEMORY_URL` (`trusty_common::mcp::memory_rpc::TRUSTY_MEMORY_URL_ENV`)
//! pointed at an in-process mock trusty-memory `/rpc` server that returns two
//! results: one huge, high-scored entry that alone busts `recall_session`'s
//! token budget (so it alone is INJECTED), and one small, lower-scored entry
//! that is therefore HELD BACK whole — mirroring
//! `tools::recall_session::tests::telemetry_marks_budget_dropped_results_held_back`'s
//! shape, but observed over the real wire instead of asserted in-process. The
//! injected entry also carries a `run_id` in the mocked daemon response, to
//! prove that field threads through end-to-end too when present.
//!
//! What: spawns the real `tcode serve --stdio` binary, runs `task.run` +
//! `session.attach`, drains the replay + live event stream to
//! `session_done`, then asserts the ONE `memory_recalled` event's `results`
//! carry: the injected entry's full text AND `run_id`, and — the whole point
//! of this slice — the HELD-BACK entry's full text (proving the "what was
//! held back" debugging surface is reachable over the wire), with `run_id`
//! gracefully absent (`null`) rather than panicking when the daemon response
//! carries none.
//! Test: this module is itself the test surface.

mod support;

use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{Value, json};
use support::{StdioSession, find_session_event, project_with_agents};
use tokio::net::TcpListener;
use tokio::sync::watch;

/// The huge entry's recalled text — long enough (see `recall_session`'s own
/// `TOKEN_BUDGET = 4000` chars/4 heuristic) that it alone exceeds the token
/// budget, forcing the second, smaller entry to be dropped whole rather than
/// merged/truncated in.
fn huge_injected_text() -> String {
    "PKCE keystone context. ".repeat(1000)
}

/// The text of the result the token budget must hold back whole — this is
/// the exact string the test asserts survives onto `Event::MemoryRecalled`.
const HELD_BACK_TEXT: &str = "PKCE required for the OAuth token exchange — held back by budget";

/// The `run_id` the mocked daemon attaches to the INJECTED result, to prove
/// the field threads through end-to-end when present.
const INJECTED_RUN_ID: &str = "run-e2e-slice-c";

/// Spin up a mock trusty-memory `/rpc` server that answers ANY `memory_recall`
/// call with the fixed two-result fixture described in the module docs.
///
/// Why: `recall_session`'s client-side filter (`filter_and_cap`) only keeps
/// results tagged `session:<the real, daemon-assigned session id>` — a value
/// this test cannot know until AFTER `task.run` responds. Rather than racing
/// the mock server's startup against the daemon discovering the real session
/// id, the handler `.await`s a [`watch::Receiver`] until the test driver
/// publishes the real tag (via the returned [`watch::Sender`]) — so the mock
/// is correct regardless of scheduling, never a flaky race.
/// What: returns `(base_url, tag_tx)`; the caller must call
/// `tag_tx.send(Some(format!("session:{session_id}")))` once it knows the
/// real session id, before the daemon's `recall_session` call is expected to
/// resolve.
async fn spawn_recall_mock_daemon() -> (String, watch::Sender<Option<String>>) {
    let (tag_tx, tag_rx) = watch::channel(None::<String>);

    async fn handle(
        State(mut rx): State<watch::Receiver<Option<String>>>,
        Json(_body): Json<Value>,
    ) -> Json<Value> {
        // Block until the test driver knows the real session id and
        // publishes the matching tag — see the function's own docs.
        let tag = loop {
            if let Some(tag) = rx.borrow().clone() {
                break tag;
            }
            if rx.changed().await.is_err() {
                break String::new();
            }
        };

        let results = vec![
            json!({
                "content": huge_injected_text(),
                "score": 0.93,
                "tags": [tag, "turn"],
                "run_id": INJECTED_RUN_ID,
            }),
            json!({
                "content": HELD_BACK_TEXT,
                "score": 0.41,
                "tags": [tag, "turn"],
                // Deliberately no `run_id` — proves the tool defaults to
                // `None` rather than panicking when the daemon omits it.
            }),
        ];
        let inner =
            json!({"palace": "p", "query": "pkce oauth flow", "results": results}).to_string();
        let envelope = json!({"content": [{"type": "text", "text": inner}]});
        Json(json!({"jsonrpc": "2.0", "id": 1, "result": envelope}))
    }

    let app = Router::new().route("/rpc", post(handle)).with_state(tag_rx);
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind mock");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), tag_tx)
}

/// Drive `task.run` -> `session.attach` -> read-until-`session_done`,
/// returning every session event's full JSON envelope (not just its `kind`
/// like `support::run_task_to_completion` — this test needs the
/// `memory_recalled` event's full `results` payload).
async fn run_recall_task_to_completion(
    daemon: &mut StdioSession,
    tag_tx: &watch::Sender<Option<String>>,
) -> Vec<Value> {
    let run_resp = daemon
        .call(
            1,
            "task.run",
            json!({"task_description": "recall what we know about the pkce oauth flow"}),
        )
        .await;
    assert!(run_resp["error"].is_null(), "task.run failed: {run_resp}");
    let session_id = run_resp["result"]["session_id"]
        .as_str()
        .expect("task.run must return a session_id")
        .to_string();

    // Now that the real session id is known, publish the matching tag so the
    // mock daemon's client-side `session:<id>` filter keeps both results.
    let _ = tag_tx.send(Some(format!("session:{session_id}")));

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

/// DOC-39 Slice C's direct acceptance proof: a HELD-BACK recall result's
/// actual TEXT (not just its score) must reach `Event::MemoryRecalled` over
/// the real wire, and a `run_id` on the injected result must survive too.
///
/// Why: this is the exact "what memory drove this / what was held back"
/// debugging gap Slice C closes — before this ticket, `RecalledMemory` only
/// carried `score`/`injected`, so a held-back memory could be COUNTED by a
/// UI but never READ. Proven here against the real subprocess daemon per
/// Bob's mandatory API-driven e2e directive, not just the in-process unit
/// tests in `tools::recall_session::tests` and `events_tests`.
/// What: spawns the daemon with `TCODE_MOCK_LLM=echo-recall` and
/// `TRUSTY_MEMORY_URL` pointed at the mock trusty-memory server, runs the
/// scripted recall task to completion, finds the one `memory_recalled`
/// event, and asserts: exactly two results; the injected (first) result
/// carries its full text and `run_id`; the held-back (second) result carries
/// its full text with `injected: false` and a `null` `run_id`.
/// Test: this test.
#[tokio::test]
async fn held_back_recall_result_carries_its_text_and_run_id_over_the_wire() {
    let (mock_base_url, tag_tx) = spawn_recall_mock_daemon().await;
    let project = project_with_agents();
    let mut daemon = StdioSession::spawn_with_mock_llm_variant_and_env(
        project.path(),
        trusty_code::task::mock_llm::MOCK_LLM_ECHO_RECALL,
        &[(
            trusty_common::mcp::memory_rpc::TRUSTY_MEMORY_URL_ENV,
            &mock_base_url,
        )],
    );

    let envelopes = run_recall_task_to_completion(&mut daemon, &tag_tx).await;

    let recall_events: Vec<&Value> = envelopes
        .iter()
        .filter(|e| e["kind"].as_str() == Some("memory_recalled"))
        .collect();
    assert_eq!(
        recall_events.len(),
        1,
        "expected exactly one memory_recalled event; got: {recall_events:?}\n\
         all envelopes: {envelopes:?}"
    );

    let results = recall_events[0]["event"]["results"]
        .as_array()
        .expect("results must be an array");
    assert_eq!(
        results.len(),
        2,
        "expected both the injected and held-back result to be REPORTED \
         (only rendering, not telemetry, drops the held-back one): {results:?}"
    );

    let injected = &results[0];
    assert_eq!(injected["injected"], true);
    assert_eq!(
        injected["text"].as_str().unwrap_or_default(),
        huge_injected_text(),
        "the injected result's full text must reach the wire"
    );
    assert_eq!(
        injected["run_id"].as_str(),
        Some(INJECTED_RUN_ID),
        "a run_id present on the daemon response must thread through"
    );

    let held_back = &results[1];
    assert_eq!(
        held_back["injected"], false,
        "the second (lower-scored, over-budget) result must be held back"
    );
    assert_eq!(
        held_back["text"].as_str().unwrap_or_default(),
        HELD_BACK_TEXT,
        "THE point of this slice: a held-back result's actual TEXT must \
         still reach Event::MemoryRecalled, not just its score"
    );
    assert_eq!(
        held_back["run_id"],
        Value::Null,
        "run_id must gracefully default to null (never panic) when the \
         daemon response carries no such field"
    );
}
