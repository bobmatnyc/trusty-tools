//! HTTP-boundary tests for the absent-vs-undeterminable split on palace open
//! (#5549, ADR-0045).
//!
//! Why: the two `open_handle` helpers — `web::error::open_handle` and
//! `MemoryService::open_handle` — each mapped EVERY `PalaceRegistry::open_palace`
//! failure to "palace not found". A palace whose `palace.json` could not be read
//! was therefore reported to the client as one that does not exist. These tests
//! assert the split where a client actually observes it, the status code, rather
//! than at the error type one or two layers in.
//! What: two undeterminable shapes per helper — a `palace.json` that cannot be
//! READ (`std::fs::read` denied) and one that cannot be STATTED (`try_exists`
//! denied, the shape PR #5574 made reachable) — plus one test that pins the
//! benign direction so the fix cannot be "everything is 500 now".
//! Test: run with `cargo test -p trusty-memory palace_open_error`.

use super::super::router;
use super::test_state;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::json;
use tower::util::ServiceExt;

/// Restore a path's mode on drop, including while unwinding from a failed
/// assertion, so a locked path never outlives the test that locked it.
///
/// The mode is a field because the two fixtures below lock different kinds of
/// path: a regular `palace.json` (0o600) and the directory holding it (0o700),
/// which would be left untraversable by a file's mode.
#[cfg(unix)]
struct RestoreMode {
    path: std::path::PathBuf,
    mode: u32,
}

#[cfg(unix)]
impl Drop for RestoreMode {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(self.mode));
    }
}

/// Create `id` through the HTTP surface and return its on-disk directory.
///
/// Why: every undeterminable case has to be a palace that genuinely exists, or
/// the test proves nothing about telling existence from absence. Creating it
/// through `POST /api/v1/palaces` also leaves a cached handle in the registry,
/// which would satisfy every later open without touching disk — so the handle
/// is evicted here too.
/// What: POSTs the palace, evicts the cached handle, returns `data_root/id`.
/// Test: the four tests below.
#[cfg(unix)]
async fn create_palace(
    state: &crate::AppState,
    app: &axum::Router,
    id: &str,
) -> std::path::PathBuf {
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

    state.data_root.join(id)
}

/// Make an existing palace's `palace.json` present and stattable but unreadable.
///
/// What: strips the file itself to mode 000, so `load_palace`'s `try_exists`
/// probe succeeds and the `std::fs::read` under it is denied —
/// `PalaceStoreError::Io`.
/// Test: the two `unreadable_palace_*` tests below.
#[cfg(unix)]
fn deny_reading_metadata(palace_dir: &std::path::Path) -> RestoreMode {
    use std::os::unix::fs::PermissionsExt;

    let target = palace_dir.join("palace.json");
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o000)).unwrap();
    let restore = RestoreMode {
        path: target.clone(),
        mode: 0o600,
    };

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

/// Make an existing palace's `palace.json` undeterminable — the probe for
/// whether it is even there fails.
///
/// Why (#5574): `load_palace` used to guard with `Path::exists()`, which is
/// `fs::metadata(..).is_ok()`, so this shape returned `NotFound` and could not
/// reach the branch under test. #5574 switched that guard to `try_exists()`, so
/// a denied stat now raises `Io` — and that is the composition this file has to
/// prove rather than assert: the two PRs stack, and BOTH undeterminable shapes
/// reach the 500 branch these callers gained.
/// What: strips the palace's own directory to mode 000, so the directory still
/// stats while stat of the `palace.json` inside it is denied.
/// Test: the two `unstattable_palace_*` tests below.
#[cfg(unix)]
fn deny_statting_metadata(palace_dir: &std::path::Path) -> RestoreMode {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(palace_dir, std::fs::Permissions::from_mode(0o000)).unwrap();
    let restore = RestoreMode {
        path: palace_dir.to_path_buf(),
        mode: 0o700,
    };

    // Same vacuous-pass guard as above, against the probe `load_palace`
    // actually makes rather than the read below it.
    let target = palace_dir.join("palace.json");
    match target.try_exists() {
        Ok(_) => panic!(
            "cannot exercise #5549: {} is still stattable with its directory at mode 000. Run \
             this suite as a non-root user on a filesystem that honours POSIX permission bits.",
            target.display()
        ),
        Err(e) => assert_eq!(
            e.kind(),
            std::io::ErrorKind::PermissionDenied,
            "expected the locked directory to deny statting palace.json, got {e}"
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
    let dir = create_palace(&state, &app, "unreadable-svc").await;
    let _restore = deny_reading_metadata(&dir);

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
    let dir = create_palace(&state, &app, "unreadable-web").await;
    let _restore = deny_reading_metadata(&dir);

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

/// Why (#5549, ADR-0045): the second undeterminable shape, and the one that
/// only became reachable when #5574 merged. `load_palace` used to answer
/// `NotFound` for a `palace.json` it could not stat, so this shape would have
/// been a 404 here no matter what these callers did; #5574 made it `Io`, which
/// means the correct status is now this crate's to get right. That the two
/// changes compose is a claim about a live code path, so it is asserted here
/// rather than inferred from the classifier matching on variants.
/// What: locks the palace's DIRECTORY (not the file) so the stat probe itself
/// is denied, and asserts `GET /api/v1/palaces/{id}/drawers` answers 500, and
/// specifically NOT 404.
/// Test: this test itself.
#[cfg(unix)]
#[tokio::test]
async fn unstattable_palace_is_500_not_404_at_the_service_open_handle() {
    let state = test_state();
    let app = router().with_state(state.clone());
    let dir = create_palace(&state, &app, "unstattable-svc").await;
    let _restore = deny_statting_metadata(&dir);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/palaces/unstattable-svc/drawers")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_ne!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "a palace whose metadata could not even be statted was reported as absent — #5574 made \
         that an Io error at load_palace, and MemoryService::open_handle flattened it back to 404"
    );
    assert_eq!(
        resp.status(),
        StatusCode::INTERNAL_SERVER_ERROR,
        "an undeterminable palace open must surface as a server-side failure"
    );
}

/// Why (#5549, ADR-0045): the unstattable shape at the second helper, for the
/// same reason as the test above — `web::error::open_handle` is a distinct
/// function serving a distinct route set, and #5574's `Io` has to reach its 500
/// branch too.
/// What: locks the palace's DIRECTORY and asserts `GET /api/v1/kg/gaps` for
/// that palace answers 500, and specifically NOT 404.
/// Test: this test itself.
#[cfg(unix)]
#[tokio::test]
async fn unstattable_palace_is_500_not_404_at_the_web_open_handle() {
    let state = test_state();
    let app = router().with_state(state.clone());
    let dir = create_palace(&state, &app, "unstattable-web").await;
    let _restore = deny_statting_metadata(&dir);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/kg/gaps?palace=unstattable-web")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_ne!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "a palace whose metadata could not even be statted was reported as absent — #5574 made \
         that an Io error at load_palace, and web::error::open_handle flattened it back to 404"
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
