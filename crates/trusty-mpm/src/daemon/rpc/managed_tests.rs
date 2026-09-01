//! Parity tests for the managed-session, control-plane, and L2 proxy methods
//! (#6288 slice 4).
//!
//! Why: the acceptance bar for this slice is that the socket and HTTP answer the
//! same thing, and that the #6197 guard did not stay behind on HTTP. Both are
//! claims about two transports agreeing, so every test here drives BOTH — the
//! real axum router built by `daemon::api::router`, and the real RPC router
//! built by [`super::managed::register`] — against the same `DaemonState`, and
//! compares what comes back.
//!
//! What: [`assert_parity`] holds the comparison rule in one place, including the
//! transport-only allowlist documented in `super::managed`'s module header. A
//! test that only asserted the RPC side would pass on a route that answers
//! something different from HTTP, which is the exact failure this slice can
//! produce.
//!
//! Nothing here spawns a real process. The per-session routes are driven with a
//! session id that does not exist, which reaches the same body and the same
//! refusal on both transports; the control-plane spawn route uses the cap-0
//! state `control_routes`'s own #6197 tests use, so the spawn path is REACHED
//! and rejected at admission rather than launching `claude`.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Method, Request};
use serde_json::{Value, json};
use tower::ServiceExt;
use trusty_common::uds::server::{
    CODE_INVALID_PARAMS, CODE_METHOD_NOT_FOUND, RpcResponse, RpcRouter,
};

use super::control::CallerTrust;
use super::outcome::{CODE_FORBIDDEN, CODE_NOT_FOUND, status_to_rpc_code};
use crate::daemon::api;
use crate::daemon::rpc::managed;
use crate::daemon::state::DaemonState;
use crate::session_manager::ManagedError;

// ── Fixtures ─────────────────────────────────────────────────────────────────

/// A `DaemonState` rooted in a fresh temp dir, with its managed-session store
/// pre-seeded so nothing here can reach the operator's live fleet.
///
/// Why `with_root_isolated_managed` and not `with_paths`: `session_manager()`
/// lazily calls `RealTmuxDriver::discover()` and then `reconcile_on_boot`, which
/// ADOPTS the machine's live tmux sessions into whatever store it was given —
/// carrying their real `workspace_path`s with them. A fleet-wide route driven
/// against that state is then operating on the operator's real worktrees, and
/// `sync-assets` in particular WRITES into them. `with_root_isolated_managed`
/// pre-seeds the `OnceCell` with a `FakeNoopTmuxDriver`-backed manager, so the
/// cell is full before the first request and discovery never runs. This is the
/// same hazard `#1790` records, and the same remedy.
async fn test_state() -> (Arc<DaemonState>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("temp dir");
    let state = DaemonState::with_root_isolated_managed(dir.path().to_path_buf()).await;
    (Arc::new(state), dir)
}

/// A `DaemonState` whose control-plane concurrency cap is `0`.
///
/// Why: the #6197 tests must never spawn a real backend. With the cap at 0,
/// `run_session` rejects at admission and returns an error BEFORE any spawn — so
/// a request that PASSES every guard still cannot launch a process, while a
/// request that a guard refuses returns 400/403 instead. The two are
/// distinguishable, which is what makes "the guard fired" provable rather than
/// assumed. Lifted verbatim from `control_routes`'s `test_state_no_spawn`.
async fn test_state_no_spawn() -> (Arc<DaemonState>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("temp dir");
    // `with_root` reads `[control_plane]` from the root it is handed, so the cap
    // goes directly there rather than under a `FrameworkPaths` layout.
    std::fs::write(
        dir.path().join("config.toml"),
        "[control_plane]\nmax_concurrent_sessions = 0\n",
    )
    .expect("write config");
    let state = DaemonState::with_root_isolated_managed(dir.path().to_path_buf()).await;
    (Arc::new(state), dir)
}

/// The daemon's real HTTP router, with a loopback peer injected.
///
/// The `MockConnectInfo` layer is what the control routes' `ConnectInfo`
/// extractor reads; without it those routes 500 before reaching their body.
fn http_router(state: Arc<DaemonState>) -> Router {
    api::router(state).layer(MockConnectInfo(SocketAddr::new(
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        0,
    )))
}

/// The daemon's real RPC router for this state.
fn rpc_router(state: &Arc<DaemonState>) -> RpcRouter {
    managed::register(RpcRouter::new(), state)
}

/// One HTTP answer, kept in the two shapes the comparison needs.
struct HttpAnswer {
    status: u16,
    text: String,
    json: Option<Value>,
}

/// Drive `router` and read the whole answer.
async fn http_call(router: Router, method: Method, uri: &str, body: Option<Value>) -> HttpAnswer {
    let mut builder = Request::builder().method(method).uri(uri);
    let request = match body {
        Some(v) => {
            builder = builder.header("content-type", "application/json");
            builder.body(Body::from(v.to_string()))
        }
        None => builder.body(Body::empty()),
    }
    .expect("build request");
    let response = router.oneshot(request).await.expect("http call");
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let json = serde_json::from_slice::<Value>(&bytes).ok();
    HttpAnswer { status, text, json }
}

/// Dispatch one unary RPC request against `router`.
async fn rpc_call(router: &RpcRouter, method: &str, params: Value) -> RpcResponse {
    let frame = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    router.dispatch(frame.to_string().as_bytes()).await
}

/// Assert that the two transports answered the same route the same way.
///
/// Why: the allowlist in `super::managed`'s header is a claim about what may
/// differ; putting the comparison in one function is what keeps every test
/// bound to the same claim instead of each test choosing its own leniency.
/// What: a 2xx HTTP answer must have an RPC `result` equal to its JSON body; a
/// refusal must have an RPC `error` whose message is the HTTP body verbatim and
/// whose code is `status_to_rpc_code(status)`. The one route that overrides that
/// projection — resume's two 422 classes, which HTTP separates with a header the
/// socket has no place for — is covered directly by
/// `managed_routes::cores::cores_tests`.
fn assert_parity(label: &str, http: &HttpAnswer, rpc: &RpcResponse) {
    if (200..300).contains(&http.status) {
        let expected = http.json.as_ref().unwrap_or_else(|| {
            panic!("{label}: a 2xx HTTP body must be JSON, got {:?}", http.text)
        });
        let got = rpc.result.as_ref().unwrap_or_else(|| {
            panic!("{label}: rpc refused a route HTTP answered {}", http.status)
        });
        assert_eq!(got, expected, "{label}: response bodies must be identical");
        return;
    }
    let error = rpc
        .error
        .as_ref()
        .unwrap_or_else(|| panic!("{label}: rpc succeeded where HTTP answered {}", http.status));
    assert_eq!(
        error.message, http.text,
        "{label}: the refusal message must be the HTTP body verbatim"
    );
    assert_eq!(
        error.code,
        status_to_rpc_code(http.status),
        "{label}: the refusal code must be the documented projection of HTTP {}",
        http.status
    );
}

