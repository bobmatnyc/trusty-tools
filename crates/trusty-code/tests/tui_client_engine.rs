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

/// An SSE body for `GET /workstreams/{WS_1}/events`: one
/// `WorkstreamActivationChanged` event with `new_active_id: null` — the
/// "deactivated, no replacement active" case (DOC-48 §4.2/§4.3), verified
/// server-side by `workstreams::activation_tests` and forwarded to THIS
/// stream (not just the new active workstream's) by `workstreams::sse`'s
/// `classify()`, which matches on `prior_id == target` too.
fn workstream_deactivated_body() -> String {
    let deactivated = json!({
        "session_id": "",
        "event_type": "workstream_activation_changed",
        "payload": {
            "type": "workstream_activation_changed",
            "new_active_id": null,
            "prior_id": WS_1,
        },
    });
    format!("data: {deactivated}\n\n")
}

/// Count how many `POST /rpc` requests the mock daemon has received for
/// `method`.
async fn count_rpc_calls(server: &MockServer, method: &str) -> usize {
    server
        .received_requests()
        .await
        .expect("received requests")
        .iter()
        .filter(|r| {
            r.url.path() == "/rpc"
                && serde_json::from_slice::<serde_json::Value>(&r.body)
                    .ok()
                    .and_then(|v| v.get("method").and_then(|m| m.as_str()).map(str::to_string))
                    .as_deref()
                    == Some(method)
        })
        .count()
}

/// `setup` must create a session via `session.create` and report the
/// daemon's active workstream — and must have already populated
/// `CodeEngine`'s pre-fetched caches for `trusty-tui`'s synchronous
/// `TuiEngine::commands()`/`picker()` accessors (#3428).
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
        "setup must populate the commands cache for TuiEngine::commands() (#3428)"
    );
    let picker = engine
        .picker("workstream")
        .expect("setup must populate the workstream picker cache for TuiEngine::picker() (#3428)");
    assert_eq!(picker.items.len(), 1);
    assert_eq!(picker.dispatch_command, "/workstream activate");
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

