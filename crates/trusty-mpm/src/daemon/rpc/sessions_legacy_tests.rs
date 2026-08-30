//! Contract tests for the legacy session, hook, and polled-event RPC methods
//! (#6288 slice 3).
//!
//! Why these are PARITY tests rather than "the socket answers 200-shaped JSON":
//! this slice's whole claim is that a route reached over the socket and the same
//! route reached over HTTP give the same answer AND leave the same state behind.
//! A test that only exercises the RPC side proves the method exists; it cannot
//! fail when the two transports drift, which is the failure the slice exists to
//! prevent. So every `parity_*` case builds the axum router AND the RPC router
//! from ONE `Arc<DaemonState>`, issues the equivalent request both ways, and
//! compares what came back — and, for a write, what the daemon then holds.
//!
//! ## Writes are compared by effect, not only by response
//!
//! Four routes here mutate. Each has a case that reads the state back:
//!
//! - `mpm.hooks.ingest` —
//!   [`parity_hooks_ingest_leaves_the_same_event_log_on_both_transports`]
//!   compares the two appended `HookEventRecord`s, and
//!   [`parity_hooks_session_start_auto_registers_on_both_transports`] compares
//!   the two auto-registered sessions.
//! - `mpm.sessions.pause` / `.resume` —
//!   [`parity_sessions_pause_leaves_the_same_state_on_both_transports`] pauses
//!   over one transport, reads the session record, resumes, pauses over the
//!   other, and compares the two records.
//! - `mpm.sessions.command` —
//!   [`parity_sessions_command_leaves_the_same_state_on_both_transports`]
//!   compares the registry snapshot on either side of each call. The write this
//!   route performs lands in tmux, not in daemon state, so the registry is what
//!   a hermetic test can observe; the tmux `send-keys` itself is one call in one
//!   shared body, reached identically from both transports.
//! - `mpm.sessions.delete` —
//!   [`parity_sessions_delete_agrees_across_transports`] deletes one of two
//!   identically-built sessions over each transport and asserts each is gone.
//! - `mpm.sessions.reap` —
//!   [`parity_sessions_reap_removes_the_dead_and_spares_the_live_on_both_transports`]
//!   gives reap one session it must remove and one it must leave, and compares
//!   both the answer and the surviving registry.
//!
//! ## The comparison allowlist
//!
//! Fields dropped before a comparison, and why each varies for a reason that is
//! not the transport:
//!
//! - `id` and `name` on `mpm.sessions.register` — a fresh UUID per call, and a
//!   tmux name derived from it.
//! - `session_id` on `mpm.sessions.pause` and `at` on an ingested hook record —
//!   the id of whichever session the case built, and a `Utc::now()` stamp.
//! - `paused_at` and `last_seen` on a compared `Session` — also
//!   `SystemTime::now()`, stamped per call.
//!
//! Nothing else is excused. Both transports run in one process against one
//! state, so any other difference would be a real finding.
//!
//! ## What is NOT here
//!
//! `mpm.sessions.discover` and `mpm.sessions.reap` both shell out to tmux, and
//! `mpm.sessions.output` / `.pane` capture a pane. The seam every case here uses
//! is the one the existing HTTP tests use: an empty hermetic registry, so
//! `TmuxService` resolves no session of its own and returns its documented empty
//! capture whether or not the host runs tmux. No case starts a tmux session.
//!
//! `mpm.sessions.discover` is the one route that still SEES the host's tmux, and
//! adopts what it sees, so
//! [`parity_sessions_discover_agrees_across_transports`] drains the host's
//! sessions with a throwaway call before comparing the two transports on the
//! drained state. That is what makes it deterministic on a developer machine
//! running six tmux sessions and in CI running none.
//!
//! Test: this file IS the test module for [`super`].

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;
use trusty_common::uds::server::{CODE_INVALID_PARAMS, RpcResponse, RpcRouter};

use super::{METHODS, register};
use crate::core::paths::FrameworkPaths;
use crate::core::session::{ControlModel, Session, SessionId, SessionStatus};
use crate::daemon::api;
use crate::daemon::error::{CODE_CONFLICT, CODE_NOT_FOUND};
use crate::daemon::state::DaemonState;

/// One daemon state rooted at an empty temp directory, plus the directory.
///
/// Why hermetic: the pause path mirrors its record to disk under the framework
/// root, and an empty temp root keeps that write off the developer's real
/// `~/.trusty-mpm`. The caller holds the `TempDir` for the test's life.
fn hermetic() -> (Arc<DaemonState>, TempDir) {
    let dir = tempfile::tempdir().expect("temp dir for hermetic DaemonState");
    let paths = FrameworkPaths::under(dir.path());
    (Arc::new(DaemonState::with_paths(&paths)), dir)
}

/// A hermetic state carrying one registered, Active session.
fn hermetic_with_session() -> (Arc<DaemonState>, SessionId, TempDir) {
    let (state, dir) = hermetic();
    let id = register_active(&state, "/tmp/p");
    (state, id, dir)
}

