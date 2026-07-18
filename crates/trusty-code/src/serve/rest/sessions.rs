//! `GET /sessions*` REST routes over the `session.*` JSON-RPC methods
//! (#2983 Slice 2 — read-only routes only, no writes).
//!
//! Why: Slice 1 (`super`) built the `rest::call`/`rpc_error_to_status`
//! bridge but wired no axum routes. This is the first concrete resource
//! group: session state is the thing every other `tcode` client (CLI, TUI,
//! future browser UI) polls most, so it lands first. Every handler below is
//! deliberately thin — parse the path param, build the JSON-RPC `params`,
//! call `super::respond` — because the actual behaviour (session lookup,
//! `-32007 session_not_found` mapping, …) already lives in
//! `crate::session::protocol`'s handlers; duplicating it here as bespoke
//! axum logic would fork the two surfaces (see `super` module docs).
//! What: [`routes`] builds a standalone `axum::Router<()>` (its own
//! `SessionsState` carrying just the `Arc<Router>`, `with_state`-erased
//! before return) mapping:
//!   - `GET /sessions` -> `session.list`
//!   - `GET /sessions/{id}` -> `session.status`
//!   - `GET /sessions/{id}/transcript` -> `session.get_transcript`
//!   - `GET /sessions/{id}/readiness` -> `session.get_readiness`
//!   - `GET /sessions/{id}/goals` -> `session.get_goals`
//!
//! `crate::serve::http::build_axum_router` merges this into the daemon's
//! main router alongside `POST /rpc`, `GET /health`, and
//! `GET /sessions/{id}/events` (SSE) — none of those paths collide with the
//! ones here.
//! Test: `tests::*`.

use std::sync::Arc;

use axum::Router as AxumRouter;
use axum::extract::{Path, State};
use axum::routing::get;
use serde_json::json;

use crate::jsonrpc::Router;

use super::{RestResult, respond};

/// Shared axum state for every route in this module: just the JSON-RPC
/// router — every handler here is read-only and goes through
/// [`super::respond`] rather than touching `SessionRegistry` directly.
#[derive(Clone)]
struct SessionsState {
    router: Arc<Router>,
}

/// Build the `GET /sessions*` route group.
///
/// Why: kept separate from `crate::serve::http::build_axum_router` so this
/// resource group is unit-testable via `tower::util::ServiceExt::oneshot`
/// on its own, exactly like `crate::serve::http`'s own routes.
/// What: five `GET` routes (see module docs), all sharing one
/// `SessionsState { router }`, `with_state`-erased to `axum::Router<()>` so
/// the caller can `.merge()` it into a router with a different state type.
/// Test: `tests::list_sessions_returns_sessions_array`.
pub fn routes(router: Arc<Router>) -> AxumRouter {
    AxumRouter::new()
        .route("/sessions", get(list_sessions))
        .route("/sessions/{id}", get(get_session))
        .route("/sessions/{id}/transcript", get(get_transcript))
        .route("/sessions/{id}/readiness", get(get_readiness))
        .route("/sessions/{id}/goals", get(get_goals))
        .with_state(SessionsState { router })
}

/// `GET /sessions` -> `session.list`.
///
/// Why: enumerates every session the daemon currently owns.
/// What: no params; forwards straight to `session.list`, returning its
/// `{"sessions": [...]}` result verbatim.
/// Test: `tests::list_sessions_returns_sessions_array`.
async fn list_sessions(State(state): State<SessionsState>) -> RestResult {
    respond(&state.router, "session.list", json!({})).await
}

/// `GET /sessions/{id}` -> `session.status`.
///
/// Why: point lookup for one session's current state.
/// What: `404` with a JSON-RPC `session_not_found` envelope for an unknown
/// `id`; otherwise the `Session` JSON.
/// Test: `tests::get_session_found_returns_200_with_session_json`,
/// `tests::get_session_missing_returns_404_session_not_found`.
async fn get_session(State(state): State<SessionsState>, Path(id): Path<String>) -> RestResult {
    respond(&state.router, "session.status", json!({"session_id": id})).await
}

/// `GET /sessions/{id}/transcript` -> `session.get_transcript`.
///
/// Why: read-only access to a session's persisted run record (turns,
/// aggregate usage, cost).
/// What: `404` for an unknown `id`; otherwise the `TranscriptRecord` JSON —
/// a never-run session returns an empty `turns` array, not an error (see
/// `session::protocol::get_transcript`'s docs).
/// Test: `tests::get_transcript_found_returns_200_with_empty_turns`,
/// `tests::get_transcript_missing_returns_404_session_not_found`.
async fn get_transcript(State(state): State<SessionsState>, Path(id): Path<String>) -> RestResult {
    respond(
        &state.router,
        "session.get_transcript",
        json!({"session_id": id}),
    )
    .await
}

/// `GET /sessions/{id}/readiness` -> `session.get_readiness`.
///
/// Why: lets a late-attaching client query the one-time index-readiness
/// probe it may have missed on the SSE stream.
/// What: `404` for an unknown `id`; otherwise the `ReadinessQuery` JSON
/// (`{"status":"probed",...}` or `{"status":"never_probed"}`).
/// Test: `tests::get_readiness_found_returns_200_never_probed`,
/// `tests::get_readiness_missing_returns_404_session_not_found`.
async fn get_readiness(State(state): State<SessionsState>, Path(id): Path<String>) -> RestResult {
    respond(
        &state.router,
        "session.get_readiness",
        json!({"session_id": id}),
    )
    .await
}

