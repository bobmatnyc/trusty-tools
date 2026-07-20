//! `GET`/`POST /skills`, `DELETE /skills/{name}` REST routes over the
//! `skills.*` JSON-RPC surface (issue #3449, Foundry GUI Skills tab).
//!
//! Why: mirrors `super::agent_catalog`'s shape exactly — a REST-only client
//! (the GUI) gets the same `skills.*` surface `POST /rpc` exposes, without
//! duplicating `crate::skills::protocol`'s business logic as bespoke axum
//! handlers.
//! What: [`routes`] builds a standalone `axum::Router<()>` mapping:
//!   - `GET /skills` -> `skills.list`
//!   - `POST /skills` -> `skills.create{name, content}` (`201 Created`)
//!   - `DELETE /skills/{name}` -> `skills.delete{name}`
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
/// router, mirroring `super::agent_catalog::AgentCatalogState`.
#[derive(Clone)]
struct SkillCatalogState {
    router: Arc<Router>,
}

/// Build the `GET`/`POST /skills`, `DELETE /skills/{name}` route group.
///
/// Test: `tests::create_skill_returns_201`, `tests::list_skills_returns_200`,
/// `tests::delete_skill_returns_200`.
pub fn routes(router: Arc<Router>) -> AxumRouter {
    AxumRouter::new()
        .route("/skills", get(list_skills).post(create_skill))
        .route("/skills/{name}", axum::routing::delete(delete_skill))
        .with_state(SkillCatalogState { router })
}

/// `GET /skills` -> `skills.list`.
async fn list_skills(State(state): State<SkillCatalogState>) -> RestResult {
    respond(&state.router, "skills.list", Value::Null).await
}

/// Request body for `POST /skills`.
#[derive(Deserialize)]
struct CreateBody {
    name: String,
    content: String,
}

/// Like [`super::respond`] but reports `201 Created` on success.
async fn respond_created(
    router: &Router,
    method: &str,
    params: Value,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Response>)> {
    respond(router, method, params)
        .await
        .map(|Json(v)| (StatusCode::CREATED, Json(v)))
}

/// `POST /skills` -> `skills.create`.
async fn create_skill(
    State(state): State<SkillCatalogState>,
    Json(body): Json<CreateBody>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Response>)> {
    respond_created(
        &state.router,
        "skills.create",
        json!({"name": body.name, "content": body.content}),
    )
    .await
}

/// `DELETE /skills/{name}` -> `skills.delete`.
async fn delete_skill(
    State(state): State<SkillCatalogState>,
    Path(name): Path<String>,
) -> RestResult {
    respond(&state.router, "skills.delete", json!({"name": name})).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use tower::util::ServiceExt;

    fn app() -> AxumRouter {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        // Mirrors `super::workstreams::tests::app`'s `std::mem::forget(dir)`
        // convention.
        std::mem::forget(tmp);
        let mut router = Router::new();
        crate::skills::protocol::register(
            &mut router,
            crate::skills::protocol::SkillsCatalogState::new(Some(&root)),
        );
        routes(Arc::new(router))
    }

    async fn body_json(response: axum::response::Response) -> Value {
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn list_skills_returns_200() {
        let resp = app()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/skills")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert!(v["skills"].is_array());
    }

    #[tokio::test]
    async fn create_skill_returns_201() {
        let a = app();
        let resp = a
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/skills")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"name":"my-skill","content":"---\nname: my-skill\n---\n\nBody.\n"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let v = body_json(resp).await;
        assert_eq!(v["name"], "my-skill");
    }

    #[tokio::test]
    async fn delete_skill_returns_200() {
        let a = app();
        a.clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/skills")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"name":"my-skill","content":"---\nname: my-skill\n---\n\nBody.\n"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        let resp = a
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/skills/my-skill")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn delete_missing_skill_returns_404() {
        let resp = app()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/skills/totally-bogus")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn create_projectless_returns_400() {
        let mut router = Router::new();
        crate::skills::protocol::register(
            &mut router,
            crate::skills::protocol::SkillsCatalogState::new(None),
        );
        let a = routes(Arc::new(router));
        let resp = a
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/skills")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"name":"my-skill","content":"x"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}
