//! Hermetic integration test for [`trusty_code::tui_client::CodeEngine`]
//! against a mock HTTP daemon (issue #3415, DOC-50 §3.3/§3.4).
//!
//! Why: `CodeEngine` is the thin-client seam over a `tcode serve --http`
//! daemon — its correctness is entirely about the wire contract (which RPC
//! methods it calls, with what params, and how it turns the daemon's
//! responses/SSE events into `trusty_tui::ReplEvent`s), not about any real
//! agent-loop behaviour. A `wiremock`-backed mock daemon (mirroring
//! `crates/trusty-channels/tests/client_http.rs`'s pattern) lets this suite
//! assert on that wire contract without spawning the real `tcode` binary —
//! this file is deliberately NOT named `*_e2e.rs` (this crate's convention
//! for tests that drive the real binary, see `tests/support/mod.rs`'s
//! docs) since it never does.
//! What: one scenario per `TuiEngine` method this slice implements —
//! `setup` (session creation + initial workstream), `handle_input`
//! (streamed assistant output + a tool invocation, correlated by `call_id`),
//! `cancel_session` (asserts the daemon's `session.cancel` RPC was actually
//! called — the thin-client axiom, DOC-39 §2.1 C-2), `subscribe_workstream_events`
//! (a `WorkstreamActivationChanged` SSE event, asserting the wire field
//! names `new_active_id`/`prior_id` match DOC-48 §5.3 exactly), and
//! `CodeEngine::discover` (the full daemon-discovery path via
//! `TCODE_DAEMON_URL`, verified against a live mock daemon's `/health`).
//! Test: this file is the test.

use std::time::Duration;

use serde_json::json;
use tokio::sync::mpsc::unbounded_channel;
use trusty_code::tui_client::CodeEngine;
use trusty_code::tui_client::discovery::DAEMON_URL_ENV;
use trusty_tui::{ReplEvent, TuiEngine};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const SESSION_ID: &str = "sess-1";
const WS_1: &str = "ws-1";
const WS_2: &str = "ws-2";

/// Matches a `POST /rpc` body whose `"method"` field equals `.0` — `POST
/// /rpc` is a single endpoint carrying many logical methods, so matching on
/// `path("/rpc")` alone can't distinguish them; this is the custom
/// `wiremock::Match` every mock below layers on top of `path("/rpc")`.
struct RpcMethod(&'static str);

impl wiremock::Match for RpcMethod {
    fn matches(&self, request: &Request) -> bool {
        serde_json::from_slice::<serde_json::Value>(&request.body)
            .ok()
            .and_then(|v| v.get("method").and_then(|m| m.as_str()).map(str::to_string))
            .as_deref()
            == Some(self.0)
    }
}

/// Mount every `POST /rpc` method + `GET /health` this suite's scenarios
/// need, EXCEPT the per-scenario SSE routes (`GET /sessions/{id}/events`,
/// `GET /workstreams/{id}/events`) — those vary per test and are mounted by
/// each test individually.
async fn mount_common(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(ResponseTemplate::new(200))
        .mount(server)
        .await;

    Mock::given(method("POST"))
        .and(path("/rpc"))
        .and(RpcMethod("session.create"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {"id": SESSION_ID, "task": "tcode tui session", "status": "running"},
        })))
        .mount(server)
        .await;

    Mock::given(method("POST"))
        .and(path("/rpc"))
        .and(RpcMethod("workstream.list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "active_workstream_id": WS_1,
                "workstreams": [{"id": WS_1, "name": "Feature X"}],
            },
        })))
        .mount(server)
        .await;

    Mock::given(method("POST"))
        .and(path("/rpc"))
        .and(RpcMethod("task.run"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {"session_id": SESSION_ID, "status": "running"},
        })))
        .mount(server)
        .await;

    Mock::given(method("POST"))
        .and(path("/rpc"))
        .and(RpcMethod("session.cancel"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {},
        })))
        .mount(server)
        .await;

    Mock::given(method("POST"))
        .and(path("/rpc"))
        .and(RpcMethod("workstream.activate"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {"active_id": WS_2, "prior_id": WS_1},
        })))
        .mount(server)
        .await;
}