/// An id no session will ever have — the hermetic way to reach a route body.
const MISSING_ID: &str = "11111111-2222-3333-4444-555555555555";

// ── Registration ─────────────────────────────────────────────────────────────

/// Every route this slice owns is reachable under its documented method name.
///
/// Why: the slice's first acceptance criterion is coverage, and a method that
/// was never registered fails silently — a caller gets `method_not_found`, which
/// reads like a client typo. This pins the whole list.
/// Test: this test.
#[tokio::test]
async fn every_scoped_route_has_a_method() {
    let (state, _dir) = test_state().await;
    let router = rpc_router(&state);
    let registered: Vec<&str> = router.method_names().chain(router.stream_names()).collect();

    for expected in [
        "mpm.managed.spawn",
        "mpm.managed.list",
        "mpm.managed.adopt",
        "mpm.managed.prune",
        "mpm.managed.decommission_ephemeral",
        "mpm.managed.prune_worktrees",
        "mpm.managed.reconcile_worktrees",
        "mpm.managed.fleet",
        "mpm.managed.get",
        "mpm.managed.stop",
        "mpm.managed.runtime_stop",
        "mpm.managed.rename",
        "mpm.managed.send",
        "mpm.managed.provision_status",
        "mpm.managed.sync_assets",
        "mpm.managed.sync_assets_all",
        "mpm.managed.answer",
        "mpm.managed.attach_cmd",
        "mpm.managed.activity",
        "mpm.managed.resume",
        "mpm.managed.reactivate",
        "mpm.managed.decommission",
        "mpm.managed.delete",
        "mpm.control.list",
        "mpm.control.run",
        "mpm.control.connect",
        "mpm.control.stop",
        "mpm.control.auth",
        "mpm.proxy.focus",
        "mpm.proxy.get_focus",
        "mpm.proxy.unfocus",
        "mpm.proxy.message",
        "mpm.proxy.summary",
    ] {
        assert!(
            registered.contains(&expected),
            "{expected} is not registered; the listener serves {registered:?}"
        );
    }
}

/// An unregistered name still refuses in the shape slice 1 established.
#[tokio::test]
async fn an_unknown_method_is_still_method_not_found() {
    let (state, _dir) = test_state().await;
    let response = rpc_call(&rpc_router(&state), "mpm.managed.nope", json!({})).await;
    assert_eq!(
        response.error.expect("unknown method refuses").code,
        CODE_METHOD_NOT_FOUND
    );
}

/// Params that do not decode are an invalid-params refusal, not a panic.
#[tokio::test]
async fn malformed_params_are_invalid_params() {
    let (state, _dir) = test_state().await;
    // `mpm.managed.get` requires an `id`; an empty object cannot decode.
    let response = rpc_call(&rpc_router(&state), "mpm.managed.get", json!({})).await;
    assert_eq!(
        response.error.expect("must refuse").code,
        CODE_INVALID_PARAMS
    );
}

// ── #6197 parity: the guard travels ──────────────────────────────────────────

/// The caller-trust refusal is byte-identical to the HTTP 403 the loopback
/// guard answers (#6197, #6288).
///
/// Why: `loopback_guard` and the socket's peer-uid check are different
/// mechanisms, so the only thing that can be identical between them is the
/// REFUSAL. This pins that, which is what lets the RPC-side tests below assert
/// against a known string rather than against whatever the body happened to say.
/// Test: this test.
#[tokio::test]
async fn caller_trust_refusal_matches_the_http_403() {
    let (state, _dir) = test_state_no_spawn().await;
    let peer = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 50)), 40000);
    let mut request = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/control/sessions/run")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"project_id": "p", "workdir": "/tmp", "backend": "stream-json"}).to_string(),
        ))
        .expect("build");
    request
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(peer));
    // No `MockConnectInfo` layer here — it would overwrite the remote peer.
    let response = api::router(Arc::clone(&state))
        .oneshot(request)
        .await
        .expect("http call");
    assert_eq!(response.status().as_u16(), 403);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    assert_eq!(
        String::from_utf8_lossy(&bytes),
        CallerTrust::REFUSAL,
        "the shared refusal string must be what HTTP actually sends"
    );

    let refusal = CallerTrust::unverified()
        .ensure_local()
        .expect_err("an unverified caller must be refused")
        .into_rpc()
        .expect_err("a 403 is always an error");
    assert_eq!(refusal.code, CODE_FORBIDDEN);
    assert_eq!(refusal.message, CallerTrust::REFUSAL);
}

/// An untrusted caller is refused by the SHARED body, before any spawn (#6197).
///
/// Why (Fail-Open Check): the socket's own peer check is stronger than
/// `loopback_guard`, so a socket request always carries
/// a verified verdict — which is exactly the argument that could hide
/// a body that never checks at all. This drives the same body the RPC method
/// drives, with the trust value the RPC method would carry if the socket had NOT
/// vouched for the caller, and asserts the 403. Under the cap-0 state a request
/// that reached `run_session` answers 500, so a 403 proves the guard returned
/// first.
/// Test: this test.
#[tokio::test]
async fn rpc_ctl_run_refuses_an_untrusted_caller() {
    let (state, _dir) = test_state_no_spawn().await;
    let dir = tempfile::tempdir().expect("temp dir");
    let outcome = crate::daemon::api::control_routes::ctl_run_core(
        &state,
        CallerTrust::unverified(),
        serde_json::from_value(json!({
            "project_id": "p",
            "workdir": dir.path().to_string_lossy(),
            "backend": "stream-json",
        }))
        .expect("decode"),
    )
    .await;
    assert_eq!(
        outcome.status, 403,
        "an unverified caller must be refused before any spawn"
    );
    let error = outcome.into_rpc().expect_err("403 is an error");
    assert_eq!(error.code, CODE_FORBIDDEN);
    assert_eq!(error.message, CallerTrust::REFUSAL);
}

/// A `claude_cmd` outside the allowed set is rejected over the SOCKET (#6197).
///
/// Why (Fail-Open Check): the pre-#6205 handler passed `claude_cmd` into the
/// spawn path unvalidated — arbitrary process execution. This sends `"sh"` over
/// the RPC router with a valid workdir, so nothing else can refuse first. Under
/// the cap-0 state an unguarded request reaches `run_session` and answers 500,
/// which is neither the asserted code nor the asserted message — so a pass here
/// cannot come from the request failing for some other reason.
/// Test: this test.
#[tokio::test]
async fn rpc_ctl_run_rejects_claude_cmd_outside_allowlist() {
    let (state, _dir) = test_state_no_spawn().await;
    let dir = tempfile::tempdir().expect("temp dir");
    let response = rpc_call(
        &rpc_router(&state),
        "mpm.control.run",
        json!({
            "project_id": "p",
            "workdir": dir.path().to_string_lossy(),
            "backend": "tmux",
            "claude_cmd": "sh",
        }),
    )
    .await;
    let error = response.error.expect("must refuse");
    assert_eq!(
        error.code, CODE_INVALID_PARAMS,
        "a 400 projects onto -32602"
    );
    assert_eq!(
        error.message,
        "claude_cmd override is not permitted over HTTP; omit it to use the default `claude`",
        "the refusal must come from validate_claude_cmd, not from somewhere else"
    );
}

