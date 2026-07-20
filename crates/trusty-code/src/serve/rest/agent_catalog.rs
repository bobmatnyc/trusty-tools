//! `GET`/`POST /agents`, `DELETE /agents/{name}` REST routes over the
//! `agents.*` JSON-RPC surface (issue #3449, Foundry GUI Agents tab).
//!
//! Why: mirrors every other resource group in this gateway — a REST-only
//! client (the GUI) gets the same `agents.*` surface `POST /rpc` exposes,
//! without duplicating `crate::agents::protocol`'s business logic as bespoke
//! axum handlers (see `super` module docs' "forking the two surfaces"
//! rationale). Named `agent_catalog` (not `agents`, which
//! `crate::serve::rest::agents` already claims for `GET
//! /sessions/{id}/agents` — the per-session LIVE roster route, a distinct
//! resource) to avoid a module-name collision while both mount under the
//! literal `/agents` path prefix at the HTTP layer (no path collision: one
//! is nested under `/sessions/{id}/`, this one is top-level).
//! What: [`routes`] builds a standalone `axum::Router<()>` mapping:
//!   - `GET /agents` -> `agents.list`
//!   - `POST /agents` -> `agents.create{name, content}` (`201 Created`)
//!   - `DELETE /agents/{name}` -> `agents.delete{name}`
//!
//! `crate::serve::http::build_axum_router` merges this into the daemon's
//! main router alongside every other REST resource group.
//!
//! Test: `tests::*`.

use std::sync::Arc;

use axum::Json;
use axum::Router as AxumRouter;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::get;
use serde::Deserialize;
use serde_json::{Value, json};
use trusty_common::mcp::Response;

use crate::jsonrpc::Router;

use super::{RestResult, respond};

/// Shared axum state for every route in this module: just the JSON-RPC
/// router, mirroring `super::workstreams::WorkstreamsState`.
#[derive(Clone)]
struct AgentCatalogState {
    router: Arc<Router>,
}

/// Build the `GET`/`POST /agents`, `DELETE /agents/{name}` route group.
///
/// Why: kept separate from `crate::serve::http::build_axum_router` so this
/// resource group is unit-testable via `tower::util::ServiceExt::oneshot` on
/// its own, exactly like every other `rest::*` group.
/// Test: `tests::create_agent_returns_201`, `tests::list_agents_returns_200`,
/// `tests::delete_agent_returns_200`.
pub fn routes(router: Arc<Router>) -> AxumRouter {
    AxumRouter::new()
        .route("/agents", get(list_agents).post(create_agent))
        .route("/agents/{name}", axum::routing::delete(delete_agent))
        .with_state(AgentCatalogState { router })
}

/// `GET /agents` -> `agents.list`.
async fn list_agents(State(state): State<AgentCatalogState>) -> RestResult {
    respond(&state.router, "agents.list", Value::Null).await
}

/// Request body for `POST /agents`.
#[derive(Deserialize)]
struct CreateBody {
    name: String,
    content: String,
}

/// Like [`super::respond`] but reports `201 Created` on success — this
/// module mints a brand-new disk-tier agent (mirrors
/// `super::workstreams::respond_created`).
async fn respond_created(
    router: &Router,
    method: &str,
    params: Value,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Response>)> {
    respond(router, method, params)
        .await
        .map(|Json(v)| (StatusCode::CREATED, Json(v)))
}

/// `POST /agents` -> `agents.create`.
async fn create_agent(
    State(state): State<AgentCatalogState>,
    Json(body): Json<CreateBody>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Response>)> {
    respond_created(
        &state.router,
        "agents.create",
        json!({"name": body.name, "content": body.content}),
    )
    .await
}

/// `DELETE /agents/{name}` -> `agents.delete`.
async fn delete_agent(
    State(state): State<AgentCatalogState>,
    Path(name): Path<String>,
) -> RestResult {
    respond(&state.router, "agents.delete", json!({"name": name})).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use tower::util::ServiceExt;

    fn app() -> AxumRouter {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().to_path_buf();
        // Mirrors `super::workstreams::tests::app`'s `std::mem::forget(dir)`
        // convention — keeps the directory alive for the lifetime of this
        // test's requests instead of deleting it when `tmp` drops.
        std::mem::forget(tmp);
        let mut router = Router::new();
        crate::agents::protocol::register(
            &mut router,
            crate::agents::protocol::AgentsCatalogState::new(dir, true),
        );
        routes(Arc::new(router))
    }

    async fn body_json(response: axum::response::Response) -> Value {
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn list_agents_returns_200() {
        let resp = app()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/agents")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert!(v["agents"].is_array());
    }

    #[tokio::test]
    async fn create_agent_returns_201() {
        let a = app();
        let resp = a
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/agents")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"name":"my-agent","content":"---\nname: my-agent\n---\n\nBody.\n"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let v = body_json(resp).await;
        assert_eq!(v["name"], "my-agent");
    }

    #[tokio::test]
    async fn create_embedded_collision_returns_403() {
        let a = app();
        let resp = a
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/agents")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"name":"engineer","content":"---\nname: engineer\n---\n\nBody.\n"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    /// A second `POST /agents` for the same name must surface as `409
    /// Conflict` at the REST layer — `rpc_error_to_status` mapping
    /// `RpcError::already_exists`'s `-32009` (code-critic PR #3465 review,
    /// HIGH 1's atomic `write_new_file` fix), not the old `-32008` slot.
    #[tokio::test]
    async fn create_conflict_returns_409() {
        let a = app();
        let body =
            Body::from(r#"{"name":"dup-agent","content":"---\nname: dup-agent\n---\n\nBody.\n"}"#);
        let first = a
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/agents")
                    .header("content-type", "application/json")
                    .body(body)
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::CREATED);

        let second = a
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/agents")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"name":"dup-agent","content":"---\nname: dup-agent\n---\n\nCLOBBER.\n"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(second.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn delete_agent_returns_200() {
        let a = app();
        a.clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/agents")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"name":"my-agent","content":"---\nname: my-agent\n---\n\nBody.\n"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        let resp = a
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/agents/my-agent")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn delete_missing_agent_returns_404() {
        let resp = app()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/agents/totally-bogus")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_embedded_agent_returns_403() {
        let resp = app()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/agents/engineer")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }
}