/// `GET /sessions/{id}/goals` -> `session.get_goals`.
///
/// Why: lets an operator/UI inspect the current 5-slot goal state without
/// pulling the larger transcript payload.
/// What: `404` for an unknown `id`; otherwise `{"goals": [...]}` — `[]` for
/// a session with no transcript yet.
/// Test: `tests::get_goals_found_returns_200_with_empty_goals`,
/// `tests::get_goals_missing_returns_404_session_not_found`.
async fn get_goals(State(state): State<SessionsState>, Path(id): Path<String>) -> RestResult {
    respond(
        &state.router,
        "session.get_goals",
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
    /// `SessionRegistry`, then the REST route group over it — mirrors
    /// `crate::serve::http::tests::router_and_sessions` but only needs the
    /// registry to seed sessions directly (these routes never touch it).
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

    /// `GET /sessions` must return `{"sessions": [...]}` with every session
    /// currently owned by the registry.
    #[tokio::test]
    async fn list_sessions_returns_sessions_array() {
        let (app, sessions) = app_and_registry();
        sessions.create("t".to_string(), None, crate::binding::ProjectBinding::None);

        let resp = get(&app, "/sessions").await;
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["sessions"].as_array().unwrap().len(), 1);
    }

    /// `GET /sessions/{id}` on a real session must return HTTP 200 with the
    /// `Session` JSON, `id` included.
    #[tokio::test]
    async fn get_session_found_returns_200_with_session_json() {
        let (app, sessions) = app_and_registry();
        let session = sessions.create("t".to_string(), None, crate::binding::ProjectBinding::None);

        let resp = get(&app, &format!("/sessions/{}", session.id)).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["id"], session.id);
    }

    /// `GET /sessions/{id}` on an unknown id must be a real HTTP 404 with a
    /// JSON-RPC `session_not_found` envelope, not a 200-wrapped error.
    #[tokio::test]
    async fn get_session_missing_returns_404_session_not_found() {
        let (app, _sessions) = app_and_registry();

        let resp = get(&app, "/sessions/does-not-exist").await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let v = body_json(resp).await;
        assert_eq!(v["error"]["code"], -32007);
    }

    /// A percent-encoded, never-created id must decode through axum's path
    /// extractor and still reach the JSON-RPC layer as a `session_not_found`
    /// 404 — proving the REST id param is never treated as "invalid" at the
    /// routing layer, only at the domain (session lookup) layer, exactly
    /// like the JSON-RPC surface itself has no separate id-format
    /// validation.
    #[tokio::test]
    async fn get_session_percent_encoded_missing_id_returns_404() {
        let (app, _sessions) = app_and_registry();

        let resp = get(&app, "/sessions/not%20a%20real%20id").await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let v = body_json(resp).await;
        assert_eq!(v["error"]["code"], -32007);
    }

    /// `GET /sessions/{id}/transcript` on a never-run session must return
    /// HTTP 200 with an empty `turns` array, not an error.
    #[tokio::test]
    async fn get_transcript_found_returns_200_with_empty_turns() {
        let (app, sessions) = app_and_registry();
        let session = sessions.create("t".to_string(), None, crate::binding::ProjectBinding::None);

        let resp = get(&app, &format!("/sessions/{}/transcript", session.id)).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["turns"].as_array().unwrap().len(), 0);
    }

    /// `GET /sessions/{id}/transcript` on an unknown id must 404.
    #[tokio::test]
    async fn get_transcript_missing_returns_404_session_not_found() {
        let (app, _sessions) = app_and_registry();

        let resp = get(&app, "/sessions/does-not-exist/transcript").await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let v = body_json(resp).await;
        assert_eq!(v["error"]["code"], -32007);
    }

    /// `GET /sessions/{id}/readiness` on a never-probed session must return
    /// HTTP 200 with `status: "never_probed"`, not an error.
    #[tokio::test]
    async fn get_readiness_found_returns_200_never_probed() {
        let (app, sessions) = app_and_registry();
        let session = sessions.create("t".to_string(), None, crate::binding::ProjectBinding::None);

        let resp = get(&app, &format!("/sessions/{}/readiness", session.id)).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["status"], "never_probed");
    }

    /// `GET /sessions/{id}/readiness` on an unknown id must 404.
    #[tokio::test]
    async fn get_readiness_missing_returns_404_session_not_found() {
        let (app, _sessions) = app_and_registry();

        let resp = get(&app, "/sessions/does-not-exist/readiness").await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let v = body_json(resp).await;
        assert_eq!(v["error"]["code"], -32007);
    }

    /// `GET /sessions/{id}/goals` on a never-run session must return HTTP
    /// 200 with an empty `goals` array, not an error.
    #[tokio::test]
    async fn get_goals_found_returns_200_with_empty_goals() {
        let (app, sessions) = app_and_registry();
        let session = sessions.create("t".to_string(), None, crate::binding::ProjectBinding::None);

        let resp = get(&app, &format!("/sessions/{}/goals", session.id)).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["goals"].as_array().unwrap().len(), 0);
    }

    /// `GET /sessions/{id}/goals` on an unknown id must 404.
    #[tokio::test]
    async fn get_goals_missing_returns_404_session_not_found() {
        let (app, _sessions) = app_and_registry();

        let resp = get(&app, "/sessions/does-not-exist/goals").await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let v = body_json(resp).await;
        assert_eq!(v["error"]["code"], -32007);
    }
}