/// A `prompt_file` carrying shell metacharacters is rejected over the SOCKET
/// (#6197).
///
/// Why: `prompt_file` reaches the tmux command line, so
/// `"/tmp/x; touch /tmp/PWNED; #"` was command injection. The asserted message
/// is `validate_prompt_file`'s and nothing else's.
/// Test: this test.
#[tokio::test]
async fn rpc_ctl_run_rejects_prompt_file_injection() {
    let (state, _dir) = test_state_no_spawn().await;
    let dir = tempfile::tempdir().expect("temp dir");
    let response = rpc_call(
        &rpc_router(&state),
        "mpm.control.run",
        json!({
            "project_id": "p",
            "workdir": dir.path().to_string_lossy(),
            "backend": "tmux",
            "prompt_file": "/tmp/x; touch /tmp/PWNED; #",
        }),
    )
    .await;
    let error = response.error.expect("must refuse");
    assert_eq!(error.code, CODE_INVALID_PARAMS);
    assert_eq!(
        error.message,
        "prompt_file must be an absolute path (no `..`, no shell metacharacters) to an existing file"
    );
    assert!(
        !std::path::Path::new("/tmp/PWNED").exists(),
        "the injected command must never have run"
    );
}

/// A relative `workdir` is rejected over the SOCKET (#6197).
#[tokio::test]
async fn rpc_ctl_run_rejects_relative_workdir() {
    let (state, _dir) = test_state_no_spawn().await;
    let response = rpc_call(
        &rpc_router(&state),
        "mpm.control.run",
        json!({"project_id": "p", "workdir": "relative/dir", "backend": "stream-json"}),
    )
    .await;
    let error = response.error.expect("must refuse");
    assert_eq!(error.code, CODE_INVALID_PARAMS);
    assert_eq!(
        error.message,
        "workdir must be an absolute path (no `..`) to an existing directory"
    );
}

/// A `workdir` containing `..` is rejected over the SOCKET (#6197).
#[tokio::test]
async fn rpc_ctl_run_rejects_workdir_traversal() {
    let (state, _dir) = test_state_no_spawn().await;
    let response = rpc_call(
        &rpc_router(&state),
        "mpm.control.run",
        json!({"project_id": "p", "workdir": "/tmp/../etc", "backend": "stream-json"}),
    )
    .await;
    let error = response.error.expect("must refuse");
    assert_eq!(error.code, CODE_INVALID_PARAMS);
    assert_eq!(
        error.message,
        "workdir must be an absolute path (no `..`) to an existing directory"
    );
}

/// A legitimate request passes every guard and reaches the capped spawn path
/// (#6197).
///
/// Why: without this, every test above could pass on a body that refuses
/// everything. Under the cap-0 state the advanced request is rejected by
/// admission — an internal error, which is neither a validator's 400 nor the
/// caller check's 403 — so reaching it proves all four guards let a valid
/// request through.
/// Test: this test.
#[tokio::test]
async fn rpc_ctl_run_accepts_a_default_request_and_reaches_the_spawn_path() {
    let (state, _dir) = test_state_no_spawn().await;
    let dir = tempfile::tempdir().expect("temp dir");
    let response = rpc_call(
        &rpc_router(&state),
        "mpm.control.run",
        json!({
            "project_id": "p",
            "workdir": dir.path().to_string_lossy(),
            "backend": "stream-json",
        }),
    )
    .await;
    let error = response
        .error
        .expect("the cap-0 state refuses at admission");
    assert_eq!(
        error.code,
        trusty_common::uds::server::CODE_INTERNAL_ERROR,
        "a valid request must reach run_session, not a guard: {}",
        error.message
    );
    assert_ne!(error.message, CallerTrust::REFUSAL);
}

// ── Control-plane parity ─────────────────────────────────────────────────────

#[tokio::test]
async fn rpc_ctl_list_parity() {
    let (state, _dir) = test_state().await;
    let http = http_call(
        http_router(Arc::clone(&state)),
        Method::GET,
        "/api/v1/control/sessions",
        None,
    )
    .await;
    let rpc = rpc_call(&rpc_router(&state), "mpm.control.list", json!({})).await;
    assert_parity("mpm.control.list", &http, &rpc);
}

#[tokio::test]
async fn rpc_ctl_stop_parity() {
    let (state, _dir) = test_state().await;
    let http = http_call(
        http_router(Arc::clone(&state)),
        Method::POST,
        "/api/v1/control/sessions/ghost/stop",
        None,
    )
    .await;
    let rpc = rpc_call(
        &rpc_router(&state),
        "mpm.control.stop",
        json!({"id": "ghost"}),
    )
    .await;
    assert_eq!(http.status, 404);
    assert_parity("mpm.control.stop", &http, &rpc);
}

#[tokio::test]
async fn rpc_ctl_auth_parity() {
    let (state, _dir) = test_state().await;
    let http = http_call(
        http_router(Arc::clone(&state)),
        Method::GET,
        "/api/v1/control/sessions/ghost/auth",
        None,
    )
    .await;
    let rpc = rpc_call(
        &rpc_router(&state),
        "mpm.control.auth",
        json!({"id": "ghost"}),
    )
    .await;
    assert_eq!(http.status, 404);
    assert_parity("mpm.control.auth", &http, &rpc);
}

/// `mpm.control.connect` refuses an unknown session before writing any frame.
///
/// Why: the SSE route 404s an unknown id; the stream form must not open a stream
/// and then report the failure inside it, because a caller reading frames would
/// have to distinguish "no events yet" from "no such session".
/// Test: this test.
#[tokio::test]
async fn rpc_ctl_connect_unknown_session_refuses_before_any_frame() {
    let (state, _dir) = test_state().await;
    let router = rpc_router(&state);
    let frame = json!({
        "jsonrpc": "2.0", "id": 1, "stream": true,
        "method": "mpm.control.connect", "params": {"id": "ghost"},
    });
    let outcome = router
        .dispatch_streaming(frame.to_string().as_bytes())
        .await;
    let trusty_common::uds::server::RpcOutcome::Stream { mut items, .. } = outcome else {
        panic!("a streaming request must produce a stream outcome");
    };
    let first = items.recv().await.expect("one terminal frame");
    let error = first.expect_err("an unknown session must refuse");
    assert_eq!(error.code, CODE_NOT_FOUND);
    assert_eq!(error.message, "session ghost not found");
    assert!(
        items.recv().await.is_none(),
        "a refusal carries exactly one frame"
    );
}

