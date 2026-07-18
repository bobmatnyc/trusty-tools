//! `GET /sessions/{id}/agents` REST route over the `session.get_agents`
//! JSON-RPC method (#2983 Slice 6).
//!
//! Why: DOC-39 §5.4 names `session.get_agents` "the principled endpoint" a
//! late-attaching UI client needs to ask "who is running right now?" for one
//! session, without folding the SSE ring-buffer replay itself (see
//! `crate::session::protocol_agents` module docs). As with every route in
//! this gateway, the handler stays thin — extract the path param, call
//! `super::respond` — because the actual behaviour (the live per-agent
//! roster fold, `-32007 session_not_found` mapping) already lives in
//! `crate::session::registry_agents`/`protocol_agents`; duplicating it here
//! would fork the two surfaces (see `super` module docs).
//! What: [`routes`] builds a standalone `axum::Router<()>` (its own
//! `AgentsState` carrying just the `Arc<Router>`, `with_state`-erased before
//! return, mirroring `super::sessions::SessionsState`) mapping
//! `GET /sessions/{id}/agents` -> `session.get_agents`.
//!
//! **Deviation from the literal `GET /agents` shape:** `session.get_agents`
//! REQUIRES a `session_id` param (verified in
//! `crate::session::protocol_agents::get_agents` and
//! `crate::session::protocol::SessionIdParams`) — there is no roster
//! independent of a session. A bare `GET /agents` route would therefore have
//! nothing to forward as `session_id`. Nesting it under `/sessions/{id}`
//! instead is both correct per the RPC's actual signature AND consistent
//! with how Slice 2 (`super::sessions`) already nests every other
//! session-scoped read route (`.../transcript`, `.../readiness`,
//! `.../goals`) — `/sessions/{id}/agents` slots into that same family.
//! `crate::serve::http::build_axum_router` merges this into the daemon's main
//! router alongside `POST /rpc`, `GET /health`, `GET /sessions/{id}/events`,
//! and the other REST resource groups. `/sessions/{id}/agents` does not
//! collide with any path Slice 2/3 already registered under `/sessions/{id}`.
//! Test: `tests::*`.

use std::sync::Arc;

use axum::Router as AxumRouter;
use axum::extract::{Path, State};
use axum::routing::get;
use serde_json::json;

use crate::jsonrpc::Router;

use super::{RestResult, respond};

/// Shared axum state for the route in this module: just the JSON-RPC
/// router, mirroring `super::sessions::SessionsState` — the handler goes
/// through [`super::respond`] rather than touching `SessionRegistry`
/// directly.
#[derive(Clone)]
struct AgentsState {
    router: Arc<Router>,
}

/// Build the `GET /sessions/{id}/agents` route group.
///
/// Why: kept separate from `crate::serve::http::build_axum_router` so this
/// resource group is unit-testable via `tower::util::ServiceExt::oneshot` on
/// its own, exactly like every other `rest::*` group.
/// What: one `GET` route, `AgentsState { router }`, `with_state`-erased to
/// `axum::Router<()>` so the caller can `.merge()` it alongside the other
/// resource groups into the daemon's main router.
/// Test: `tests::get_agents_found_returns_200_with_empty_roster`.
pub fn routes(router: Arc<Router>) -> AxumRouter {
    AxumRouter::new()
        .route("/sessions/{id}/agents", get(get_agents))
        .with_state(AgentsState { router })
}

/// `GET /sessions/{id}/agents` -> `session.get_agents`.
///
/// Why: lets an operator/UI inspect who is running (or has run) within a
/// session without pulling the larger transcript payload.
/// What: `404` for an unknown `id`; otherwise `{"agents": [...]}` — `[]` for
/// a session with no tool activity yet.
/// Test: `tests::get_agents_found_returns_200_with_empty_roster`,
/// `tests::get_agents_missing_returns_404_session_not_found`.
async fn get_agents(State(state): State<AgentsState>, Path(id): Path<String>) -> RestResult {
    respond(
        &state.router,
        "session.get_agents",
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
    /// `sessions::tests::app_and_registry`.
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

    /// `GET /sessions/{id}/agents` on a real session with no tool activity
    /// yet must return `200` with an empty `agents` array, not an error.
    #[tokio::test]
    async fn get_agents_found_returns_200_with_empty_roster() {
        let (app, sessions) = app_and_registry();
        let session = sessions.create("t".to_string(), None, crate::binding::ProjectBinding::None);

        let resp = get(&app, &format!("/sessions/{}/agents", session.id)).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["agents"].as_array().unwrap().len(), 0);
    }

    /// `GET /sessions/{id}/agents` on an unknown id must be a real `404` with
    /// a `session_not_found` envelope.
    #[tokio::test]
    async fn get_agents_missing_returns_404_session_not_found() {
        let (app, _sessions) = app_and_registry();

        let resp = get(&app, "/sessions/does-not-exist/agents").await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let v = body_json(resp).await;
        assert_eq!(v["error"]["code"], -32007);
    }
}
