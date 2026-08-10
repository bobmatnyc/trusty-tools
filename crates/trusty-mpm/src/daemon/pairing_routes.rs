//! The `/pair/*` route registrations as one sub-router.
//!
//! Why: `api.rs` is grandfathered over the 500-SLOC production cap at a FROZEN
//! line-cap budget, so it cannot grow by even one `.route(…)` line — and #4480
//! needs one. Extracting a cohesive, already-complete route cluster into a
//! sub-router buys that room and moves the file in the direction the ratchet
//! wants. The pairing cluster is the natural candidate: four verbs of one
//! handshake, backed by one service ([`super::services::PairingService`]) and
//! one store ([`super::pairing_store`]), with nothing else registered against
//! them. This mirrors [`super::managed_routes::reconcile::worktree_routes`],
//! which was created for the same reason.
//!
//! What: registration only. The four handlers stay in `api.rs` next to their
//! request/response types and their `utoipa` annotations, and are referenced
//! here by path — moving them too would be a large diff for no gain, since the
//! cap is about file size and the handlers are what make `api.rs` large in a way
//! this change is not trying to solve.
//! Test: `pair_request_returns_code` and `pair_confirm_rejects_bad_code` in
//! `super::api_tests` drive the handlers; `router_registers_the_pairing_verbs`
//! below pins that all four stay registered after the move.

use std::sync::Arc;

use axum::{
    Router,
    routing::{get, post},
};

use crate::daemon::state::DaemonState;

/// The four pairing routes as one sub-router.
///
/// Why: see the module doc.
/// What: `POST /pair/request`, `POST /pair/confirm`, `GET /pair/status`,
/// `POST /pair/reset` — the same four registrations, unchanged, that were
/// inline in [`super::api::router`].
/// Test: `router_registers_the_pairing_verbs`.
pub fn router() -> Router<Arc<DaemonState>> {
    Router::new()
        .route("/pair/request", post(super::api::pair_request))
        .route("/pair/confirm", post(super::api::pair_confirm))
        .route("/pair/status", get(super::api::pair_status))
        .route("/pair/reset", post(super::api::pair_reset))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::paths::FrameworkPaths;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    /// Every pairing verb must still answer after the extraction.
    ///
    /// Why: the failure mode of a registration move is silent — a dropped
    /// `.route` line 404s at runtime and no compiler sees it. This drives all
    /// four through the router itself rather than calling the handlers.
    #[tokio::test]
    async fn router_registers_the_pairing_verbs() {
        let dir = tempfile::tempdir().expect("temp dir");
        let paths = FrameworkPaths::under(dir.path());
        let state = Arc::new(DaemonState::with_paths(&paths));

        // All FOUR, matching this module's doc. `/pair/confirm` takes a JSON
        // body it will not get here — it may answer 4xx for the missing body,
        // but a 404 would mean the registration was dropped in the move.
        for (method, path) in [
            ("POST", "/pair/request"),
            ("POST", "/pair/confirm"),
            ("GET", "/pair/status"),
            ("POST", "/pair/reset"),
        ] {
            let response = router()
                .with_state(state.clone())
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(path)
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("router responds");
            assert_ne!(
                response.status(),
                StatusCode::NOT_FOUND,
                "{method} {path} must stay registered"
            );
        }
    }
}