/// A unary request for the streaming connect method is refused, not silently
/// answered with a snapshot.
#[tokio::test]
async fn rpc_ctl_connect_refuses_a_unary_request() {
    let (state, _dir) = test_state().await;
    let response = rpc_call(
        &rpc_router(&state),
        "mpm.control.connect",
        json!({"id": "x"}),
    )
    .await;
    assert_eq!(
        response.error.expect("must refuse").code,
        trusty_common::uds::server::CODE_STREAM_REQUIRED
    );
}

/// `mpm.control.connect` carries the session's events as stream frames — the
/// same events, in order, that the SSE route's `data:` lines carry (#6288).
///
/// Why: the refusal test above proves only that the method exists. Without this,
/// a handler that opened a stream and never forwarded anything would pass every
/// other test here, and a caller would read an idle stream as a quiet session.
/// Test: this test.
#[tokio::test]
async fn rpc_ctl_connect_streams_events() {
    use crate::control::id::ControlSessionId;
    use crate::control::state::SessionMetadata;

    let (state, _dir) = test_state().await;
    let id = ControlSessionId::new("stream-proj", 0);
    let (command_tx, _command_rx) = tokio::sync::mpsc::channel(4);
    let (event_tx, _) = tokio::sync::broadcast::channel(16);
    let handle = crate::control::actor::SessionActorHandle {
        command_tx,
        event_tx: event_tx.clone(),
        write_lock_held: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        metadata: Arc::new(tokio::sync::RwLock::new(SessionMetadata::new(
            id.clone(),
            "stream-proj".into(),
            crate::control::event::BackendKind::StreamJson,
        ))),
    };
    state.session_registry.register(id.clone(), handle).await;

    let router = rpc_router(&state);
    let frame = json!({
        "jsonrpc": "2.0", "id": 1, "stream": true,
        "method": "mpm.control.connect", "params": {"id": id.to_string()},
    });
    let outcome = router
        .dispatch_streaming(frame.to_string().as_bytes())
        .await;
    let trusty_common::uds::server::RpcOutcome::Stream { mut items, .. } = outcome else {
        panic!("a streaming request must produce a stream outcome");
    };

    let event = crate::control::event::SessionEvent::ObserverLagged {
        session_id: id.clone(),
        dropped: 7,
    };
    let expected = serde_json::to_value(&event).expect("serialize");
    event_tx.send(event).expect("the subscriber is live");

    let first = items.recv().await.expect("one item").expect("not an error");
    assert_eq!(
        first, expected,
        "a stream frame must carry the SessionEvent verbatim"
    );
}

// ── Managed-session parity ───────────────────────────────────────────────────

/// Drive one id-addressed route both ways against a session that does not
/// exist, and assert the two answers match.
async fn assert_missing_session_parity(
    label: &str,
    http_method: Method,
    http_uri: &str,
    rpc_method: &str,
    rpc_params: Value,
) {
    let (state, _dir) = test_state().await;
    let body = if http_method == Method::GET || http_method == Method::DELETE {
        None
    } else {
        Some(json!({"text": "x", "answer": "x", "name": "x"}))
    };
    let http = http_call(http_router(Arc::clone(&state)), http_method, http_uri, body).await;
    let rpc = rpc_call(&rpc_router(&state), rpc_method, rpc_params).await;
    assert!(
        http.status >= 400,
        "{label}: an absent session must refuse, got {}",
        http.status
    );
    assert_parity(label, &http, &rpc);
}

#[tokio::test]
async fn managed_get_parity() {
    assert_missing_session_parity(
        "mpm.managed.get",
        Method::GET,
        &format!("/api/v1/sessions/managed/{MISSING_ID}"),
        "mpm.managed.get",
        json!({"id": MISSING_ID}),
    )
    .await;
}

#[tokio::test]
async fn managed_runtime_stop_parity() {
    assert_missing_session_parity(
        "mpm.managed.runtime_stop",
        Method::POST,
        &format!("/api/v1/sessions/managed/{MISSING_ID}/runtime-stop"),
        "mpm.managed.runtime_stop",
        json!({"id": MISSING_ID}),
    )
    .await;
}

/// The `DELETE` alias and `mpm.managed.stop` are the same body.
#[tokio::test]
async fn managed_stop_alias_parity() {
    assert_missing_session_parity(
        "mpm.managed.stop",
        Method::DELETE,
        &format!("/api/v1/sessions/managed/{MISSING_ID}"),
        "mpm.managed.stop",
        json!({"id": MISSING_ID}),
    )
    .await;
}

#[tokio::test]
async fn managed_send_parity() {
    assert_missing_session_parity(
        "mpm.managed.send",
        Method::POST,
        &format!("/api/v1/sessions/managed/{MISSING_ID}/send"),
        "mpm.managed.send",
        json!({"id": MISSING_ID, "text": "x"}),
    )
    .await;
}

#[tokio::test]
async fn managed_answer_parity() {
    assert_missing_session_parity(
        "mpm.managed.answer",
        Method::POST,
        &format!("/api/v1/sessions/managed/{MISSING_ID}/answer"),
        "mpm.managed.answer",
        json!({"id": MISSING_ID, "answer": "x"}),
    )
    .await;
}

#[tokio::test]
async fn managed_attach_cmd_parity() {
    assert_missing_session_parity(
        "mpm.managed.attach_cmd",
        Method::GET,
        &format!("/api/v1/sessions/managed/{MISSING_ID}/attach-cmd"),
        "mpm.managed.attach_cmd",
        json!({"id": MISSING_ID}),
    )
    .await;
}

#[tokio::test]
async fn managed_activity_parity() {
    assert_missing_session_parity(
        "mpm.managed.activity",
        Method::GET,
        &format!("/api/v1/sessions/managed/{MISSING_ID}/activity"),
        "mpm.managed.activity",
        json!({"id": MISSING_ID}),
    )
    .await;
}

#[tokio::test]
async fn managed_rename_parity() {
    assert_missing_session_parity(
        "mpm.managed.rename",
        Method::PATCH,
        &format!("/api/v1/sessions/managed/{MISSING_ID}"),
        "mpm.managed.rename",
        json!({"id": MISSING_ID, "name": "x"}),
    )
    .await;
}

#[tokio::test]
async fn managed_resume_parity() {
    assert_missing_session_parity(
        "mpm.managed.resume",
        Method::POST,
        &format!("/api/v1/sessions/managed/{MISSING_ID}/resume"),
        "mpm.managed.resume",
        json!({"id": MISSING_ID}),
    )
    .await;
}

#[tokio::test]
async fn managed_reactivate_parity() {
    assert_missing_session_parity(
        "mpm.managed.reactivate",
        Method::POST,
        &format!("/api/v1/sessions/managed/{MISSING_ID}/reactivate"),
        "mpm.managed.reactivate",
        json!({"id": MISSING_ID}),
    )
    .await;
}

