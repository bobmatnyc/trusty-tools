//! `tm ls` stop-then-delete: the errored-session route and its fail-open check (#7224).
//!
//! Why: split out of `tests_behavior_d_tests.rs`, which sits within a few
//! dozen lines of the 3000-SLOC test cap — the same reason
//! `tests_behavior_d_ls_connector_tests.rs` exists. The block is cohesive on
//! its own: everything here is about ONE route, `picker_delete::stop_then_delete`.
//!
//! What: the two pure seams the route is built from
//! ([`delete_needs_stop_first`](crate::commands::picker_delete::delete_needs_stop_first)
//! and [`classify_stop_first`](crate::commands::picker_delete::classify_stop_first)),
//! plus real-HTTP round trips against a counting stub daemon. The counting is
//! the point: proving a REJECTED stop issues no delete needs a server that can
//! say the delete endpoint was never called, which a message assertion cannot.

use std::net::SocketAddr;

use crate::commands::picker_delete::DeleteReport;

/// #7224: the stop-first leg is scoped to `errored` and nothing else. A
/// `stopped` record has no runtime to stop, and `active`/`provisioning` take the
/// force-confirm path — neither may pick up a stop it does not need.
#[test]
fn delete_needs_stop_first_only_for_errored() {
    use crate::commands::picker_delete::delete_needs_stop_first;
    assert!(delete_needs_stop_first("errored"));
    for state in [
        "stopped",
        "active",
        "provisioning",
        "decommissioned",
        "deleted",
    ] {
        assert!(
            !delete_needs_stop_first(state),
            "{state} must not take the stop-first route"
        );
    }
}

/// #7224 fail-open check: only an explicit 2xx or 404 lets the delete proceed.
/// Every other status — a 409, a 500, a 503 — is `Failed`, which the caller
/// turns into "nothing was deleted". A carve-out keyed on "an error happened"
/// rather than on the daemon's own answer is exactly how a stop-then-delete
/// sequence deletes a session the daemon still considers live.
#[test]
fn classify_stop_first_maps_each_status() {
    use crate::commands::picker_delete::{StopFirstNext, classify_stop_first};
    assert_eq!(
        classify_stop_first(reqwest::StatusCode::OK),
        StopFirstNext::Stopped
    );
    assert_eq!(
        classify_stop_first(reqwest::StatusCode::ACCEPTED),
        StopFirstNext::Stopped,
        "any 2xx means the daemon accepted the stop"
    );
    assert_eq!(
        classify_stop_first(reqwest::StatusCode::NOT_FOUND),
        StopFirstNext::NothingToStop,
        "404 is the daemon's explicit 'no live session here'"
    );
    for status in [
        reqwest::StatusCode::CONFLICT,
        reqwest::StatusCode::INTERNAL_SERVER_ERROR,
        reqwest::StatusCode::SERVICE_UNAVAILABLE,
        reqwest::StatusCode::BAD_REQUEST,
    ] {
        assert_eq!(
            classify_stop_first(status),
            StopFirstNext::Failed,
            "{status} must NOT be downgraded into a delete"
        );
    }
}