/// Register one Active session and return its id.
///
/// `project_path` is assigned AFTER construction rather than passed to
/// `Session::new`, and the distinction matters: the constructor derives
/// `tmux_name` as `tm-<folder>` from a project directory, so passing `/tmp/p`
/// would name every fixture session `tm-p` — which tmux prefix-matches to
/// whatever the developer's host is running, and the pane capture then returns a
/// real shell instead of the empty string a hermetic case expects. Leaving the
/// argument `None` keeps the UUID-derived unique name, and the filter case still
/// gets the `project_path` it needs, since `list_sessions_for_project` reads
/// that field rather than the `project` label.
fn register_active(state: &Arc<DaemonState>, project: &str) -> SessionId {
    let id = SessionId::new();
    let mut session = Session::new(id, project, ControlModel::Tmux, None);
    session.project_path = Some(std::path::PathBuf::from(project));
    session.status = SessionStatus::Active;
    state.register_session(session);
    id
}

/// The RPC router this slice registers, over `state`.
fn rpc_router(state: &Arc<DaemonState>) -> RpcRouter {
    register(RpcRouter::new(), state)
}

/// Drive one HTTP request through the real daemon router and decode the answer.
///
/// Why the real router rather than calling a handler directly: the parity claim
/// is about the ROUTE, so the path, the method, and the extractors all have to
/// participate. Calling the handler would skip exactly the decoding the socket
/// has to reproduce.
async fn http(
    state: &Arc<DaemonState>,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let request = Request::builder().method(method).uri(uri);
    let request = match body {
        Some(v) => request
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&v).expect("encode body")))
            .expect("build request"),
        None => request.body(Body::empty()).expect("build request"),
    };

    let response = api::router(Arc::clone(state))
        .oneshot(request)
        .await
        .expect("the router must answer");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read the response body");
    // Not every refusal carries a JSON body: a `StatusCode`-only one is empty,
    // and axum's own extractor rejection (an unknown `event` name on `/hooks`)
    // is plain text. Report those as `null` and as a JSON string rather than
    // failing the decode, so an error-parity case can still see the status.
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
    };
    (status, value)
}

/// Dispatch one JSON-RPC call against `router` and return the whole frame.
async fn rpc_frame(router: &RpcRouter, method: &str, params: Value) -> Value {
    let frame = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
    let response: RpcResponse = router
        .dispatch(&serde_json::to_vec(&frame).expect("encode the request frame"))
        .await;
    serde_json::to_value(response).expect("the response frame must serialise")
}

/// The `result` half of a call that must succeed.
async fn rpc_ok(router: &RpcRouter, method: &str, params: Value) -> Value {
    let frame = rpc_frame(router, method, params).await;
    assert!(
        frame.get("error").is_none(),
        "{method} must succeed, got {frame}"
    );
    frame
        .get("result")
        .cloned()
        .unwrap_or_else(|| panic!("{method} answered no result: {frame}"))
}

/// The `error` half of a call that must be refused.
async fn rpc_err(router: &RpcRouter, method: &str, params: Value) -> Value {
    let frame = rpc_frame(router, method, params).await;
    frame
        .get("error")
        .cloned()
        .unwrap_or_else(|| panic!("{method} must be refused, got {frame}"))
}

/// Assert the two transports answered the same JSON for one route.
///
/// `drop_fields` is the allowlist this module's doc records.
fn assert_same(method: &str, mut http_body: Value, mut rpc_result: Value, drop_fields: &[&str]) {
    for field in drop_fields {
        if let Some(map) = http_body.as_object_mut() {
            map.remove(*field);
        }
        if let Some(map) = rpc_result.as_object_mut() {
            map.remove(*field);
        }
    }
    assert_eq!(
        http_body, rpc_result,
        "{method} must answer identically over HTTP and the socket"
    );
}

/// One session as JSON, with the fields the allowlist excuses removed.
fn session_json(state: &Arc<DaemonState>, id: SessionId, drop_fields: &[&str]) -> Value {
    let session = state.session(id).expect("the session must still exist");
    let mut value = serde_json::to_value(session).expect("a Session must serialise");
    if let Some(map) = value.as_object_mut() {
        for field in drop_fields {
            map.remove(*field);
        }
    }
    value
}

// ── The method table ─────────────────────────────────────────────────────────

/// Why: the slice-7 client swap will dial these names by literal, with no
/// compile-time link to this table. Pinning the registered set here turns a
/// rename into a failing assertion rather than a consumer that silently reports
/// `method_not_found`.
/// Test: this function IS the test.
#[tokio::test]
async fn rpc_router_registers_every_documented_method() {
    let (state, _dir) = hermetic();
    let router = rpc_router(&state);
    let registered: Vec<&str> = router.method_names().collect();

    let mut documented: Vec<&str> = METHODS.to_vec();
    documented.sort_unstable();

    assert_eq!(
        registered, documented,
        "the router and METHODS must name the same set"
    );
    assert_eq!(
        METHODS.len(),
        16,
        "slice 3 owns sixteen names; a new one needs a row in sessions_legacy.rs's table too"
    );
}