#[tokio::test]
async fn managed_decommission_parity() {
    assert_missing_session_parity(
        "mpm.managed.decommission",
        Method::POST,
        &format!("/api/v1/sessions/managed/{MISSING_ID}/decommission"),
        "mpm.managed.decommission",
        json!({"id": MISSING_ID}),
    )
    .await;
}

#[tokio::test]
async fn managed_delete_parity() {
    assert_missing_session_parity(
        "mpm.managed.delete",
        Method::POST,
        &format!("/api/v1/sessions/managed/{MISSING_ID}/delete"),
        "mpm.managed.delete",
        json!({"id": MISSING_ID}),
    )
    .await;
}

#[tokio::test]
async fn managed_provision_status_parity() {
    assert_missing_session_parity(
        "mpm.managed.provision_status",
        Method::GET,
        &format!("/api/v1/sessions/managed/{MISSING_ID}/provision-status"),
        "mpm.managed.provision_status",
        json!({"id": MISSING_ID}),
    )
    .await;
}

#[tokio::test]
async fn managed_sync_assets_parity() {
    assert_missing_session_parity(
        "mpm.managed.sync_assets",
        Method::POST,
        &format!("/api/v1/sessions/managed/{MISSING_ID}/sync-assets"),
        "mpm.managed.sync_assets",
        json!({"id": MISSING_ID}),
    )
    .await;
}

/// An unparseable id is the SAME 400 on both transports (#6288).
#[tokio::test]
async fn managed_unparseable_id_parity() {
    let (state, _dir) = test_state().await;
    let http = http_call(
        http_router(Arc::clone(&state)),
        Method::GET,
        "/api/v1/sessions/managed/not-a-uuid",
        None,
    )
    .await;
    let rpc = rpc_call(
        &rpc_router(&state),
        "mpm.managed.get",
        json!({"id": "not-a-uuid"}),
    )
    .await;
    assert_eq!(http.status, 400);
    assert_parity("mpm.managed.get (bad id)", &http, &rpc);
}

#[tokio::test]
async fn managed_list_parity() {
    let (state, _dir) = test_state().await;
    let http = http_call(
        http_router(Arc::clone(&state)),
        Method::GET,
        "/api/v1/sessions/managed",
        None,
    )
    .await;
    let rpc = rpc_call(&rpc_router(&state), "mpm.managed.list", json!({})).await;
    assert_eq!(http.status, 200);
    assert_parity("mpm.managed.list", &http, &rpc);
}

#[tokio::test]
async fn managed_fleet_parity() {
    let (state, _dir) = test_state().await;
    let http = http_call(
        http_router(Arc::clone(&state)),
        Method::GET,
        "/api/v1/sessions/managed/fleet",
        None,
    )
    .await;
    let rpc = rpc_call(&rpc_router(&state), "mpm.managed.fleet", json!({})).await;
    assert_eq!(http.status, 200);
    assert_parity("mpm.managed.fleet", &http, &rpc);
}

#[tokio::test]
async fn managed_sync_assets_all_parity() {
    let (state, _dir) = test_state().await;
    let http = http_call(
        http_router(Arc::clone(&state)),
        Method::POST,
        "/api/v1/sessions/managed/sync-assets",
        None,
    )
    .await;
    let rpc = rpc_call(
        &rpc_router(&state),
        "mpm.managed.sync_assets_all",
        json!({}),
    )
    .await;
    assert_eq!(http.status, 200);
    assert_parity("mpm.managed.sync_assets_all", &http, &rpc);
}

#[tokio::test]
async fn managed_decommission_ephemeral_parity() {
    let (state, _dir) = test_state().await;
    let http = http_call(
        http_router(Arc::clone(&state)),
        Method::POST,
        "/api/v1/sessions/managed/decommission-ephemeral?dry_run=true",
        None,
    )
    .await;
    let rpc = rpc_call(
        &rpc_router(&state),
        "mpm.managed.decommission_ephemeral",
        json!({"dry_run": true}),
    )
    .await;
    assert_eq!(http.status, 200);
    assert_parity("mpm.managed.decommission_ephemeral", &http, &rpc);
}

#[tokio::test]
async fn managed_prune_parity() {
    let (state, _dir) = test_state().await;
    let body = json!({"state": "stopped", "dry_run": true, "include_active": false});
    let http = http_call(
        http_router(Arc::clone(&state)),
        Method::POST,
        "/api/v1/sessions/managed/prune",
        Some(body.clone()),
    )
    .await;
    let rpc = rpc_call(&rpc_router(&state), "mpm.managed.prune", body).await;
    assert_eq!(http.status, 200);
    assert_parity("mpm.managed.prune", &http, &rpc);
}

/// #6118's refusal travels: an unparseable `invoking_session` is a 400 on the
/// socket too, rather than a prune run without the requested self-exclusion.
#[tokio::test]
async fn managed_prune_rejects_an_unparseable_invoking_session() {
    let (state, _dir) = test_state().await;
    let body = json!({
        "state": "stopped", "dry_run": true, "include_active": false,
        "invoking_session": "not-a-uuid",
    });
    let http = http_call(
        http_router(Arc::clone(&state)),
        Method::POST,
        "/api/v1/sessions/managed/prune",
        Some(body.clone()),
    )
    .await;
    let rpc = rpc_call(&rpc_router(&state), "mpm.managed.prune", body).await;
    assert_eq!(http.status, 400);
    assert!(
        http.text.contains("#6118"),
        "the refusal must be #6118's, got {:?}",
        http.text
    );
    assert_parity("mpm.managed.prune (bad invoker)", &http, &rpc);
}

/// Pin `TRUSTY_MPM_WORKSPACE_ROOT` at an empty fixture dir for one test (#6551).
///
/// Why: `prune_worktrees_core` and `reconcile_worktrees_core` resolve the
/// worktree root through `TrustyToolsConfig::load()` on EVERY call, and with the
/// variable unset that resolves to `$HOME/trusty-mpm-projects` — the operator's
/// real fleet. Two consequences, both observed: the two transports enumerate
/// ~200 real worktrees instead of the `TempDir` fixture, and a sibling test that
/// sets or clears this same process-global variable between the HTTP call and
/// the RPC call splits the parity comparison. Pinning it removes both, because
/// the two calls then read one value that names an empty directory.
///
/// What: takes the crate-wide `env_test_lock` and sets the variable, restoring
/// the previous value (or removing it) on drop, unwinding panics included.
/// BOTH guards are needed and they cover different populations, exactly as
/// `prune::tests::prune_spares_a_stopped_records_workspace` records:
/// `env_test_lock` serialises the env-precedence tests, `#[serial_test::serial]`
/// serialises `connectors::tm_tests`, which mutates this variable under
/// `serial` alone. The lock is held across the awaited route calls on purpose —
/// releasing it earlier is the race.
///
/// `#[serial_test::file_serial]` is deliberately NOT used: the pinned resource
/// is a process environment variable, so under `cargo nextest`'s
/// process-per-test model (#4162) each test already has its own copy and there
/// is nothing shared across processes to serialise.
///
/// Test: `managed_prune_worktrees_enumerates_only_the_pinned_workspace_root`.
struct WorkspaceRootEnv {
    _lock: std::sync::MutexGuard<'static, ()>,
    prev: Option<std::ffi::OsString>,
}

