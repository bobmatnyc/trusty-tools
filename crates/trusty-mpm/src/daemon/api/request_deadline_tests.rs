//! Tests for the router-wide request deadline (#8476).

use super::*;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::Router;
use axum::body::Body;
use axum::http::{Method, header::ALLOW};
use axum::routing::get;
use tower::ServiceExt;

/// A router whose `/slow` handler never finishes on its own, under the same
/// middleware `api::router` layers.
fn deadline_router(dropped: Arc<AtomicBool>) -> Router {
    /// Sets its flag when the handler future holding it is dropped.
    struct DropFlag(Arc<AtomicBool>);
    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    Router::new()
        .route(
            "/slow",
            get(move || {
                let flag = DropFlag(Arc::clone(&dropped));
                async move {
                    tokio::time::sleep(Duration::from_secs(3600)).await;
                    drop(flag);
                    "finished"
                }
            }),
        )
        .route("/fast", get(|| async { "ok" }))
        .layer(axum::middleware::from_fn(enforce))
}

async fn send(app: Router, method: Method, uri: &str) -> Response {
    let req = axum::http::Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .expect("request");
    app.oneshot(req).await.expect("infallible router")
}

async fn body_json(resp: Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body");
    serde_json::from_slice(&bytes).expect("JSON body")
}

#[tokio::test(start_paused = true)]
async fn deadline_timeout_maps_to_504_with_json_error() {
    let started = tokio::time::Instant::now();
    let resp = send(deadline_router(Arc::default()), Method::GET, "/slow").await;
    let waited = started.elapsed();

    assert_eq!(resp.status(), StatusCode::GATEWAY_TIMEOUT);
    assert!(
        waited >= STANDARD_DEADLINE && waited < STANDARD_DEADLINE + Duration::from_secs(1),
        "the 504 must arrive at the standard deadline, not after; waited {waited:?}"
    );
    let body = body_json(resp).await;
    assert_eq!(body["deadline_secs"], STANDARD_DEADLINE.as_secs());
    assert_eq!(body["route"], "/slow");
    let error = body["error"].as_str().expect("error string");
    assert!(
        error.contains("GET /slow") && error.contains("25 s server-side deadline"),
        "error must name the route and the deadline: {error}"
    );
}

#[tokio::test(start_paused = true)]
async fn timed_out_handler_future_is_dropped() {
    let dropped = Arc::new(AtomicBool::new(false));
    let resp = send(deadline_router(Arc::clone(&dropped)), Method::GET, "/slow").await;

    assert_eq!(resp.status(), StatusCode::GATEWAY_TIMEOUT);
    assert!(
        dropped.load(Ordering::SeqCst),
        "the handler future must be dropped once its deadline fires, not left running"
    );
}

#[tokio::test(start_paused = true)]
async fn fast_route_passes_through_untouched() {
    let resp = send(deadline_router(Arc::default()), Method::GET, "/fast").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body");
    assert_eq!(&bytes[..], b"ok");
}

#[test]
fn long_routes_resolve_to_their_class_deadline() {
    let cases = [
        (
            "POST",
            Some("/api/v1/sessions/managed/prune-worktrees"),
            SURVEY_DEADLINE,
        ),
        ("POST", Some("/api/v1/sessions/managed"), PROVISION_DEADLINE),
        ("POST", Some("/rpc"), PROVISION_DEADLINE),
        ("POST", Some(COORDINATOR_CHAT_PATH), LLM_DEADLINE),
        ("GET", Some("/api/v1/doctor"), DOCTOR_DEADLINE),
        (
            "DELETE",
            Some("/api/v1/sessions/managed/{id}"),
            LIFECYCLE_DEADLINE,
        ),
        // Same template, cheap method: the method is part of the key.
        (
            "GET",
            Some("/api/v1/sessions/managed/{id}"),
            STANDARD_DEADLINE,
        ),
        ("GET", Some("/api/v1/sessions/managed"), STANDARD_DEADLINE),
        // SSE heads are produced at once; the body is never bounded.
        ("GET", Some("/events"), STANDARD_DEADLINE),
        ("GET", Some("/health"), STANDARD_DEADLINE),
        // The unmatched-path fallback carries no template.
        ("GET", None, STANDARD_DEADLINE),
    ];
    for (method, template, want) in cases {
        assert_eq!(
            deadline_for(method, template),
            want,
            "{method} {template:?}"
        );
    }
}

/// Each class answers before the client bound its callers already use, and
/// never below the work bound it exists to cover.
#[test]
fn long_deadlines_answer_before_their_clients_hang_up() {
    use crate::client::http_client::{
        DISK_SURVEY_REQUEST_TIMEOUT, PROVISION_REQUEST_TIMEOUT, RECLAIM_SURVEY_REQUEST_TIMEOUT,
    };
    let secs = Duration::from_secs;

    // Standard: over the CLI's 10 s default, under trusty-console's 30 s proxy.
    assert!(STANDARD_DEADLINE > secs(10) && STANDARD_DEADLINE < secs(30));
    assert!(
        LIFECYCLE_DEADLINE > crate::session_manager::git_ceiling::GIT_CALL_TIMEOUT,
        "one git call must fit inside the lifecycle deadline"
    );
    // CHAT_REQUEST_TIMEOUT (130 s) and DOCTOR_REQUEST_TIMEOUT (120 s) are
    // `pub(super)` in the client, so their values are restated here.
    assert!(LLM_DEADLINE > secs(120) && LLM_DEADLINE < secs(130));
    assert!(DOCTOR_DEADLINE < secs(120));
    assert!(PROVISION_DEADLINE < PROVISION_REQUEST_TIMEOUT);
    assert!(
        PROVISION_DEADLINE >= DISK_SURVEY_REQUEST_TIMEOUT,
        "`/rpc` carries the disk survey; its caller must not be cut short"
    );
    assert!(SURVEY_DEADLINE < RECLAIM_SURVEY_REQUEST_TIMEOUT);
    assert!(SURVEY_DEADLINE > PROVISION_DEADLINE);
}

/// A misspelled template would silently fall back to the standard deadline,
/// so every row must name a route and method the real router serves.
///
/// What: sends `PUT` (served by no daemon route) to each row's path with its
/// parameters filled in. A registered path answers `405` with an `Allow`
/// header that must list the row's method; an unknown path answers `404`.
#[tokio::test]
async fn every_long_route_is_a_registered_route() {
    let dir = tempfile::tempdir().expect("temp dir");
    let paths = crate::core::paths::FrameworkPaths::under(dir.path());
    let state = Arc::new(crate::daemon::state::DaemonState::with_paths(&paths));
    let app = super::super::router(state);

    for (method, template, _) in LONG_ROUTES {
        let uri: String = template
            .split('/')
            .map(|seg| if seg.starts_with('{') { "x" } else { seg })
            .collect::<Vec<_>>()
            .join("/");
        let resp = send(app.clone(), Method::PUT, &uri).await;
        assert_eq!(
            resp.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "{template} is not a registered route"
        );
        let allow = resp
            .headers()
            .get(ALLOW)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        assert!(
            allow.split(',').any(|m| m.trim() == *method),
            "{template} does not serve {method}; Allow: {allow}"
        );
    }
}