/// A hermetic stand-in for the two managed endpoints the stop-first route
/// touches, so the STOP status can be dialled to anything and the delete leg
/// can be OBSERVED — the fail-open check needs to prove no delete request was
/// issued, which only a counting server can show.
#[derive(Clone)]
struct StopStub {
    stop_status: axum::http::StatusCode,
    stops: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    deletes: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl StopStub {
    fn new(stop_status: axum::http::StatusCode) -> Self {
        Self {
            stop_status,
            stops: Default::default(),
            deletes: Default::default(),
        }
    }

    fn router(&self) -> axum::Router {
        use axum::{Router, extract::State, routing::post};
        let stub = self.clone();
        Router::new()
            .route(
                "/api/v1/sessions/managed/{id}/runtime-stop",
                post(|State(s): State<StopStub>| async move {
                    s.stops.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    (s.stop_status, "stub stop body")
                }),
            )
            .route(
                "/api/v1/sessions/managed/{id}/delete",
                post(|State(s): State<StopStub>| async move {
                    s.deletes.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    axum::Json(serde_json::json!({
                        "name": "tm-stub-01",
                        "state": "errored",
                        "deleted": true,
                    }))
                }),
            )
            .with_state(stub)
    }

    fn counts(&self) -> (usize, usize) {
        use std::sync::atomic::Ordering::SeqCst;
        (self.stops.load(SeqCst), self.deletes.load(SeqCst))
    }
}

/// Serve `router` on a fresh loopback port; returns the base URL and the task.
async fn serve_stub(router: axum::Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback port");
    let addr: SocketAddr = listener.local_addr().expect("resolve bound addr");
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    (format!("http://{addr}"), handle)
}

/// The whole point of the route: an errored session whose runtime is up gets
/// stopped and then deleted, in one operator action, with no bounce to the CLI.
#[tokio::test]
async fn stop_then_delete_deletes_after_a_successful_stop() {
    use crate::commands::picker_delete::stop_then_delete;
    let stub = StopStub::new(axum::http::StatusCode::OK);
    let (url, handle) = serve_stub(stub.router()).await;

    let report = stop_then_delete(&reqwest::Client::new(), &url, "sid-1", false)
        .await
        .expect("routing call must not error");
    assert!(
        matches!(report, DeleteReport::Deleted { local: false, .. }),
        "expected a managed delete, got {report:?}"
    );
    assert_eq!(stub.counts(), (1, 1), "one stop, then one delete");
    handle.abort();
}

/// "Nothing to stop" is the daemon's EXPLICIT 404, not a guess: the delete still
/// goes out, and its own not-found routing decides what that means.
#[tokio::test]
async fn stop_then_delete_treats_not_found_as_nothing_to_stop() {
    use crate::commands::picker_delete::stop_then_delete;
    let stub = StopStub::new(axum::http::StatusCode::NOT_FOUND);
    let (url, handle) = serve_stub(stub.router()).await;

    let report = stop_then_delete(&reqwest::Client::new(), &url, "sid-2", false)
        .await
        .expect("routing call must not error");
    assert!(
        matches!(report, DeleteReport::Deleted { .. }),
        "a 404 stop must not block the delete, got {report:?}"
    );
    assert_eq!(stub.counts(), (1, 1));
    handle.abort();
}

/// Fail-open check. A stop the daemon REJECTED (500, and separately 409) must
/// leave the record alone: the report says nothing was deleted, and — the part
/// a message assertion alone would not prove — the delete endpoint is never
/// called at all.
#[tokio::test]
async fn stop_then_delete_never_deletes_after_a_failed_stop() {
    use crate::commands::picker_delete::stop_then_delete;
    for status in [
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        axum::http::StatusCode::CONFLICT,
    ] {
        let stub = StopStub::new(status);
        let (url, handle) = serve_stub(stub.router()).await;

        let report = stop_then_delete(&reqwest::Client::new(), &url, "sid-3", false)
            .await
            .expect("routing call must not error");
        match report {
            DeleteReport::StopFailed(msg) => {
                assert!(
                    msg.contains(status.as_str()),
                    "must carry the status: {msg}"
                );
                assert!(msg.contains("stub stop body"), "must carry the body: {msg}");
            }
            other => panic!("a {status} stop must report StopFailed, got {other:?}"),
        }
        assert_eq!(
            stub.counts(),
            (1, 0),
            "a rejected stop must issue NO delete request ({status})"
        );
        handle.abort();
    }
}

// ── the numbered picker's own delete route (#7224) ──────────────────────────

/// The daemon's answer for an ERRORED session whose tmux pane is still up: the
/// delete guard 409s until the runtime has actually been stopped. This is the
/// shape of the reported bug — a delete that never stops first bounces off that
/// 409 with advice to leave the surface and run `tm session stop` by hand.
#[derive(Clone)]
struct LiveErroredStub {
    stop_status: axum::http::StatusCode,
    runtime_down: std::sync::Arc<std::sync::atomic::AtomicBool>,
    stops: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    deletes: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl LiveErroredStub {
    fn new(stop_status: axum::http::StatusCode) -> Self {
        Self {
            stop_status,
            runtime_down: Default::default(),
            stops: Default::default(),
            deletes: Default::default(),
        }
    }

    fn router(&self) -> axum::Router {
        use std::sync::atomic::Ordering::SeqCst;

        use axum::{Router, extract::State, response::IntoResponse, routing::post};
        let stub = self.clone();
        Router::new()
            .route(
                "/api/v1/sessions/managed/{id}/runtime-stop",
                post(|State(s): State<LiveErroredStub>| async move {
                    s.stops.fetch_add(1, SeqCst);
                    // 2xx and 404 are both "no live runtime remains"; anything
                    // else leaves the session exactly as it was.
                    if s.stop_status.is_success()
                        || s.stop_status == axum::http::StatusCode::NOT_FOUND
                    {
                        s.runtime_down.store(true, SeqCst);
                    }
                    (s.stop_status, "stub stop body")
                }),
            )
            .route(
                "/api/v1/sessions/managed/{id}/delete",
                post(|State(s): State<LiveErroredStub>| async move {
                    s.deletes.fetch_add(1, SeqCst);
                    if !s.runtime_down.load(SeqCst) {
                        return (
                            axum::http::StatusCode::CONFLICT,
                            "session is errored — stop it first with `tm session stop`",
                        )
                            .into_response();
                    }
                    axum::Json(serde_json::json!({
                        "name": "tm-quiet-falcon",
                        "state": "errored",
                        "deleted": true,
                    }))
                    .into_response()
                }),
            )
            .with_state(stub)
    }

    fn counts(&self) -> (usize, usize) {
        use std::sync::atomic::Ordering::SeqCst;
        (self.stops.load(SeqCst), self.deletes.load(SeqCst))
    }
}

/// A managed row in `state`, with the id the stub answers for.
fn row(state: &str) -> trusty_mpm::client::ManagedSessionSummary {
    trusty_mpm::client::ManagedSessionSummary {
        id: "sid-picker".to_string(),
        name: "tm-quiet-falcon".to_string(),
        state: state.to_string(),
        persisted_state: None,
        workspace_path: None,
        repo_url: None,
        branch: None,
        created_at: None,
        last_activity_at: None,
        pending_decision: None,
        proposed_default: None,
        source_id: None,
        task: None,
        cwd: None,
        claude_session_id: None,
        deliverable_id: None,
        pane_id: None,
        injection_status: None,
        unresumable: false,
        stale_assets: false,
        stale_assets_unchecked: false,
        attached: false,
        slot: 1,
        deleted: false,
        auto_resume_parked: None,
    }
}

/// The reported bug, on the surface it was reported from. `tm ls` falls back to
/// the NUMBERED picker whenever the terminal has no raw mode, and that picker's
/// delete driver used to issue the delete alone — so an errored row whose tmux
/// pane was still up got the daemon's 409 and the operator got told to go run a
/// different command in a different surface.
#[tokio::test]
async fn picker_delete_stops_an_errored_session_before_deleting_it() {
    use crate::commands::picker_delete::delete_confirmed;
    let stub = LiveErroredStub::new(axum::http::StatusCode::OK);
    let (url, handle) = serve_stub(stub.router()).await;

    let deleted = delete_confirmed(&reqwest::Client::new(), &url, &row("errored"))
        .await
        .expect("the delete driver must not error");
    assert!(deleted, "the errored row must end up deleted, not refused");
    assert_eq!(stub.counts(), (1, 1), "one stop, then one delete");
    handle.abort();
}

/// A 404 stop is the daemon's explicit "no live session here", not a failure —
/// the delete still goes out and does its own not-found routing.
#[tokio::test]
async fn picker_delete_treats_a_not_found_stop_as_nothing_to_stop() {
    use crate::commands::picker_delete::delete_confirmed;
    let stub = LiveErroredStub::new(axum::http::StatusCode::NOT_FOUND);
    let (url, handle) = serve_stub(stub.router()).await;

    let deleted = delete_confirmed(&reqwest::Client::new(), &url, &row("errored"))
        .await
        .expect("the delete driver must not error");
    assert!(deleted, "a 404 stop must not block the delete");
    assert_eq!(stub.counts(), (1, 1));
    handle.abort();
}

/// Fail-open check on the picker's own route: a stop the daemon REJECTED leaves
/// the record alone, and the delete endpoint is never reached at all.
#[tokio::test]
async fn picker_delete_never_deletes_after_a_failed_stop() {
    use crate::commands::picker_delete::delete_confirmed;
    for status in [
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        axum::http::StatusCode::CONFLICT,
    ] {
        let stub = LiveErroredStub::new(status);
        let (url, handle) = serve_stub(stub.router()).await;

        let deleted = delete_confirmed(&reqwest::Client::new(), &url, &row("errored"))
            .await
            .expect("the delete driver must not error");
        assert!(!deleted, "a {status} stop must not report a deletion");
        assert_eq!(
            stub.counts(),
            (1, 0),
            "a rejected stop must issue NO delete request ({status})"
        );
        handle.abort();
    }
}

/// The carve-out stays scoped: a `stopped` row is deleted exactly as before,
/// with no runtime-stop request issued on its behalf.
#[tokio::test]
async fn picker_delete_issues_no_stop_for_a_non_errored_row() {
    use crate::commands::picker_delete::delete_confirmed;
    let stub = StopStub::new(axum::http::StatusCode::OK);
    let (url, handle) = serve_stub(stub.router()).await;

    let deleted = delete_confirmed(&reqwest::Client::new(), &url, &row("stopped"))
        .await
        .expect("the delete driver must not error");
    assert!(deleted);
    assert_eq!(
        stub.counts(),
        (0, 1),
        "a stopped row has no runtime to stop — one delete, no stop"
    );
    handle.abort();
}

/// The pair is the whole reason the two surfaces cannot diverge again, so it
/// must stay exactly the two guards it composes — never a third opinion.
#[test]
fn delete_route_flags_match_the_two_guards() {
    use crate::commands::picker_delete::{
        delete_needs_force, delete_needs_stop_first, delete_route_flags,
    };
    for state in [
        "errored",
        "stopped",
        "active",
        "provisioning",
        "decommissioned",
        "deleted",
    ] {
        assert_eq!(
            delete_route_flags(state),
            (delete_needs_force(state), delete_needs_stop_first(state)),
            "{state}"
        );
    }
}

/// Confirming an errored row runs two legs, and both surfaces must promise the
/// same two before the operator agrees — one sentence, not two spellings.
#[test]
fn both_delete_surfaces_ask_the_same_errored_question() {
    use crate::commands::picker_delete::errored_confirm_ask;
    assert_eq!(
        errored_confirm_ask("tm-quiet-falcon"),
        "'tm-quiet-falcon' is errored. Stop it and delete it? Type y, then Enter."
    );
}

// ── the `tm session delete <id>` verb's own route (#7388) ───────────────────

/// The three managed endpoints the VERB touches, with the state it reports
/// dialled per test and the stop/delete legs recorded IN ORDER.
///
/// The order is the point: proving the verb stops an errored session *first*
/// needs a server that can say which request arrived when, and a delete that
/// refuses while the runtime is still up — exactly what the daemon does.
#[derive(Clone)]
struct VerbStub {
    /// The `state` the managed GET reports, or `None` to answer 404 there —
    /// the id then names no managed record (a project-only session).
    state: Option<String>,
    stop_status: axum::http::StatusCode,
    /// While true the delete endpoint 409s, mirroring the daemon's tmux probe.
    live: std::sync::Arc<std::sync::atomic::AtomicBool>,
    legs: std::sync::Arc<std::sync::Mutex<Vec<&'static str>>>,
}

impl VerbStub {
    fn new(state: Option<&str>, stop_status: axum::http::StatusCode, live: bool) -> Self {
        Self {
            state: state.map(str::to_string),
            stop_status,
            live: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(live)),
            legs: Default::default(),
        }
    }

