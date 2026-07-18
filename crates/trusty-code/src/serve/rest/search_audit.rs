//! `GET /sessions/{id}/search-audit` REST route over the
//! `session.get_search_audit` JSON-RPC method (issue #3072).
//!
//! Why: DOC-39 §4.7 (Search tab, 10d) is the audit trail of agent
//! search/recall operations — explicitly not an input box (AC-7.1) — and
//! #3027's 8b monitor card needs the same settled history. Both need a
//! REST-pollable snapshot rather than becoming SSE consumers of
//! `GET /sessions/{id}/events` (DOC-39 §2.1). As with every route in this
//! gateway, the handler stays thin — extract the path param, call
//! `super::respond` — because the actual behaviour (the retained-list read,
//! `-32007 session_not_found` mapping) already lives in
//! `crate::session::registry_search_audit`/`protocol_search_audit`;
//! duplicating it here would fork the two surfaces (see `super` module
//! docs).
//! What: [`routes`] builds a standalone `axum::Router<()>` (its own
//! `SearchAuditState` carrying just the `Arc<Router>`, `with_state`-erased
//! before return, mirroring `super::agents::AgentsState`) mapping
//! `GET /sessions/{id}/search-audit` -> `session.get_search_audit`.
//! `crate::serve::http::build_axum_router` merges this into the daemon's main
//! router alongside `POST /rpc`, `GET /health`, `GET /sessions/{id}/events`,
//! and the other REST resource groups. `/sessions/{id}/search-audit` does not
//! collide with any path the other slices already registered under
//! `/sessions/{id}`.
//! Test: `tests::*`.

use std::sync::Arc;

use axum::Router as AxumRouter;
use axum::extract::{Path, State};
use axum::routing::get;
use serde_json::json;

use crate::jsonrpc::Router;

use super::{RestResult, respond};

/// Shared axum state for the route in this module: just the JSON-RPC
/// router, mirroring `super::agents::AgentsState` — the handler goes through
/// [`super::respond`] rather than touching `SessionRegistry` directly.
#[derive(Clone)]
struct SearchAuditState {
    router: Arc<Router>,
}

/// Build the `GET /sessions/{id}/search-audit` route group.
///
/// Why: kept separate from `crate::serve::http::build_axum_router` so this
/// resource group is unit-testable via `tower::util::ServiceExt::oneshot` on
/// its own, exactly like every other `rest::*` group.
/// What: one `GET` route, `SearchAuditState { router }`, `with_state`-erased
/// to `axum::Router<()>` so the caller can `.merge()` it alongside the other
/// resource groups into the daemon's main router.
/// Test: `tests::get_search_audit_found_returns_200_with_empty_list`.
pub fn routes(router: Arc<Router>) -> AxumRouter {
    AxumRouter::new()
        .route("/sessions/{id}/search-audit", get(get_search_audit))
        .with_state(SearchAuditState { router })
}

/// `GET /sessions/{id}/search-audit` -> `session.get_search_audit`.
///
/// Why: lets an operator/UI inspect a session's search/recall history
/// without becoming an SSE consumer of the live event stream.
/// What: `404` for an unknown `id`; otherwise `{"search_audit": [...]}` —
/// `[]` for a session with no search/recall activity yet.
/// Test: `tests::get_search_audit_found_returns_200_with_empty_list`,
/// `tests::get_search_audit_missing_returns_404_session_not_found`.
async fn get_search_audit(
    State(state): State<SearchAuditState>,
    Path(id): Path<String>,
) -> RestResult {
    respond(
        &state.router,
        "session.get_search_audit",
        json!({"session_id": id}),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use serde_json::Value;
    use tower::util::ServiceExt;

    /// Build a router wired with every `session.*` method plus a fresh
    /// `SessionRegistry`, then this module's route group over it — mirrors
    /// `agents::tests::app_and_registry`.
    fn app_and_registry() -> (AxumRouter, Arc<crate::session::SessionRegistry>) {
        let sessions = Arc::new(crate::session::SessionRegistry::new());
        let mut router = Router::new();
        crate::session::protocol::register(&mut router, sessions.clone());
        let app = routes(Arc::new(router));
        (app, sessions)
    }

    async fn body_json(response: axum::response::Response) -> Value {
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn get(app: &AxumRouter, uri: &str) -> axum::response::Response {
        app.clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    /// `GET /sessions/{id}/search-audit` on a real session with no
    /// search/recall activity yet must return `200` with an empty
    /// `search_audit` array, not an error.
    #[tokio::test]
    async fn get_search_audit_found_returns_200_with_empty_list() {
        let (app, sessions) = app_and_registry();
        let session = sessions.create("t".to_string(), None, crate::binding::ProjectBinding::None);

        let resp = get(&app, &format!("/sessions/{}/search-audit", session.id)).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["search_audit"].as_array().unwrap().len(), 0);
    }

    /// `GET /sessions/{id}/search-audit` on an unknown id must be a real
    /// `404` with a `session_not_found` envelope.
    #[tokio::test]
    async fn get_search_audit_missing_returns_404_session_not_found() {
        let (app, _sessions) = app_and_registry();

        let resp = get(&app, "/sessions/does-not-exist/search-audit").await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let v = body_json(resp).await;
        assert_eq!(v["error"]["code"], -32007);
    }
}
