//! The retired dispatch-time builder-slot routes (#6892, retired by #8261).
//!
//! Why: option D (owner ruling 2026-09-24) moved the machine-wide build cap to
//! the build command, where `tm build-lease` takes a kernel `flock` on
//! `~/.trusty-mpm/build-slots/slot-K.lock` and gets its `CARGO_TARGET_DIR` from
//! that slot. A daemon route that still handed out a `slot_path` was a SECOND
//! allocator of the same pool directories, and a dispatch granted `slot-0`
//! there could build in the directory a lease holder was already using (#8261
//! round 3, critic finding 9).
//! What: `POST /api/v1/sessions/{id}/delegations/builder-slot` and
//! `GET /api/v1/builder-slots` answer `410 Gone` with a JSON body naming the
//! replacement; nothing is claimed, recorded or granted. The build-lease
//! decision log is merged here because `api.rs` sits at its frozen line-cap
//! budget.
//! Test: the `#[cfg(test)]` suite below.

use std::sync::Arc;

use axum::{Json, Router, http::StatusCode, routing::get, routing::post};
use serde_json::{Value, json};

use crate::daemon::state::DaemonState;

/// The routes: both retired paths, plus the build-lease decision log.
///
/// Test: `the_builder_slot_route_is_gone_and_grants_no_slot_path`.
pub fn router() -> Router<Arc<DaemonState>> {
    Router::new()
        .route("/api/v1/sessions/{id}/delegations/builder-slot", post(gone))
        .route("/api/v1/builder-slots", get(gone))
        .merge(super::build_lease_routes::router())
}

/// `410 Gone`, naming what replaced the route.
async fn gone() -> (StatusCode, Json<Value>) {
    (
        StatusCode::GONE,
        Json(json!({
            "error": "the dispatch-time builder-slot cap is retired (#8261): heavy builds take \
                      a slot at the build command with `tm build-lease`, and `tm doctor`'s \
                      builder_cap row reports the holders",
        })),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    /// #8261 round 3: the daemon allocates no slot directory any more.
    #[tokio::test]
    async fn the_builder_slot_route_is_gone_and_grants_no_slot_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = Arc::new(DaemonState::with_paths(
            &crate::core::paths::FrameworkPaths::under(dir.path()),
        ));
        for (method, uri) in [
            (
                "POST",
                "/api/v1/sessions/5f0e2c1a-1111-4222-8333-944445555666/delegations/builder-slot",
            ),
            ("GET", "/api/v1/builder-slots"),
        ] {
            let response = router()
                .with_state(Arc::clone(&state))
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(uri)
                        .header("content-type", "application/json")
                        .body(Body::from(r#"{"payload":{}}"#))
                        .expect("request"),
                )
                .await
                .expect("response");
            assert_eq!(response.status(), StatusCode::GONE, "{method} {uri}");
            let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
                .await
                .expect("body");
            let text = String::from_utf8_lossy(&body);
            assert!(!text.contains("slot_path"), "{text}");
            assert!(text.contains("tm build-lease"), "{text}");
        }
    }
}