    fn router(&self) -> axum::Router {
        use std::sync::atomic::Ordering::SeqCst;

        use axum::{
            Router,
            extract::State,
            response::IntoResponse,
            routing::{get, post},
        };
        let stub = self.clone();
        Router::new()
            .route(
                "/api/v1/sessions/managed/{id}",
                get(|State(s): State<VerbStub>| async move {
                    match s.state {
                        Some(state) => axum::Json(serde_json::json!({
                            "id": "sid-verb",
                            "name": "tm-quiet-falcon",
                            "state": state,
                        }))
                        .into_response(),
                        None => (axum::http::StatusCode::NOT_FOUND, "no such managed session")
                            .into_response(),
                    }
                }),
            )
            .route(
                "/api/v1/sessions/managed/{id}/runtime-stop",
                post(|State(s): State<VerbStub>| async move {
                    s.legs.lock().expect("legs lock").push("stop");
                    if s.stop_status.is_success()
                        || s.stop_status == axum::http::StatusCode::NOT_FOUND
                    {
                        s.live.store(false, SeqCst);
                    }
                    (s.stop_status, "stub stop body")
                }),
            )
            .route(
                "/api/v1/sessions/managed/{id}/delete",
                post(|State(s): State<VerbStub>| async move {
                    s.legs.lock().expect("legs lock").push("delete");
                    if s.live.load(SeqCst) {
                        return (
                            axum::http::StatusCode::CONFLICT,
                            "session 'tm-quiet-falcon' is errored — stop it first with \
                             `tm session stop`",
                        )
                            .into_response();
                    }
                    axum::Json(serde_json::json!({
                        "name": "tm-quiet-falcon",
                        "state": "errored",
                        "deleted": true,
                    }))
                    .into_response()
                }),
            )
            .with_state(stub)
    }

