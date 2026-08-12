//! HTTP-boundary tests for the absent-vs-undeterminable split on palace open
//! (#5549, ADR-0045).
//!
//! Why: the two `open_handle` helpers — `web::error::open_handle` and
//! `MemoryService::open_handle` — each mapped EVERY `PalaceRegistry::open_palace`
//! failure to "palace not found". A palace whose `palace.json` could not be read
//! was therefore reported to the client as one that does not exist. These tests
//! assert the split where a client actually observes it, the status code, rather
//! than at the error type one or two layers in.
//! What: one test per helper for the undeterminable direction, plus one that
//! pins the benign direction so the fix cannot be "everything is 500 now".
//! Test: run with `cargo test -p trusty-memory palace_open_error`.

use super::super::router;
use super::test_state;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::json;
use tower::util::ServiceExt;

/// Restore a path's mode on drop, including while unwinding from a failed
/// assertion, so a locked `palace.json` never outlives the test that locked it.
#[cfg(unix)]
struct RestoreMode(std::path::PathBuf);

#[cfg(unix)]
impl Drop for RestoreMode {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o600));
    }
}

/// Create `id` through the HTTP surface, then make its `palace.json` unreadable.
///
/// Why: the undeterminable case has to be a palace that genuinely exists, or the
/// test proves nothing about telling existence from absence. Creating it through
/// `POST /api/v1/palaces` also leaves a cached handle in the registry, which
/// would satisfy every later open without touching disk — so the handle is
/// evicted here too.
/// What: POSTs the palace, evicts the cached handle, strips `palace.json` to
/// mode 000, asserts the denial took hold, and returns the restore guard.
/// Test: the three tests below.
#[cfg(unix)]
async fn create_then_lock_metadata(
    state: &crate::AppState,
    app: &axum::Router,
    id: &str,
) -> RestoreMode {
    use std::os::unix::fs::PermissionsExt;

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/palaces")
                .header("content-type", "application/json")
                .body(Body::from(json!({ "name": id }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "palace create must succeed");

    state
        .registry
        .remove(&trusty_common::memory_core::PalaceId::new(id));

    let target = state.data_root.join(id).join("palace.json");
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o000)).unwrap();
    let restore = RestoreMode(target.clone());

    // Root bypasses the mode bits outright and some filesystems ignore them, so
    // confirm the denial actually took hold. A vacuous pass on a fail-open guard
    // is worse than no test at all.
    match std::fs::read(&target) {
        Ok(_) => panic!(
            "cannot exercise #5549: {} is still readable at mode 000. Run this suite as a \
             non-root user on a filesystem that honours POSIX permission bits.",
            target.display()
        ),
        Err(e) => assert_eq!(
            e.kind(),
            std::io::ErrorKind::PermissionDenied,
            "expected the locked palace.json to deny reads, got {e}"
        ),
    }
    restore
}

/// Why (#5549, ADR-0045): `MemoryService::open_handle` mapped every open failure
/// to `ServiceError::NotFound`, which the HTTP layer renders as 404. That is the
/// coercion PR #5574 fixed at the two `update_palace_name` sites, one layer
/// further out and reaching far more endpoints — every `/kg*` route, the drawer
/// CRUD routes, and per-palace recall all funnel through this helper.
/// What: locks a real palace's metadata and asserts `GET
/// /api/v1/palaces/{id}/drawers` answers 500, and specifically NOT 404. The
/// direction is the point, not merely that the request failed.
/// Test: this test itself.
#[cfg(unix)]
#[tokio::test]
async fn unreadable_palace_is_500_not_404_at_the_service_open_handle() {
    let state = test_state();
    let app = router().with_state(state.clone());
    let _restore = create_then_lock_metadata(&state, &app, "unreadable-svc").await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/palaces/unreadable-svc/drawers")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_ne!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "a palace whose metadata cannot be read was reported as absent — that is the #5549 \
         coercion re-created at MemoryService::open_handle, one layer out from PR #5574"
    );
    assert_eq!(
        resp.status(),
        StatusCode::INTERNAL_SERVER_ERROR,
        "an undeterminable palace open must surface as a server-side failure"
    );
}

/// Why (#5549, ADR-0045): `web::error::open_handle` is the second site with the
/// same blanket 404, serving `/api/v1/kg/gaps`, `/api/v1/kg/aliases`, and the
/// three `/api/v1/messages` endpoints. It is a distinct function from the
/// service-layer helper above and needs its own boundary assertion.
/// What: locks a real palace's metadata and asserts `GET /api/v1/kg/gaps` for
/// that palace answers 500, and specifically NOT 404.
/// Test: this test itself.
#[cfg(unix)]
#[tokio::test]
async fn unreadable_palace_is_500_not_404_at_the_web_open_handle() {
    let state = test_state();
    let app = router().with_state(state.clone());
    let _restore = create_then_lock_metadata(&state, &app, "unreadable-web").await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/kg/gaps?palace=unreadable-web")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_ne!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "a palace whose metadata cannot be read was reported as absent — that is the #5549 \
         coercion re-created at web::error::open_handle"
    );
    assert_eq!(
        resp.status(),
        StatusCode::INTERNAL_SERVER_ERROR,
        "an undeterminable palace open must surface as a server-side failure"
    );
}

/// Why (#5549, ADR-0045 decision 3): the fix must not turn a genuine absence
/// into a 500. `NotFound` keeps its benign meaning, and a client asking for an
/// id that was never created still has to be told so — otherwise every typo in
/// a palace name becomes an opaque server error.
/// What: asks both helpers' endpoints for an id no palace was ever created
/// under, and asserts 404 from each. Uses no permission bits, so it runs on any
/// platform and under any uid.
/// Test: this test itself.
#[tokio::test]
async fn absent_palace_is_still_404_at_both_open_handles() {
    let state = test_state();
    let app = router().with_state(state);

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/palaces/never-created/drawers")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "an absent palace must still be 404 at MemoryService::open_handle"
    );

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/kg/gaps?palace=never-created")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "an absent palace must still be 404 at web::error::open_handle"
    );
}