/// Regression test for epic #3411's deferred Slice 3 review item: once
/// `pump_session_events` exhausts every reconnect attempt (`GET
/// /sessions/{id}/events` closing cleanly with no data, over and over)
/// without ever observing a terminal `SessionDone`/`SessionCancelled` event,
/// `handle_input` must return `Err` (not silently `Ok(true)`) AND the TUI
/// must already have received a VISIBLE `done: true, is_error: true`
/// `AssistantOutput` — not just a trail of `ConnectionLost`s that never clear
/// `ReplApp::busy`. That combination is what rules out the "TUI looks alive
/// with a stuck spinner and no error text" silent stall the review flagged.
/// Takes ~10s of real wall-clock time
/// (`SESSION_STREAM_MAX_RECONNECTS` reconnects at the fixed
/// `RECONNECT_BACKOFF`, neither test-overridable) — hermetic-but-slow beats
/// mocking away the exact timing this test asserts on.
///
/// `handle_input` is wrapped in a 30s `tokio::time::timeout` (well above the
/// real ~10s) — code-critic finding on PR #3482: this test's ENTIRE purpose
/// is guarding against an un-exhaustible reconnect loop, so if that bug (or
/// any regression like it) ever comes back, the un-timed-out `.await` would
/// hang this test binary forever — exactly the failure mode a CI runner
/// needs a manual kill to recover from, wrong for a test whose job is to
/// catch this. The timeout turns "hangs forever" into "fails deterministically."
#[tokio::test]
async fn handle_input_surfaces_visible_error_after_exhausting_reconnects() {
    let server = MockServer::start().await;
    mount_common(&server).await;
    // Every open of GET /sessions/{id}/events closes immediately with no
    // body — the "clean-but-premature close" case that used to `return
    // Ok(())` silently once reconnects ran out (`pump_session_events`'s
    // `Ok(Ok(None))` arm, before this fix).
    Mock::given(method("GET"))
        .and(path(format!("/sessions/{SESSION_ID}/events")))
        .respond_with(ResponseTemplate::new(200).set_body_raw("", "text/event-stream"))
        .mount(&server)
        .await;

    let engine = CodeEngine::with_daemon_url(reqwest::Client::new(), server.uri(), None);
    let (tx, mut rx) = unbounded_channel();
    engine.setup(tx.clone()).await.expect("setup");
    while rx.try_recv().is_ok() {} // drain setup's own events

    let result = tokio::time::timeout(
        Duration::from_secs(30),
        engine.handle_input("hello".to_string(), tx),
    )
    .await
    .expect(
        "handle_input must not hang — an un-exhaustible reconnect loop must fail this test \
         deterministically, not hang the test binary forever",
    );
    assert!(
        result.is_err(),
        "handle_input must return Err once every reconnect attempt is exhausted with no \
         terminal session event ever observed, not Ok(true)"
    );

    let mut saw_terminal_error = false;
    while let Ok(ev) = rx.try_recv() {
        if let ReplEvent::AssistantOutput {
            done: true,
            is_error: true,
            ..
        } = ev
        {
            saw_terminal_error = true;
        }
    }
    assert!(
        saw_terminal_error,
        "expected a done:true, is_error:true AssistantOutput before handle_input returned Err \
         — otherwise the TUI stalls silently (busy stays true, no error text rendered)"
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

    assert_eq!(
        count_rpc_calls(&server, "session.cancel").await,
        1,
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

    // Collect every event up through the first `WorkstreamActivationChanged`
    // (rather than skipping straight to it) — this fix also pushes
    // `WorkstreamUpdated`/`StatuslineUpdate` from the cache refresh that
    // precedes it (DOC-50 §5.3, Slice 6), and this test asserts on those
    // too, not just the activation-changed event itself.
    let events = tokio::time::timeout(Duration::from_secs(5), async {
        let mut seen = Vec::new();
        loop {
            match rx.recv().await {
                Some(ev @ ReplEvent::WorkstreamActivationChanged { .. }) => {
                    seen.push(ev);
                    return seen;
                }
                Some(ev) => seen.push(ev),
                None => panic!("channel closed before observing WorkstreamActivationChanged"),
            }
        }
    })
    .await
    .expect("timed out waiting for WorkstreamActivationChanged");

    let activation = events
        .iter()
        .find(|e| matches!(e, ReplEvent::WorkstreamActivationChanged { .. }))
        .expect("collected events must include WorkstreamActivationChanged");
    match activation {
        ReplEvent::WorkstreamActivationChanged {
            new_active_id,
            prior_id,
        } => {
            assert_eq!(new_active_id.as_deref(), Some(WS_2));
            assert_eq!(prior_id.as_deref(), Some(WS_1));
        }
        other => panic!("unexpected {other:?}"),
    }

    // The SAME cache refresh that produced the activation-changed event must
    // ALSO have pushed the status line's Workstream segment (DOC-50 §5.3's
    // "SSE-driven status-line display") — not just the structured event a
    // future picker/command layer consumes.
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ReplEvent::WorkstreamUpdated(_))),
        "expected a WorkstreamUpdated alongside the activation change; got {events:?}"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            ReplEvent::StatuslineUpdate(segs)
                if segs.iter().any(|s| matches!(s, trusty_tui::StatuslineSegment::Workstream { .. }))
        )),
        "expected a StatuslineUpdate carrying a Workstream segment; got {events:?}"
    );

    engine.shutdown().await.expect("shutdown");
}