    fn legs(&self) -> Vec<&'static str> {
        self.legs.lock().expect("legs lock").clone()
    }
}

/// #7388, the reported bug: `tm session delete <errored-id>` exited 1 telling
/// the operator to go run `tm session stop` — the command they were already
/// asking for. It must stop the runtime and THEN delete the record, in that
/// order. A fix that deletes without stopping records `["delete"]` and gets the
/// daemon's 409 back; a fix that special-cases `errored` by skipping the stop
/// records the same. Only the real two-leg route records `["stop", "delete"]`.
#[tokio::test]
async fn verb_delete_stops_an_errored_session_before_deleting_it() {
    let stub = VerbStub::new(Some("errored"), axum::http::StatusCode::OK, true);
    let (url, handle) = serve_stub(stub.router()).await;

    crate::commands::delete::session_delete(
        &reqwest::Client::new(),
        &url,
        "sid-verb".to_string(),
        false,
    )
    .await
    .expect("an errored session must delete, not refuse");
    assert_eq!(
        stub.legs(),
        vec!["stop", "delete"],
        "the stop must arrive before the delete"
    );
    handle.abort();
}

/// Fail-open check on the verb's route: a stop the daemon REJECTED leaves the
/// record alone, the delete endpoint is never reached, and the verb exits
/// non-zero so a script sees the failure.
#[tokio::test]
async fn verb_delete_never_deletes_after_a_failed_stop() {
    for status in [
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        axum::http::StatusCode::CONFLICT,
    ] {
        let stub = VerbStub::new(Some("errored"), status, true);
        let (url, handle) = serve_stub(stub.router()).await;

        let err = crate::commands::delete::session_delete(
            &reqwest::Client::new(),
            &url,
            "sid-verb".to_string(),
            false,
        )
        .await
        .expect_err("a rejected stop must not report success");
        assert!(
            err.to_string().contains("stop failed"),
            "must name the failed stop: {err}"
        );
        assert_eq!(
            stub.legs(),
            vec!["stop"],
            "a rejected stop must issue NO delete request ({status})"
        );
        handle.abort();
    }
}