/// Why: slice 2's twenty methods and slice 3's sixteen mount on ONE router, and
/// a name collision would silently replace a registration rather than fail.
/// Test: this function IS the test.
#[tokio::test]
async fn slice_two_and_slice_three_methods_do_not_collide() {
    let (state, _dir) = hermetic();
    let combined = register(
        super::super::core::register(RpcRouter::new(), &state),
        &state,
    );
    assert_eq!(
        combined.method_names().count(),
        super::super::core::METHODS.len() + METHODS.len(),
        "a collision would show as a shortfall here"
    );
}

/// Why: a method that takes arguments must refuse a payload it cannot decode,
/// with the reason, rather than running on a default.
/// Test: this function IS the test.
#[tokio::test]
async fn rpc_reports_invalid_params_for_an_undecodable_payload() {
    let (state, _dir) = hermetic();
    let error = rpc_err(&rpc_router(&state), "mpm.sessions.get", json!({"id": 42})).await;
    assert_eq!(error["code"], json!(CODE_INVALID_PARAMS), "{error}");
}

/// Why: `mpm.sessions.list` decodes `Option<SessionQuery>` so an ABSENT params
/// object works, and the risk that buys is a payload that is present but wrong
/// silently collapsing to `None` — the caller would then get every session
/// rather than an error, which is the worst possible answer to a typo. A wrong
/// TYPE must still be `invalid_params`.
/// Test: this function IS the test.
#[tokio::test]
async fn rpc_sessions_list_reports_invalid_params_for_a_wrong_typed_filter() {
    let (state, _dir) = hermetic();
    register_active(&state, "/tmp/alpha");

    let error = rpc_err(
        &rpc_router(&state),
        "mpm.sessions.list",
        json!({"project": 42}),
    )
    .await;
    assert_eq!(error["code"], json!(CODE_INVALID_PARAMS), "{error}");
    assert!(
        error["message"]
            .as_str()
            .unwrap_or_default()
            .contains("params do not decode"),
        "the refusal must say what failed, not answer with a list: {error}"
    );
}

/// Why: `params` is absent on a well-formed no-argument call, and the polled
/// feed is the one a script reaches for first.
/// Test: this function IS the test.
#[tokio::test]
async fn rpc_events_poll_answers_with_no_params() {
    let (state, _dir) = hermetic();
    let result = rpc_ok(&rpc_router(&state), "mpm.events.poll", Value::Null).await;
    assert_eq!(result["events"], json!([]), "{result}");
}

// ── Parity: reads ────────────────────────────────────────────────────────────

/// Test: this function IS the test.
#[tokio::test]
async fn parity_sessions_list_agrees_across_transports() {
    let (state, _id, _dir) = hermetic_with_session();
    let (status, body) = http(&state, "GET", "/sessions", None).await;
    assert_eq!(status, StatusCode::OK);
    let result = rpc_ok(&rpc_router(&state), "mpm.sessions.list", Value::Null).await;
    assert_eq!(
        body["sessions"].as_array().map(Vec::len),
        Some(1),
        "the fixture session must be visible: {body}"
    );
    assert_same("mpm.sessions.list", body, result, &[]);
}

/// Why the explicit `?project=`: passing the filter on both sides proves the
/// ARGUMENT survives the transport change rather than only the default.
/// Test: this function IS the test.
#[tokio::test]
async fn parity_sessions_list_project_filter_agrees_across_transports() {
    let (state, _dir) = hermetic();
    register_active(&state, "/tmp/alpha");
    register_active(&state, "/tmp/beta");

    let (status, body) = http(&state, "GET", "/sessions?project=/tmp/alpha", None).await;
    assert_eq!(status, StatusCode::OK);
    let result = rpc_ok(
        &rpc_router(&state),
        "mpm.sessions.list",
        json!({"project": "/tmp/alpha"}),
    )
    .await;
    assert_eq!(
        body["sessions"].as_array().map(Vec::len),
        Some(1),
        "the filter must reach the body: {body}"
    );
    assert_same("mpm.sessions.list", body, result, &[]);
}

/// Test: this function IS the test.
#[tokio::test]
async fn parity_get_session_agrees_across_transports() {
    let (state, id, _dir) = hermetic_with_session();
    let (status, body) = http(&state, "GET", &format!("/sessions/{}", id.0), None).await;
    assert_eq!(status, StatusCode::OK);
    let result = rpc_ok(
        &rpc_router(&state),
        "mpm.sessions.get",
        json!({"id": id.0.to_string()}),
    )
    .await;
    assert_same("mpm.sessions.get", body, result, &[]);
}