/// Regression test (HIGH finding, code-critic review of PR #3436): a
/// `WorkstreamActivationChanged` event with `new_active_id: null` (this
/// workstream deactivated with no replacement active — a state the daemon
/// legitimately publishes, DOC-48 §4.2/§4.3) must NOT be silently dropped.
/// Before the original fix, `run_workstream_subscription` only matched
/// `new_active_id: Some(..)`, so the whole event fell through the `if let`
/// unhandled: no `ReplEvent` was sent and `refresh_workstream_cache` never
/// ran, leaving the TUI's status line and the `"workstream"` picker cache
/// stale indefinitely. This asserts BOTH halves of the fix: the cache
/// actually refreshes (a second `workstream.list` call beyond `setup()`'s
/// own), and the engine surfaces a structured signal — now that
/// `trusty-tui`'s `ReplEvent::WorkstreamActivationChanged` carries
/// `new_active_id: Option<String>`, deactivation-with-no-replacement is
/// `WorkstreamActivationChanged { new_active_id: None, prior_id: Some(..) }`
/// rather than a free-text `StatusMessage` fallback.
#[tokio::test]
async fn subscribe_workstream_events_refreshes_cache_on_deactivation_with_no_replacement() {
    let server = MockServer::start().await;
    mount_common(&server).await;
    Mock::given(method("GET"))
        .and(path(format!("/workstreams/{WS_1}/events")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(workstream_deactivated_body(), "text/event-stream"),
        )
        .mount(&server)
        .await;
    // `mount_common`'s `workstream.list` mock always reports WS_1 active —
    // fine for `setup()`'s own call, but this test's whole point is the
    // SECOND call (triggered by the SSE deactivation event's cache refresh)
    // observing a genuinely deactivated daemon. Override it with two
    // higher-priority (lower number = checked first, wiremock-rs) mocks:
    // the FIRST `workstream.list` call gets the original WS_1-active
    // response (`up_to_n_times(1)`); once that's exhausted, every
    // subsequent call falls through to the `active_workstream_id: null`
    // response — the real shape a client would see once the daemon
    // actually deactivated.
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
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/rpc"))
        .and(RpcMethod("workstream.list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "active_workstream_id": null,
                "workstreams": [{"id": WS_1, "name": "Feature X"}],
            },
        })))
        .with_priority(2)
        .mount(&server)
        .await;

    let engine = CodeEngine::with_daemon_url(reqwest::Client::new(), server.uri(), None);
    let (tx, mut rx) = unbounded_channel();
    engine.setup(tx.clone()).await.expect("setup");
    while rx.try_recv().is_ok() {} // drain setup's own events

    let calls_before = count_rpc_calls(&server, "workstream.list").await;
    assert_eq!(
        calls_before, 1,
        "setup must have called workstream.list exactly once so far"
    );

    engine
        .subscribe_workstream_events(tx)
        .await
        .expect("subscribe_workstream_events");

    // Half 1 of the fix: the cache must actually refresh on the `None` arm
    // — proven by a SECOND workstream.list call beyond setup()'s own.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if count_rpc_calls(&server, "workstream.list").await > calls_before {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("timed out waiting for refresh_workstream_cache to re-call workstream.list");

    // Half 2 of the fix: the engine must not go silent — a structured
    // `WorkstreamActivationChanged { new_active_id: None, .. }` must report
    // the deactivation. Collect every event up through it (rather than
    // discarding non-matching ones) so Half 3 below can also assert on the
    // `StatuslineUpdate` this same cache refresh sends — which, per
    // `run_workstream_subscription`, is sent BEFORE the activation-changed
    // event, so a "hunt and discard" loop that only looked for the latter
    // would have already thrown the `StatuslineUpdate` away.
    let events = tokio::time::timeout(Duration::from_secs(5), async {
        let mut seen = Vec::new();
        loop {
            match rx.recv().await {
                Some(ev @ ReplEvent::WorkstreamActivationChanged { .. }) => {
                    seen.push(ev);
                    return seen;
                }
                Some(ev) => seen.push(ev),
                None => panic!("channel closed before observing the deactivation event"),
            }
        }
    })
    .await
    .expect("timed out waiting for the deactivation event");

    let activation = events
        .iter()
        .find(|e| matches!(e, ReplEvent::WorkstreamActivationChanged { .. }))
        .expect("collected events must include WorkstreamActivationChanged");
    match activation {
        ReplEvent::WorkstreamActivationChanged {
            new_active_id: None,
            prior_id,
        } => {
            assert_eq!(
                prior_id.as_deref(),
                Some(WS_1),
                "expected WorkstreamActivationChanged{{new_active_id: None, prior_id: Some(WS_1)}}"
            );
        }
        other => panic!("expected new_active_id: None; got {other:?}"),
    }

    // Half 3 (code-critic finding on PR #3482, closing the asymmetric
    // coverage vs. `subscribe_workstream_events_emits_activation_changed_with_wire_field_names`,
    // which asserts the non-empty `StatuslineUpdate` for an activation): the
    // SAME cache refresh must ALSO clear the status line's `Workstream`
    // segment — an EMPTY segment list, not a stale one — so a regression
    // that broke only the deactivation -> `StatuslineUpdate` wiring (wrong
    // ordering vs. the `active_workstream` mutex write, sending before
    // `refresh_workstream_cache` actually commits, etc.) would fail this
    // test instead of passing every other green assertion here.
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ReplEvent::StatuslineUpdate(segs) if segs.is_empty())),
        "expected a StatuslineUpdate with an empty segment list clearing the Workstream segment \
         on deactivation; got {events:?}"
    );

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