/// The carve-out stays scoped on the verb too: an id the managed store does not
/// know (a project-only session) gets no runtime-stop issued on its behalf, so
/// the local-fallback route is unchanged.
#[tokio::test]
async fn verb_delete_issues_no_stop_when_the_id_is_not_a_managed_record() {
    let stub = VerbStub::new(None, axum::http::StatusCode::OK, false);
    let (url, handle) = serve_stub(stub.router()).await;

    // The managed GET 404s, so the state is unknown and the plain delete goes
    // out; the stub's delete endpoint answers it (the local fallback beyond it
    // is covered by `local_delete_*` in `tests_behavior_d_tests.rs`).
    crate::commands::delete::session_delete(
        &reqwest::Client::new(),
        &url,
        "sid-verb".to_string(),
        false,
    )
    .await
    .expect("an unknown-state delete must route exactly as before");
    assert_eq!(
        stub.legs(),
        vec!["delete"],
        "an unclassifiable id must not pick up a stop leg"
    );
    handle.abort();
}

/// #7388: the two surfaces must decide the same route for the same state. The
/// picker reads the state off the row it listed and the verb asks the daemon
/// for it, but from there both go through `route_delete_for_state`, so the
/// stop/delete legs they issue must match state for state.
#[tokio::test]
async fn picker_and_verb_route_each_state_identically() {
    use crate::commands::picker_delete::delete_confirmed;
    // Every variant of `ManagedSessionState`, in its serde (snake_case) spelling.
    for state in [
        "provisioning",
        "active",
        "stopped",
        "errored",
        "decommissioned",
        "deleted",
    ] {
        // `live = false`: the delete succeeds either way, so what differs
        // between the surfaces is the ROUTE, not the daemon's verdict.
        let picker_stub = VerbStub::new(Some(state), axum::http::StatusCode::OK, false);
        let (picker_url, picker_handle) = serve_stub(picker_stub.router()).await;
        delete_confirmed(&reqwest::Client::new(), &picker_url, &row(state))
            .await
            .expect("the picker's delete driver must not error");
        picker_handle.abort();

        let verb_stub = VerbStub::new(Some(state), axum::http::StatusCode::OK, false);
        let (verb_url, verb_handle) = serve_stub(verb_stub.router()).await;
        // The picker force-confirms a running row by having the operator type
        // `force`; `--force` is the verb's spelling of the same consent, so
        // pass the flag the state calls for and compare like with like.
        let force = crate::commands::picker_delete::delete_needs_force(state);
        crate::commands::delete::session_delete(
            &reqwest::Client::new(),
            &verb_url,
            "sid-verb".to_string(),
            force,
        )
        .await
        .expect("the verb must not error");
        verb_handle.abort();

        assert_eq!(
            picker_stub.legs(),
            verb_stub.legs(),
            "the picker and `tm session delete` must route a {state} session the same way"
        );
    }
}
