//! `GET /projects` REST route over the `fs.list_projects` JSON-RPC method
//! (issue #3365).
//!
//! Why: the GUI's workstream-first project-picker modal needs a small,
//! read-only, loopback-only roster of candidate projects (ADR-0011 — this
//! daemon binds loopback only, matching every other route in this gateway).
//! Mirrors `super::fs`'s shape exactly: a thin handler over
//! `crate::fs_browse::protocol::list_projects` via [`super::respond`], never
//! duplicating the scan logic (`crate::fs_browse::roster::list_projects`) as
//! a bespoke axum handler — see `super` module docs' "forking the two
//! surfaces" rationale.
//! What: [`routes`] builds a standalone `axum::Router<()>` (its own
//! `ProjectsState` carrying just the `Arc<Router>`, `with_state`-erased
//! before return, mirroring `super::fs::FsState`) mapping `GET /projects` ->
//! `fs.list_projects`. No query params — the roster takes none.
//! `crate::serve::http::build_axum_router` merges this into the daemon's
//! main router alongside `POST /rpc`, `GET /health`, `GET /fs`, and the
//! other REST resource groups — `/projects` collides with none of them.
//! Test: `tests::*`.

use std::sync::Arc;

use axum::Router as AxumRouter;
use axum::extract::State;
use axum::routing::get;
use serde_json::Value;

use crate::jsonrpc::Router;

use super::{RestResult, respond};

/// Shared axum state for the route in this module: just the JSON-RPC
/// router, mirroring `super::fs::FsState` — the handler goes through
/// [`super::respond`] rather than touching the filesystem directly.
#[derive(Clone)]
struct ProjectsState {
    router: Arc<Router>,
}

/// Build the `GET /projects` route group.
///
/// Why: kept separate from `crate::serve::http::build_axum_router` so this
/// resource group is unit-testable via `tower::util::ServiceExt::oneshot` on
/// its own, exactly like every other `rest::*` group.
/// What: one `GET` route, `ProjectsState { router }`, `with_state`-erased to
/// `axum::Router<()>` so the caller can `.merge()` it alongside the other
/// resource groups into the daemon's main router.
/// Test: `tests::list_projects_returns_200_with_entries_array`.
pub fn routes(router: Arc<Router>) -> AxumRouter {
    AxumRouter::new()
        .route("/projects", get(list_projects))
        .with_state(ProjectsState { router })
}

/// `GET /projects` -> `fs.list_projects`.
///
/// Why: the REST entry point for the project-picker modal's roster fetch.
/// What: `200` with the `ProjectRoster` JSON (`{"entries": [...]}`) — the
/// underlying RPC method never returns a caller-facing error (see
/// `fs_browse::protocol::list_projects`'s docs), so this route has no
/// documented error path of its own beyond a lower-level dispatch failure.
/// Test: `tests::list_projects_returns_200_with_entries_array`.
async fn list_projects(State(state): State<ProjectsState>) -> RestResult {
    respond(&state.router, "fs.list_projects", Value::Null).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use serde_json::Value as JsonValue;
    use tower::util::ServiceExt;

    fn app() -> AxumRouter {
        let mut router = Router::new();
        crate::fs_browse::protocol::register(&mut router);
        routes(Arc::new(router))
    }

    async fn body_json(response: axum::response::Response) -> JsonValue {
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// `GET /projects` must be reachable and return the `entries` array
    /// shape the picker modal dereferences. Runs against the real
    /// developer/CI-runner home directory (no injectable seam at the REST
    /// layer — `fs_browse::roster::tests` carries the real scan-behaviour
    /// coverage), so this only pins the route + wire shape.
    #[tokio::test]
    async fn list_projects_returns_200_with_entries_array() {
        let resp = app()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/projects")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert!(v["entries"].is_array());
    }
}