/// Why: an unknown id is the error class every consumer of this family hits
/// first, so it must carry the same code and the same message either way
/// (requirement 3).
/// Test: this function IS the test.
#[tokio::test]
async fn parity_get_session_unknown_id_agrees_across_transports() {
    let (state, _dir) = hermetic();
    let unknown = SessionId::new().0.to_string();

    let (status, body) = http(&state, "GET", &format!("/sessions/{unknown}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let error = rpc_err(
        &rpc_router(&state),
        "mpm.sessions.get",
        json!({"id": unknown}),
    )
    .await;
    assert_eq!(error["code"], json!(CODE_NOT_FOUND), "{error}");
    assert_eq!(
        error["message"], body["error"],
        "the socket must carry the HTTP body's message verbatim"
    );
}

/// Why: a malformed id is a DIFFERENT class from an unknown one — 400 rather
/// than 404 — and collapsing the two in the move to the socket would cost the
/// caller the difference between "retry with a UUID" and "that session is gone".
/// Test: this function IS the test.
#[tokio::test]
async fn parity_get_session_malformed_id_agrees_across_transports() {
    let (state, _dir) = hermetic();
    let (status, body) = http(&state, "GET", "/sessions/not-a-uuid", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let error = rpc_err(
        &rpc_router(&state),
        "mpm.sessions.get",
        json!({"id": "not-a-uuid"}),
    )
    .await;
    assert_eq!(error["code"], json!(CODE_INVALID_PARAMS), "{error}");
    assert_eq!(error["message"], body["error"], "{error}");
}

/// Test: this function IS the test.
#[tokio::test]
async fn parity_events_poll_agrees_across_transports() {
    let (state, _id, _dir) = hermetic_with_session();
    let (status, body) = http(&state, "GET", "/events/poll", None).await;
    assert_eq!(status, StatusCode::OK);
    let result = rpc_ok(&rpc_router(&state), "mpm.events.poll", Value::Null).await;
    assert_same("mpm.events.poll", body, result, &[]);
}

/// Test: this function IS the test.
#[tokio::test]
async fn parity_session_events_poll_agrees_across_transports() {
    let (state, id, _dir) = hermetic_with_session();
    let (status, body) = http(
        &state,
        "GET",
        &format!("/sessions/{}/events/poll", id.0),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let result = rpc_ok(
        &rpc_router(&state),
        "mpm.sessions.events_poll",
        json!({"id": id.0.to_string()}),
    )
    .await;
    assert_same("mpm.sessions.events_poll", body, result, &[]);
}

/// Why the explicit `?lines=`: it proves the query argument reaches the body,
/// and the response echoes it, so a dropped argument fails loudly.
/// Test: this function IS the test.
#[tokio::test]
async fn parity_sessions_output_agrees_across_transports() {
    let (state, id, _dir) = hermetic_with_session();
    let (status, body) = http(
        &state,
        "GET",
        &format!("/sessions/{}/output?lines=7", id.0),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let result = rpc_ok(
        &rpc_router(&state),
        "mpm.sessions.output",
        json!({"id": id.0.to_string(), "lines": 7}),
    )
    .await;
    assert_eq!(body["lines"], json!(7), "the argument must reach the body");
    assert_same("mpm.sessions.output", body, result, &[]);
}

/// Why: `/sessions/{id}/pane` and `/sessions/{id}/output` are one axum handler
/// under two paths, and `mpm.sessions.pane` must stay the same alias rather than
/// quietly becoming a second implementation.
/// Test: this function IS the test.
#[tokio::test]
async fn parity_sessions_pane_matches_output_on_both_transports() {
    let (state, id, _dir) = hermetic_with_session();
    let (status, pane_body) = http(&state, "GET", &format!("/sessions/{}/pane", id.0), None).await;
    assert_eq!(status, StatusCode::OK);
    let (_, output_body) = http(&state, "GET", &format!("/sessions/{}/output", id.0), None).await;
    assert_eq!(pane_body, output_body, "the two HTTP paths are one handler");

    let router = rpc_router(&state);
    let params = json!({"id": id.0.to_string()});
    let pane = rpc_ok(&router, "mpm.sessions.pane", params.clone()).await;
    let output = rpc_ok(&router, "mpm.sessions.output", params).await;
    assert_eq!(pane, output, "the two method names are one body");
    assert_same("mpm.sessions.pane", pane_body, pane, &[]);
}

/// Why: an unknown session on the read path must refuse identically, and it is
/// the deterministic 404 on this route — no tmux involved, the resolve fails
/// first.
/// Test: this function IS the test.
#[tokio::test]
async fn parity_sessions_output_unknown_agrees_across_transports() {
    let (state, _dir) = hermetic();
    let unknown = SessionId::new().0.to_string();

    let (status, body) = http(&state, "GET", &format!("/sessions/{unknown}/output"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let error = rpc_err(
        &rpc_router(&state),
        "mpm.sessions.output",
        json!({"id": unknown}),
    )
    .await;
    assert_eq!(error["code"], json!(CODE_NOT_FOUND), "{error}");
    assert_eq!(error["message"], body["error"], "{error}");
}

/// Why: `discover` and `reap` both shell out to tmux, so what they FIND varies
/// with the host. What must not vary is that the two transports agree on the
/// same host in the same process — which is exactly what comparing them here
/// proves, whatever the machine has running.
/// Test: this function IS the test.
#[tokio::test]
async fn parity_sessions_reap_agrees_across_transports() {
    let (state, _dir) = hermetic();
    let (status, body) = http(&state, "DELETE", "/sessions/dead", None).await;
    assert_eq!(status, StatusCode::OK);
    let result = rpc_ok(&rpc_router(&state), "mpm.sessions.reap", Value::Null).await;
    assert_same("mpm.sessions.reap", body, result, &[]);
}

/// Why the first call is thrown away: discovery ADOPTS what it finds, so on a
/// machine running tmux the first call reports every live session and the second
/// reports none — comparing call one against call two would fail for a reason
/// that is the route's idempotence, not the transport's. Draining first puts
/// both transports on the same footing, and the comparison then holds on a
/// machine with tmux and on one without.
/// Why this is the reap case that matters: the empty-registry case above proves
/// the two transports agree when there is nothing to do, which a broken
/// registration would also pass. This one gives reap something to REMOVE and
/// something it must LEAVE, then runs it once per transport and compares both
/// the answer and the registry left behind.
///
/// The survivor is a `SessionHost::Native` session, not a live tmux one, and
/// that choice is what keeps the case deterministic: `reap_against` skips every
/// non-tmux session outright, so the survivor survives on a machine with tmux
/// and on one without, and no test here has to start a tmux server. The victim
/// is a tmux-origin session whose UUID-derived name no tmux server is hosting.
///
/// Whether the victim is actually removed depends on what reap can SEE of
/// tmux, and the precondition has to be the same question reap asks.
/// `DaemonState::reap_dead_sessions` reaps nothing when `driver.list_sessions`
/// fails, deliberately, so that an unreachable tmux cannot wipe the registry —
/// and on a host where no tmux server has ever run that listing DOES fail,
/// with `error connecting to <socket> (No such file or directory)`, which
/// `TmuxDriver::list_sessions` does not classify as an empty server. A CI
/// runner is exactly that host. Probing `TmuxDriver::discover().is_ok()`
/// instead asks only whether the tmux BINARY resolves, which is true there and
/// made `main` red (#6411). So the probe below runs the listing itself.
/// The removal is asserted under that condition and the PARITY is asserted
/// unconditionally: whatever this host let reap do, both transports did the
/// same thing.
///
/// Why serial: `TmuxDriver::discover` consults the #5784 host-state gate, which
/// other tests in this binary move process-wide by overriding `$HOME`. One
/// landing between this test's two calls would let tmux be reachable for one
/// transport and refused for the other — failing the comparison while proving
/// nothing about either.
/// Test: this function IS the test.
#[serial_test::serial]
#[tokio::test]
async fn parity_sessions_reap_removes_the_dead_and_spares_the_live_on_both_transports() {
    use crate::core::session::SessionHost;

    let (state, _dir) = hermetic();
    // #6411: ask the question reap asks — can it LIST the live set? — not the
    // weaker "does the tmux binary resolve".
    let reap_sees_tmux = crate::daemon::tmux::TmuxDriver::discover()
        .and_then(|driver| driver.list_sessions())
        .is_ok();

    // The survivor: native origin, so the tmux liveness rule skips it entirely.
    let survivor = SessionId::new();
    let mut native = Session::new(survivor, "/tmp/native", ControlModel::Tmux, None);
    native.origin = SessionHost::Native;
    native.status = SessionStatus::Active;
    state.register_session(native);

    // The victim over HTTP: tmux origin, a name no tmux server hosts.
    let victim_http = register_active(&state, "/tmp/dead-http");
    let (status, body) = http(&state, "DELETE", "/sessions/dead", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        state.session(survivor).is_some(),
        "a native session is never reaped, whatever tmux says"
    );

    // The victim over the socket: same shape, same starting registry.
    let victim_rpc = register_active(&state, "/tmp/dead-rpc");
    let result = rpc_ok(&rpc_router(&state), "mpm.sessions.reap", Value::Null).await;

    assert_same("mpm.sessions.reap", body, result.clone(), &[]);
    assert!(
        state.session(survivor).is_some(),
        "the socket must spare it too"
    );
    if reap_sees_tmux {
        assert_eq!(
            result["removed"],
            json!(1),
            "with the tmux listing readable each transport must reap exactly its own victim: {result}"
        );
        assert!(state.session(victim_http).is_none(), "HTTP must reap it");
        assert!(state.session(victim_rpc).is_none(), "the socket must too");
        assert_eq!(
            state.list_sessions().len(),
            1,
            "only the native survivor may remain"
        );
    } else {
        assert_eq!(
            result["removed"],
            json!(0),
            "with the tmux listing unreadable neither transport may reap anything: {result}"
        );
        assert!(
            state.session(victim_http).is_some(),
            "nothing may be reaped"
        );
        assert!(state.session(victim_rpc).is_some(), "nor over the socket");
    }
}

/// Test: this function IS the test.
#[tokio::test]
async fn parity_sessions_discover_agrees_across_transports() {
    let (state, _dir) = hermetic();
    let (drain_status, _drained) = http(&state, "POST", "/sessions/discover", None).await;
    assert_eq!(drain_status, StatusCode::OK);

    let result = rpc_ok(&rpc_router(&state), "mpm.sessions.discover", Value::Null).await;
    let (status, body) = http(&state, "POST", "/sessions/discover", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        result["discovered"],
        json!(0),
        "a drained registry must adopt nothing more: {result}"
    );
    assert_same("mpm.sessions.discover", body, result, &[]);
}

// ── Parity: writes, compared by the state they leave behind ──────────────────

/// Why the dropped `id`/`name`: registration mints a fresh UUID per call, and
/// the tmux name is derived from it. Everything else about the two records —
/// project, workdir, status, the fact that a record exists at all — is compared.
/// Test: this function IS the test.
#[tokio::test]
async fn parity_sessions_register_agrees_across_transports() {
    let (state, _dir) = hermetic();
    let payload = json!({"project": "/tmp/reg", "project_path": null, "name": null});

    let (status, body) = http(&state, "POST", "/sessions", Some(payload.clone())).await;
    assert_eq!(status, StatusCode::OK);
    let http_id: SessionId = serde_json::from_value(body["id"].clone()).expect("an id");

    let result = rpc_ok(&rpc_router(&state), "mpm.sessions.register", payload).await;
    let rpc_id: SessionId = serde_json::from_value(result["id"].clone()).expect("an id");

    assert_same("mpm.sessions.register", body, result, &["id", "name"]);
    assert_eq!(state.list_sessions().len(), 2, "both writes must land");
    // The two records must differ only where the allowlist says they may.
    let drop = ["id", "tmux_name", "created_at", "last_seen"];
    assert_eq!(
        session_json(&state, http_id, &drop),
        session_json(&state, rpc_id, &drop),
        "the two transports must register the same session shape"
    );
}

/// Why `connect` gets its own case: it is a SECOND name over the same body, and
/// the thing that could break is the wiring, not the body — a `connect` that
/// registered nothing would still return a plausible id.
/// Test: this function IS the test.
#[tokio::test]
async fn parity_sessions_connect_agrees_across_transports() {
    let (state, _dir) = hermetic();
    let payload = json!({"project": "/tmp/conn"});

    let (status, body) = http(
        &state,
        "POST",
        "/api/v1/sessions/connect",
        Some(payload.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let result = rpc_ok(&rpc_router(&state), "mpm.sessions.connect", payload).await;

    assert_same("mpm.sessions.connect", body, result, &["id", "name"]);
    assert_eq!(state.list_sessions().len(), 2, "both writes must land");
}

/// Why two sessions rather than one deleted twice: the second delete of one
/// session would be a 404, which proves nothing about whether the FIRST removal
/// happened the same way. Two identically-built sessions, one deleted per
/// transport, compares the removal itself.
/// Test: this function IS the test.
#[tokio::test]
async fn parity_sessions_delete_agrees_across_transports() {
    let (state, _dir) = hermetic();
    let over_http = register_active(&state, "/tmp/del");
    let over_rpc = register_active(&state, "/tmp/del");

    let (status, body) = http(
        &state,
        "DELETE",
        &format!("/sessions/{}", over_http.0),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(state.session(over_http).is_none(), "HTTP must remove it");

    let result = rpc_ok(
        &rpc_router(&state),
        "mpm.sessions.delete",
        json!({"id": over_rpc.0.to_string()}),
    )
    .await;
    assert!(state.session(over_rpc).is_none(), "the socket must too");
    assert_eq!(state.list_sessions().len(), 0, "both removals must land");

    // `removed` echoes the id that was asked for, so only the shape is shared.
    assert_eq!(body["removed"], json!(over_http.0.to_string()), "{body}");
    assert_eq!(result["removed"], json!(over_rpc.0.to_string()), "{result}");
}

/// Why pause-read-resume-pause rather than two sessions: pause derives its
/// summary from the session it resolves, so running both transports against the
/// SAME session is the comparison that has no confound. The resume in the middle
/// puts the session back to where the first pause found it.
/// Test: this function IS the test.
#[tokio::test]
async fn parity_sessions_pause_leaves_the_same_state_on_both_transports() {
    let (state, id, _dir) = hermetic_with_session();
    let uri = format!("/sessions/{}/pause", id.0);
    let payload = json!({"summary": "mid-task"});
    let drop = ["paused_at", "last_seen"];

    let (status, body) = http(&state, "POST", &uri, Some(payload.clone())).await;
    assert_eq!(status, StatusCode::OK);
    let after_http = session_json(&state, id, &drop);
    assert_eq!(
        after_http["status"],
        json!("Paused"),
        "HTTP must actually pause it: {after_http}"
    );

    let router = rpc_router(&state);
    rpc_ok(
        &router,
        "mpm.sessions.resume",
        json!({"id": id.0.to_string()}),
    )
    .await;
    let result = rpc_ok(
        &router,
        "mpm.sessions.pause",
        json!({"id": id.0.to_string(), "summary": "mid-task"}),
    )
    .await;
    let after_rpc = session_json(&state, id, &drop);

    assert_same("mpm.sessions.pause", body, result, &["session_id"]);
    assert_eq!(
        after_http, after_rpc,
        "both transports must leave the same paused session"
    );
}

/// Why: resume is the state-clearing half, and a resume that answered `true`
/// without clearing `pause_summary` would pass a response-only comparison.
/// Test: this function IS the test.
#[tokio::test]
async fn parity_sessions_resume_leaves_the_same_state_on_both_transports() {
    let (state, id, _dir) = hermetic_with_session();
    let router = rpc_router(&state);
    let params = json!({"id": id.0.to_string()});
    let drop = ["paused_at", "last_seen"];

    rpc_ok(&router, "mpm.sessions.pause", params.clone()).await;
    let (status, body) = http(&state, "POST", &format!("/sessions/{}/resume", id.0), None).await;
    assert_eq!(status, StatusCode::OK);
    let after_http = session_json(&state, id, &drop);
    assert_eq!(after_http["pause_summary"], json!(null), "{after_http}");

    rpc_ok(&router, "mpm.sessions.pause", params.clone()).await;
    let result = rpc_ok(&router, "mpm.sessions.resume", params).await;
    let after_rpc = session_json(&state, id, &drop);

    assert_same("mpm.sessions.resume", body, result, &[]);
    assert_eq!(
        after_http, after_rpc,
        "both transports must leave the same resumed session"
    );
}

/// Why: resuming a session that is not paused is the 409 on this family, and a
/// transport that turned it into a success would silently clear state the
/// caller never asked to clear.
/// Test: this function IS the test.
#[tokio::test]
async fn parity_sessions_resume_unpaused_agrees_across_transports() {
    let (state, id, _dir) = hermetic_with_session();
    let (status, body) = http(&state, "POST", &format!("/sessions/{}/resume", id.0), None).await;
    assert_eq!(status, StatusCode::CONFLICT);

    let error = rpc_err(
        &rpc_router(&state),
        "mpm.sessions.resume",
        json!({"id": id.0.to_string()}),
    )
    .await;
    assert_eq!(error["code"], json!(CODE_CONFLICT), "{error}");
    assert_eq!(error["message"], body["error"], "{error}");
    assert_eq!(
        state.session(id).expect("still there").status,
        SessionStatus::Active,
        "a refused resume must not mutate the session"
    );
}

/// Why the registry snapshot rather than a tmux assertion: this route's write
/// lands in tmux, which a hermetic test has none of. What IS observable is that
/// neither transport disturbs the registry, and that both return the same
/// capture — and the tmux `send-keys` they skip is one call in one shared body.
/// Test: this function IS the test.
#[tokio::test]
async fn parity_sessions_command_leaves_the_same_state_on_both_transports() {
    let (state, id, _dir) = hermetic_with_session();
    let drop = ["last_seen"];
    let before = session_json(&state, id, &drop);
    let payload = json!({"command": "help"});

    let (status, body) = http(
        &state,
        "POST",
        &format!("/sessions/{}/command", id.0),
        Some(payload),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let after_http = session_json(&state, id, &drop);

    let result = rpc_ok(
        &rpc_router(&state),
        "mpm.sessions.command",
        json!({"id": id.0.to_string(), "command": "help"}),
    )
    .await;
    let after_rpc = session_json(&state, id, &drop);

    assert_same("mpm.sessions.command", body, result, &[]);
    assert_eq!(before, after_http, "HTTP must not disturb the registry");
    assert_eq!(after_http, after_rpc, "nor may the socket");
    assert_eq!(state.list_sessions().len(), 1, "no session may be added");
}

/// Why: a `Stopped` session is refused BEFORE anything is sent, and that refusal
/// is the one failure branch on this route that must not become a
/// warning-and-continue on either transport (the #6288 Fail-Open Check).
/// Test: this function IS the test.
#[tokio::test]
async fn parity_sessions_command_stopped_agrees_across_transports() {
    let (state, _dir) = hermetic();
    let id = SessionId::new();
    let mut session = Session::new(id, "/tmp/p", ControlModel::Tmux, None);
    session.status = SessionStatus::Stopped;
    state.register_session(session);

    let (status, body) = http(
        &state,
        "POST",
        &format!("/sessions/{}/command", id.0),
        Some(json!({"command": "help"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    let error = rpc_err(
        &rpc_router(&state),
        "mpm.sessions.command",
        json!({"id": id.0.to_string(), "command": "help"}),
    )
    .await;
    assert_eq!(error["code"], json!(CODE_CONFLICT), "{error}");
    assert_eq!(error["message"], body["error"], "{error}");
}

/// Test: this function IS the test.
#[tokio::test]
async fn parity_sessions_set_pid_agrees_across_transports() {
    let (state, id, _dir) = hermetic_with_session();
    let uri = format!("/sessions/{}/pid", id.0);

    let (status, body) = http(&state, "PATCH", &uri, Some(json!({"pid": 4242}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        state.session(id).expect("still there").pid,
        Some(4242),
        "HTTP must record the pid"
    );

    let result = rpc_ok(
        &rpc_router(&state),
        "mpm.sessions.set_pid",
        json!({"id": id.0.to_string(), "pid": 5353}),
    )
    .await;
    assert_eq!(
        state.session(id).expect("still there").pid,
        Some(5353),
        "the socket must record it too"
    );
    assert_same("mpm.sessions.set_pid", body, result, &["pid"]);
}

/// Why: an unknown id on a WRITE must refuse rather than silently no-op, which
/// is the failure mode a fail-open transport would introduce.
/// Test: this function IS the test.
#[tokio::test]
async fn parity_sessions_set_pid_unknown_agrees_across_transports() {
    let (state, _dir) = hermetic();
    let unknown = SessionId::new().0.to_string();

    let (status, body) = http(
        &state,
        "PATCH",
        &format!("/sessions/{unknown}/pid"),
        Some(json!({"pid": 1})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let error = rpc_err(
        &rpc_router(&state),
        "mpm.sessions.set_pid",
        json!({"id": unknown, "pid": 1}),
    )
    .await;
    assert_eq!(error["code"], json!(CODE_NOT_FOUND), "{error}");
    assert_eq!(error["message"], body["error"], "{error}");
}

// ── Parity: hook ingestion, compared by the event log ────────────────────────

/// Why the event log rather than the response: `mpm.hooks.ingest` answers with
/// nothing but the event name it accepted, so a transport that decoded the frame
/// and then dropped the record would pass a response-only comparison. The two
/// appended records are what actually prove the write happened twice.
/// Test: this function IS the test.
#[tokio::test]
async fn parity_hooks_ingest_leaves_the_same_event_log_on_both_transports() {
    let (state, id, _dir) = hermetic_with_session();
    let payload = json!({
        "session_id": id.0.to_string(),
        "event": "PostToolUse",
        "payload": {"tool": "Edit"},
    });

    let (status, body) = http(&state, "POST", "/hooks", Some(payload.clone())).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(state.recent_hook_events().len(), 1, "HTTP must append one");

    let result = rpc_ok(&rpc_router(&state), "mpm.hooks.ingest", payload).await;
    let events = state.recent_hook_events();
    assert_eq!(events.len(), 2, "the socket must append one too");

    assert_same("mpm.hooks.ingest", body, result, &[]);
    let mut first = serde_json::to_value(&events[0]).expect("a record must serialise");
    let mut second = serde_json::to_value(&events[1]).expect("a record must serialise");
    for value in [&mut first, &mut second] {
        if let Some(map) = value.as_object_mut() {
            map.remove("at");
        }
    }
    assert_eq!(
        first, second,
        "both transports must append the same record modulo its timestamp"
    );
}

/// Why: `SessionStart` for an unknown id AUTO-REGISTERS a session — the daemon's
/// connection-driven registration path, and the largest side effect on this
/// slice. A transport that ingested the event without registering would look
/// identical on the wire.
/// Test: this function IS the test.
#[tokio::test]
async fn parity_hooks_session_start_auto_registers_on_both_transports() {
    let (state, _dir) = hermetic();
    let over_http = SessionId::new();
    let over_rpc = SessionId::new();
    let event = |id: SessionId| json!({"session_id": id.0.to_string(), "event": "SessionStart", "payload": {}});

    let (status, body) = http(&state, "POST", "/hooks", Some(event(over_http))).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        state.session(over_http).is_some(),
        "HTTP must auto-register the session"
    );

    let result = rpc_ok(&rpc_router(&state), "mpm.hooks.ingest", event(over_rpc)).await;
    assert!(
        state.session(over_rpc).is_some(),
        "the socket must auto-register it too"
    );

    assert_same("mpm.hooks.ingest", body, result, &[]);
    let drop = ["id", "tmux_name", "created_at", "last_seen"];
    assert_eq!(
        session_json(&state, over_http, &drop),
        session_json(&state, over_rpc, &drop),
        "both transports must register the same session shape"
    );
}

/// Why: a malformed session id must be refused before anything is appended, on
/// both transports — a fail-open socket would swallow the id error and leave a
/// record attributed to nothing.
/// Test: this function IS the test.
#[tokio::test]
async fn parity_hooks_ingest_malformed_id_agrees_across_transports() {
    let (state, _dir) = hermetic();
    let payload = json!({"session_id": "not-a-uuid", "event": "PostToolUse", "payload": {}});

    let (status, body) = http(&state, "POST", "/hooks", Some(payload.clone())).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(state.recent_hook_events().len(), 0, "nothing may be logged");

    let error = rpc_err(&rpc_router(&state), "mpm.hooks.ingest", payload).await;
    assert_eq!(error["code"], json!(CODE_INVALID_PARAMS), "{error}");
    assert_eq!(error["message"], body["error"], "{error}");
    assert_eq!(state.recent_hook_events().len(), 0, "still nothing logged");
}

/// Why: an event name that is not a known `HookEvent` is refused by the DECODER,
/// which is the one refusal that is genuinely per-transport — axum rejects the
/// body, and the RPC router rejects the params. Both must still refuse, and
/// neither may log.
/// Test: this function IS the test.
#[tokio::test]
async fn parity_hooks_ingest_unknown_event_is_refused_on_both_transports() {
    let (state, id, _dir) = hermetic_with_session();
    let payload = json!({"session_id": id.0.to_string(), "event": "NotAnEvent", "payload": {}});

    let (status, _body) = http(&state, "POST", "/hooks", Some(payload.clone())).await;
    assert!(status.is_client_error(), "HTTP must refuse it: {status}");

    let error = rpc_err(&rpc_router(&state), "mpm.hooks.ingest", payload).await;
    assert_eq!(error["code"], json!(CODE_INVALID_PARAMS), "{error}");
    assert_eq!(state.recent_hook_events().len(), 0, "nothing may be logged");
}