/// An SSE body for `GET /sessions/{SESSION_ID}/events`: a tool invocation
/// (`ToolStarted`), a chunk of assistant text (`Message`), then a terminal
/// `SessionDone` — the full shape `handle_input`'s scenario asserts on.
fn session_events_body() -> String {
    let tool_started = json!({
        "session_id": SESSION_ID, "seq": 1, "at": "2026-07-20T00:00:00Z", "kind": "tool_started",
        "event": {
            "type": "tool_started", "session_id": SESSION_ID, "agent": "pm", "agent_id": "",
            "tool": "bash", "call_id": "call-1", "args_preview": "ls",
        },
    });
    let message = json!({
        "session_id": SESSION_ID, "seq": 2, "at": "2026-07-20T00:00:01Z", "kind": "message",
        "event": {"type": "message", "session_id": SESSION_ID, "text": "hello from the daemon"},
    });
    let done = json!({
        "session_id": SESSION_ID, "seq": 3, "at": "2026-07-20T00:00:02Z", "kind": "session_done",
        "event": {"type": "session_done", "session_id": SESSION_ID, "status": "finished"},
    });
    format!("data: {tool_started}\n\ndata: {message}\n\ndata: {done}\n\n")
}

/// An SSE body for `GET /workstreams/{WS_1}/events`: one
/// `WorkstreamActivationChanged` event, wire-shaped exactly as DOC-48 §5.3
/// specifies (`new_active_id`/`prior_id`, NOT the drifted `new_id` DOC-50 §5
/// Slice 6's prose used).
fn workstream_activation_body() -> String {
    let activation = json!({
        "session_id": "",
        "event_type": "workstream_activation_changed",
        "payload": {
            "type": "workstream_activation_changed",
            "new_active_id": WS_2,
            "prior_id": WS_1,
        },
    });
    format!("data: {activation}\n\n")
}

/// `setup` must create a session via `session.create` and report the
/// daemon's active workstream — and, ahead of `trusty-tui` Slice 1.5's
/// synchronous `TuiEngine::commands()`/`picker()` accessors (#3428), must
/// have already populated `CodeEngine`'s pre-fetched caches for them.
#[tokio::test]
async fn setup_creates_session_and_reports_active_workstream() {
    let server = MockServer::start().await;
    mount_common(&server).await;

    let engine = CodeEngine::with_daemon_url(reqwest::Client::new(), server.uri(), None);
    let (tx, mut rx) = unbounded_channel();
    engine.setup(tx).await.expect("setup");

    let mut saw_workstream_updated = false;
    while let Ok(ev) = rx.try_recv() {
        if let ReplEvent::WorkstreamUpdated(ws) = ev {
            assert_eq!(ws.id, WS_1);
            assert_eq!(ws.name, "Feature X");
            saw_workstream_updated = true;
        }
    }
    assert!(
        saw_workstream_updated,
        "setup must emit WorkstreamUpdated for the daemon's active workstream"
    );
    assert_eq!(
        engine.commands().len(),
        1,
        "setup must populate the commands cache ahead of TuiEngine::commands() (#3428)"
    );
    assert_eq!(
        engine.picker("workstream").len(),
        1,
        "setup must populate the workstream picker cache ahead of TuiEngine::picker() (#3428)"
    );
}

/// `handle_input` on a chat line must call `task.run` against the current
/// session, then stream the response as `ReplEvent`s: a `ToolInvocation`
/// (carrying the daemon's `call_id`), an `AssistantOutput` chunk, and a
/// final `AssistantOutput{done: true}`.
#[tokio::test]
async fn handle_input_streams_assistant_output_and_tool_invocation() {
    let server = MockServer::start().await;
    mount_common(&server).await;
    Mock::given(method("GET"))
        .and(path(format!("/sessions/{SESSION_ID}/events")))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(session_events_body(), "text/event-stream"),
        )
        .mount(&server)
        .await;

    let engine = CodeEngine::with_daemon_url(reqwest::Client::new(), server.uri(), None);
    let (tx, mut rx) = unbounded_channel();
    engine.setup(tx.clone()).await.expect("setup");
    while rx.try_recv().is_ok() {} // drain setup's own events

    let keep_going = engine
        .handle_input("hello".to_string(), tx)
        .await
        .expect("handle_input");
    assert!(
        keep_going,
        "handle_input must return Ok(true) to keep the REPL running"
    );

    let mut events = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        events.push(ev);
    }

    assert!(
        events.iter().any(|e| matches!(
            e,
            ReplEvent::ToolInvocation { id, tool_name, result: None, .. }
                if id == "call-1" && tool_name == "bash"
        )),
        "expected a ToolInvocation for call-1/bash with no result yet; got {events:?}"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            ReplEvent::AssistantOutput { chunk, done: false, is_error: false }
                if chunk == "hello from the daemon"
        )),
        "expected an in-progress AssistantOutput chunk; got {events:?}"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            ReplEvent::AssistantOutput {
                done: true,
                is_error: false,
                ..
            }
        )),
        "expected a final AssistantOutput{{done: true}} on SessionDone; got {events:?}"
    );
}