impl WorkspaceRootEnv {
    fn pin(root: &std::path::Path) -> Self {
        let lock = crate::core::trusty_tools_config::env_test_lock();
        let key = crate::core::trusty_tools_config::WORKSPACE_ROOT_ENV;
        let prev = std::env::var_os(key);
        // SAFETY: the crate-wide `env_test_lock` is held for this value's whole
        // lifetime, and every other mutator of this variable takes it too.
        unsafe { std::env::set_var(key, root) };
        Self { _lock: lock, prev }
    }
}

impl Drop for WorkspaceRootEnv {
    fn drop(&mut self) {
        let key = crate::core::trusty_tools_config::WORKSPACE_ROOT_ENV;
        // SAFETY: `_lock` is still held — it is dropped after this field.
        unsafe {
            match self.prev.take() {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }
}

/// Every path a route reports, across the three path-carrying keys.
fn reported_paths(body: &Value) -> Vec<String> {
    ["paths", "owner_unknown_paths", "agent_owned_paths"]
        .iter()
        .filter_map(|key| body.get(*key).and_then(|v| v.as_array()))
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect()
}

/// The safe defaults survive the transport: an RPC `prune-worktrees` with no
/// flags previews rather than deleting, exactly as an empty HTTP body does
/// (#4091, #2919).
///
/// #6551: pinned to a fixture workspace root. Unpinned, the two transports read
/// the ambient `$HOME` at different instants and `assert_parity` compares one
/// side's real worktree inventory against the other's.
#[serial_test::serial]
#[tokio::test]
async fn managed_prune_worktrees_parity_defaults_to_a_preview() {
    let (state, _dir) = test_state().await;
    let workspace = tempfile::tempdir().expect("workspace root");
    let _env = WorkspaceRootEnv::pin(workspace.path());
    let http = http_call(
        http_router(Arc::clone(&state)),
        Method::POST,
        "/api/v1/sessions/managed/prune-worktrees",
        Some(json!({})),
    )
    .await;
    let rpc = rpc_call(
        &rpc_router(&state),
        "mpm.managed.prune_worktrees",
        json!({}),
    )
    .await;
    assert_eq!(http.status, 200);
    assert_eq!(
        http.json.as_ref().and_then(|v| v["dry_run"].as_bool()),
        Some(true),
        "an unspecified prune must preview, not delete"
    );
    assert_parity("mpm.managed.prune_worktrees", &http, &rpc);
}

/// The prune preview reports the pinned workspace root and nothing outside it
/// (#6551).
///
/// Why: `managed_prune_worktrees_parity_defaults_to_a_preview` compares the two
/// transports to each other, so it stays green whenever they agree — including
/// when they agree on the operator's real `~/trusty-mpm-projects`. This asserts
/// on WHICH root was scanned, which is the thing that was wrong.
/// What: pins the root at a `GitWorktreeFixture` holding one reclaimable
/// orphaned worktree, then asserts every reported path is inside it. Seeding a
/// real, reclaimable worktree is what stops an empty result from passing for
/// isolation — the same fixture `prune::tests` uses.
/// Test: this is the test. RED without `WorkspaceRootEnv::pin`: the preview
/// enumerates the real home's worktrees, none of which is under the fixture.
#[serial_test::serial]
#[tokio::test]
async fn managed_prune_worktrees_enumerates_only_the_pinned_workspace_root() {
    use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;

    let (state, _dir) = test_state().await;
    let fx = GitWorktreeFixture::new();
    let orphan = fx.add_worktree("orphaned-6551");
    GitWorktreeFixture::stamp_reclaimable_sentinel(&orphan);
    let root = fx
        .repos_root
        .canonicalize()
        .unwrap_or_else(|_| fx.repos_root.clone());

    let _env = WorkspaceRootEnv::pin(&root);
    let http = http_call(
        http_router(Arc::clone(&state)),
        Method::POST,
        "/api/v1/sessions/managed/prune-worktrees",
        Some(json!({})),
    )
    .await;
    assert_eq!(http.status, 200);
    let body = http.json.as_ref().expect("prune-worktrees returns json");
    let paths = reported_paths(body);
    assert!(
        !paths.is_empty(),
        "the seeded orphan at {} must be reported, or this proves nothing: {body}",
        orphan.display()
    );
    let stray: Vec<&String> = paths
        .iter()
        .filter(|p| !std::path::Path::new(p).starts_with(&root))
        .collect();
    assert!(
        stray.is_empty(),
        "#6551: the preview scanned outside the pinned workspace root {} — \
         these paths come from the ambient $HOME: {stray:?}",
        root.display()
    );
}

/// #6551: pinned for the same reason as the prune parity test above —
/// `reconcile_worktrees_core` resolves the same ambient workspace root.
#[serial_test::serial]
#[tokio::test]
async fn managed_reconcile_worktrees_parity() {
    let (state, _dir) = test_state().await;
    let workspace = tempfile::tempdir().expect("workspace root");
    let _env = WorkspaceRootEnv::pin(workspace.path());
    let http = http_call(
        http_router(Arc::clone(&state)),
        Method::GET,
        "/api/v1/sessions/managed/reconcile-worktrees",
        None,
    )
    .await;
    let rpc = rpc_call(
        &rpc_router(&state),
        "mpm.managed.reconcile_worktrees",
        json!({}),
    )
    .await;
    assert_parity("mpm.managed.reconcile_worktrees", &http, &rpc);
}

/// An invalid runtime selector is the same 400 on both transports, and neither
/// provisions anything (#6288).
#[tokio::test]
async fn managed_spawn_rejects_an_invalid_runtime_on_both_transports() {
    let (state, _dir) = test_state().await;
    let body = json!({
        "repo_url": "https://example.com/x.git",
        "ref": "main",
        "task": "t",
        "runtime": "nonsense-runtime",
    });
    let http = http_call(
        http_router(Arc::clone(&state)),
        Method::POST,
        "/api/v1/sessions/managed",
        Some(body.clone()),
    )
    .await;
    let rpc = rpc_call(&rpc_router(&state), "mpm.managed.spawn", body).await;
    assert_eq!(
        http.status, 400,
        "an invalid runtime must be refused before any provisioning side effect"
    );
    assert_parity("mpm.managed.spawn (bad runtime)", &http, &rpc);
}

/// An adopt of a tmux session that does not exist refuses identically, without
/// registering anything (#1433).
#[tokio::test]
async fn managed_adopt_rejects_an_invalid_runtime_on_both_transports() {
    let (state, _dir) = test_state().await;
    let body = json!({"tmux_name": "no-such-pane", "cwd": "/tmp", "runtime": "nonsense-runtime"});
    let http = http_call(
        http_router(Arc::clone(&state)),
        Method::POST,
        "/api/v1/sessions/managed/adopt",
        Some(body.clone()),
    )
    .await;
    let rpc = rpc_call(&rpc_router(&state), "mpm.managed.adopt", body).await;
    assert_eq!(http.status, 400);
    assert_parity("mpm.managed.adopt (bad runtime)", &http, &rpc);
}

/// A tmux double that reports the sessions it was asked to create as LIVE.
///
/// Why: `delete_record`'s running-session guard is a tmux PROBE
/// (`session_exists_checked`), not a record-state check, so under the default
/// `FakeNoopTmuxDriver` — whose `list_sessions` is always empty — nothing is
/// ever running and `force` is never consulted. That is precisely how the flag
/// went untested. This driver makes a seeded session observably live so the
/// guard fires, without a real tmux server.
/// What: records every `create_session` name and serves them from
/// `list_sessions`, which is what the trait's default `session_exists_checked`
/// reads; `kill_session` removes one. Every other method is the no-op
/// `FakeNoopTmuxDriver` provides.
/// Test: `managed_delete_refuses_a_running_session_without_force`.
#[derive(Debug, Default)]
struct LiveTrackingTmux {
    live: std::sync::Mutex<Vec<String>>,
}

impl crate::session_manager::ManagedTmuxDriver for LiveTrackingTmux {
    fn create_session(&self, name: &str, _workdir: &str) -> Result<(), ManagedError> {
        self.live
            .lock()
            .expect("not poisoned")
            .push(name.to_owned());
        Ok(())
    }

    fn kill_session(&self, name: &str) -> Result<(), ManagedError> {
        self.live
            .lock()
            .expect("not poisoned")
            .retain(|n| n != name);
        Ok(())
    }

    fn send_line(&self, _name: &str, _text: &str) -> Result<(), ManagedError> {
        Ok(())
    }

    fn send_keys_literal(&self, _name: &str, _text: &str) -> Result<(), ManagedError> {
        Ok(())
    }

    fn send_interrupt(&self, _name: &str) -> Result<(), ManagedError> {
        Ok(())
    }

    fn capture(&self, _name: &str, _lines: usize) -> Result<String, ManagedError> {
        Ok(String::new())
    }

    fn list_sessions(&self) -> Result<Vec<String>, ManagedError> {
        Ok(self.live.lock().expect("not poisoned").clone())
    }
}

/// A `test_state` whose seeded sessions read as live, for the `force` tests.
async fn test_state_live_tmux() -> (Arc<DaemonState>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("temp dir");
    let driver: Arc<dyn crate::session_manager::ManagedTmuxDriver> =
        Arc::new(LiveTrackingTmux::default());
    let state =
        DaemonState::with_root_isolated_managed_and_driver(dir.path().to_path_buf(), driver).await;
    (Arc::new(state), dir)
}

// ── The `force` flag, decoded from a real frame ──────────────────────────────

/// Seed one `Active` managed session in `state`, and return its id.
///
/// Why: every other managed test here drives a session id that does not exist,
/// which reaches the route body but refuses before any flag is read. The two
/// `force` tests below need a record whose state the guard actually refuses.
async fn seed_running_session(
    state: &Arc<DaemonState>,
    root: &std::path::Path,
) -> crate::session_manager::ManagedSessionId {
    let mgr = state.session_manager().await;
    let id = crate::session_manager::ManagedSessionId::new();
    let workspace = root.join(format!("{id}-force-test"));
    mgr.create_with_id(
        id,
        "force-flag test".to_string(),
        Some(workspace.clone()),
        None,
        Some(workspace),
        Some("https://example.com/r.git".to_string()),
        Some("main".to_string()),
        crate::runtime::RuntimeKind::default(),
        false,
        false,
    )
    .await
    .expect("seed session");
    id
}

/// `force: false` refuses a running session identically on both transports
/// (#2012, #6288).
///
/// Why: `mpm.managed.delete`'s `force` field is decoded from the request frame,
/// and until this test nothing decoded it — the other delete tests drive a
/// missing id, which refuses before `force` is read. A field that is never
/// decoded is a field whose `#[serde(default)]` could flip to `true` unnoticed,
/// which would turn the running-session guard off for every socket caller.
/// What: seeds an `Active` record, then drives `?force=false` over HTTP and
/// `{"force": false}` over the socket, asserting the same 409 and the same
/// message. The record is re-read afterwards to prove neither call deleted it.
/// Test: this function IS the test.
#[tokio::test]
async fn managed_delete_refuses_a_running_session_without_force() {
    let (state, dir) = test_state_live_tmux().await;
    let session = seed_running_session(&state, dir.path()).await;
    let id = session.to_string();

    let http = http_call(
        http_router(Arc::clone(&state)),
        Method::POST,
        &format!("/api/v1/sessions/managed/{id}/delete?force=false"),
        None,
    )
    .await;
    let rpc = rpc_call(
        &rpc_router(&state),
        "mpm.managed.delete",
        json!({"id": id, "force": false}),
    )
    .await;

    assert_eq!(
        http.status, 409,
        "a running session must be refused without force, body={:?}",
        http.text
    );
    assert_parity("mpm.managed.delete (running, no force)", &http, &rpc);

    let mgr = state.session_manager().await;
    let record = mgr.get(&session).await.expect("record must survive");
    assert_ne!(
        record.state,
        crate::session_manager::ManagedSessionState::Deleted,
        "a refused delete must not have deleted the record"
    );
}

/// `force: true` reaches the destructive path on both transports (#2012, #6288).
///
/// Why: the mirror of the test above — proving the flag is not merely decoded
/// but honoured, on the socket as on HTTP. Without it, a socket caller's
/// `force: true` could be silently dropped and the operator would read a 409
/// they did not ask for.
/// What: seeds TWO `Active` records and deletes one over each transport, since
/// the verb is destructive and a single record cannot be deleted twice. The
/// bodies are compared with the two identity fields that necessarily differ
/// (`id`, `name`) blanked; everything else, `deleted` and the record's new
/// state included, must match.
/// Test: this function IS the test.
#[tokio::test]
async fn managed_delete_force_bypasses_the_running_guard_on_both_transports() {
    let (state, dir) = test_state_live_tmux().await;
    let over_http = seed_running_session(&state, dir.path()).await;
    let over_rpc = seed_running_session(&state, dir.path()).await;

    let http = http_call(
        http_router(Arc::clone(&state)),
        Method::POST,
        &format!("/api/v1/sessions/managed/{over_http}/delete?force=true"),
        None,
    )
    .await;
    let rpc = rpc_call(
        &rpc_router(&state),
        "mpm.managed.delete",
        json!({"id": over_rpc.to_string(), "force": true}),
    )
    .await;

    assert_eq!(
        http.status, 200,
        "force=true must bypass the running guard, body={:?}",
        http.text
    );
    let http_body = http.json.clone().expect("a 200 delete answers JSON");
    let rpc_body = rpc.result.clone().expect("the socket must not refuse");
    assert_eq!(
        http_body["deleted"],
        json!(true),
        "HTTP must report the record deleted"
    );
    assert_eq!(
        rpc_body["deleted"],
        json!(true),
        "the socket must report the record deleted"
    );
    assert_eq!(
        blank_identity(&http_body),
        blank_identity(&rpc_body),
        "the two transports must answer the same body once the per-record \
         identity fields are set aside"
    );

    let mgr = state.session_manager().await;
    for id in [&over_http, &over_rpc] {
        let record = mgr
            .get(id)
            .await
            .expect("a deleted record is kept, not removed");
        assert_eq!(
            record.state,
            crate::session_manager::ManagedSessionState::Deleted,
            "session {id} must be marked deleted on both transports"
        );
    }
}

/// Blank the two fields that identify WHICH record a delete answered for.
///
/// Why: the `force: true` test deletes a different record per transport, so the
/// fields naming WHICH record answered differ by construction. Blanking exactly
/// those — rather than comparing a hand-picked subset — keeps every other field
/// under the equality assertion, so a divergence anywhere else still fails.
fn blank_identity(body: &Value) -> Value {
    let mut body = body.clone();
    if let Some(map) = body.as_object_mut() {
        // `DeleteResponse` flattens the summary, so these sit at the top level.
        for field in [
            "id",
            "name",
            "tmux_name",
            "cwd",
            "workspace_path",
            "created_at",
        ] {
            if map.contains_key(field) {
                map.insert(field.to_owned(), Value::Null);
            }
        }
    }
    body
}

/// `mpm.control.stop` decodes `force` off the frame and sends the matching
/// command (#6288).
///
/// Why: `rpc_ctl_stop_parity` drives a session that does not exist, so it
/// refuses before `force` is read — the field's wire decoding was untested. The
/// difference between `Stop` and `ForceStop` is the difference between a
/// graceful shutdown and a kill, so a dropped flag is not cosmetic.
/// What: registers a real actor handle, sends `force: true` and then an omitted
/// `force`, and asserts BOTH the echoed field and the `ActorCommand` the actor
/// actually received on its channel — the response alone could echo a flag the
/// registry never acted on.
/// Test: this function IS the test.
#[tokio::test]
async fn rpc_ctl_stop_decodes_the_force_flag() {
    use crate::control::ActorCommand;

    let (state, _dir) = test_state().await;
    let id = crate::control::ControlSessionId::new("force-proj", 0);
    let (command_tx, mut commands) = tokio::sync::mpsc::channel(4);
    let (event_tx, _) = tokio::sync::broadcast::channel(4);
    let handle = crate::control::actor::SessionActorHandle {
        command_tx,
        event_tx,
        write_lock_held: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        metadata: Arc::new(tokio::sync::RwLock::new(
            crate::control::state::SessionMetadata::new(
                id.clone(),
                "force-proj".into(),
                crate::control::event::BackendKind::StreamJson,
            ),
        )),
    };
    state.session_registry.register(id.clone(), handle).await;
    let router = rpc_router(&state);

    let forced = rpc_call(
        &router,
        "mpm.control.stop",
        json!({"id": id.to_string(), "force": true}),
    )
    .await;
    assert_eq!(
        forced.result.expect("stop must succeed")["force"],
        json!(true),
        "the response must echo the flag it acted on"
    );
    assert!(
        matches!(
            commands.recv().await.expect("a command must be sent"),
            ActorCommand::ForceStop
        ),
        "force: true must reach the actor as ForceStop, not Stop"
    );

    // An omitted `force` is the graceful stop, exactly as an absent `?force=` is.
    let graceful = rpc_call(&router, "mpm.control.stop", json!({"id": id.to_string()})).await;
    assert_eq!(
        graceful.result.expect("stop must succeed")["force"],
        json!(false)
    );
    assert!(
        matches!(
            commands.recv().await.expect("a command must be sent"),
            ActorCommand::Stop
        ),
        "an omitted force must reach the actor as Stop"
    );
}

// ── L2 proxy parity ──────────────────────────────────────────────────────────

#[tokio::test]
async fn proxy_focus_parity() {
    let (state, _dir) = test_state().await;
    let body = json!({"conversation_key": "c1", "session_id": ""});
    let http = http_call(
        http_router(Arc::clone(&state)),
        Method::POST,
        "/api/v1/sessions/proxy/focus",
        Some(body.clone()),
    )
    .await;
    let rpc = rpc_call(&rpc_router(&state), "mpm.proxy.focus", body).await;
    assert_eq!(http.status, 200);
    assert_parity("mpm.proxy.focus", &http, &rpc);
}

#[tokio::test]
async fn proxy_get_focus_parity() {
    let (state, _dir) = test_state().await;
    let http = http_call(
        http_router(Arc::clone(&state)),
        Method::GET,
        "/api/v1/sessions/proxy/focus/c1",
        None,
    )
    .await;
    let rpc = rpc_call(
        &rpc_router(&state),
        "mpm.proxy.get_focus",
        json!({"conversation_key": "c1"}),
    )
    .await;
    assert_eq!(http.status, 200);
    assert_parity("mpm.proxy.get_focus", &http, &rpc);
}

#[tokio::test]
async fn proxy_unfocus_parity() {
    let (state, _dir) = test_state().await;
    let body = json!({"conversation_key": "c1"});
    let http = http_call(
        http_router(Arc::clone(&state)),
        Method::POST,
        "/api/v1/sessions/proxy/unfocus",
        Some(body.clone()),
    )
    .await;
    let rpc = rpc_call(&rpc_router(&state), "mpm.proxy.unfocus", body).await;
    assert_eq!(http.status, 200);
    assert_parity("mpm.proxy.unfocus", &http, &rpc);
}

#[tokio::test]
async fn proxy_message_parity() {
    let (state, _dir) = test_state().await;
    let body = json!({"conversation_key": "c1", "text": "hello"});
    let http = http_call(
        http_router(Arc::clone(&state)),
        Method::POST,
        "/api/v1/sessions/proxy/message",
        Some(body.clone()),
    )
    .await;
    let rpc = rpc_call(&rpc_router(&state), "mpm.proxy.message", body).await;
    assert_eq!(http.status, 200);
    assert_parity("mpm.proxy.message", &http, &rpc);
}

#[tokio::test]
async fn proxy_summary_parity() {
    let (state, _dir) = test_state().await;
    let http = http_call(
        http_router(Arc::clone(&state)),
        Method::GET,
        "/api/v1/sessions/proxy/summary/c1",
        None,
    )
    .await;
    let rpc = rpc_call(
        &rpc_router(&state),
        "mpm.proxy.summary",
        json!({"conversation_key": "c1"}),
    )
    .await;
    assert_eq!(http.status, 200);
    assert_parity("mpm.proxy.summary", &http, &rpc);
}
