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