/// `cancel_session` must call the daemon's `session.cancel` RPC — per the
/// thin-client axiom (DOC-39 §2.1 C-2), client-side render-stop alone is
/// never acceptable.
#[tokio::test]
async fn cancel_session_calls_session_cancel_rpc() {
    let server = MockServer::start().await;
    mount_common(&server).await;

    let engine = CodeEngine::with_daemon_url(reqwest::Client::new(), server.uri(), None);
    let (tx, _rx) = unbounded_channel();
    engine.setup(tx).await.expect("setup");

    engine.cancel_session().await.expect("cancel_session");

    let requests = server.received_requests().await.expect("received requests");
    let saw_cancel = requests.iter().any(|r| {
        r.url.path() == "/rpc"
            && serde_json::from_slice::<serde_json::Value>(&r.body)
                .ok()
                .and_then(|v| v.get("method").and_then(|m| m.as_str()).map(str::to_string))
                .as_deref()
                == Some("session.cancel")
    });
    assert!(
        saw_cancel,
        "cancel_session must call the daemon's session.cancel RPC, not just stop client-side \
         rendering (DOC-39 §2.1 C-2)"
    );
}

/// `subscribe_workstream_events` must open `GET /workstreams/{id}/events`
/// and translate a `WorkstreamActivationChanged` event into
/// `ReplEvent::WorkstreamActivationChanged`, preserving the DOC-48 §5.3 wire
/// field names (`new_active_id`/`prior_id`) exactly.
#[tokio::test]
async fn subscribe_workstream_events_emits_activation_changed_with_wire_field_names() {
    let server = MockServer::start().await;
    mount_common(&server).await;
    Mock::given(method("GET"))
        .and(path(format!("/workstreams/{WS_1}/events")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(workstream_activation_body(), "text/event-stream"),
        )
        .mount(&server)
        .await;
    // The subscription reconnects to ws-2's endpoint after observing the
    // activation change (DOC-48 §5.3 point 5) — give that reconnect a
    // well-formed (empty, never-ending in practice but fine for this
    // assertion) stream too, so the background task doesn't emit spurious
    // `ConnectionLost` events after the assertion below already passed.
    Mock::given(method("GET"))
        .and(path(format!("/workstreams/{WS_2}/events")))
        .respond_with(ResponseTemplate::new(200).set_body_raw("", "text/event-stream"))
        .mount(&server)
        .await;

    let engine = CodeEngine::with_daemon_url(reqwest::Client::new(), server.uri(), None);
    let (tx, mut rx) = unbounded_channel();
    engine.setup(tx.clone()).await.expect("setup");
    while rx.try_recv().is_ok() {} // drain setup's own events

    engine
        .subscribe_workstream_events(tx)
        .await
        .expect("subscribe_workstream_events");

    let event = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match rx.recv().await {
                Some(ev @ ReplEvent::WorkstreamActivationChanged { .. }) => return ev,
                Some(_) => continue,
                None => panic!("channel closed before observing WorkstreamActivationChanged"),
            }
        }
    })
    .await
    .expect("timed out waiting for WorkstreamActivationChanged");

    match event {
        ReplEvent::WorkstreamActivationChanged {
            new_active_id,
            prior_id,
        } => {
            assert_eq!(new_active_id, WS_2);
            assert_eq!(prior_id.as_deref(), Some(WS_1));
        }
        other => panic!("unexpected {other:?}"),
    }

    engine.shutdown().await.expect("shutdown");
}

/// `CodeEngine::discover` must find and verify a daemon named by
/// `TCODE_DAEMON_URL` (the highest-priority discovery source, DOC-50 §3.4
/// point 1), pinging its real `/health` route before returning.
#[tokio::test]
async fn discover_finds_daemon_via_env_var_override() {
    let server = MockServer::start().await;
    mount_common(&server).await;

    // SAFETY: test-only env mutation. This is the only test in this binary
    // that reads or writes `TCODE_DAEMON_URL`, so no cross-test lock is
    // needed (mirrors this crate's existing convention of a shared lock only
    // when more than one test in the same file touches the same var — see
    // `crate::task::mock_llm::MOCK_LLM_ENV_LOCK`).
    unsafe {
        std::env::set_var(DAEMON_URL_ENV, server.uri());
    }
    let result = CodeEngine::discover(None).await;
    unsafe {
        std::env::remove_var(DAEMON_URL_ENV);
    }

    let engine = result.expect("discover must succeed against a live mock daemon");
    assert_eq!(engine.daemon_url(), server.uri());
}
