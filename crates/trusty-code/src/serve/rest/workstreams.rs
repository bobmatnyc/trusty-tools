//! `POST /workstreams/{id}/activate` and `POST /workstreams/{id}/deactivate`
//! REST routes over the `workstream.activate`/`workstream.deactivate`
//! JSON-RPC methods (DOC-48 §5.2/§6, issue #3294).
//!
//! # Spec References
//!
//! - [`SPEC-WS-05~draft`](docs/specs/DOC-48-tcode-workstreams.md#SPEC-WS-05~draft)
//!
//! Why: mirrors every other REST resource group in this module
//! (`super::sessions`/`super::sessions_write`/`super::tasks`) — a thin axum
//! handler calling [`super::respond`] against the ALREADY-implemented
//! `workstream.*` JSON-RPC handler (`crate::workstreams::protocol`), so the
//! REST and JSON-RPC surfaces can never fork (see `super` module docs).
//!
//! **Path convention note:** DOC-48 §5.2 specifies `/api/v1/workstreams/...`
//! paths, but every REST resource this crate has actually shipped so far
//! (`/sessions`, `/tasks`, `/fs`, …) is unprefixed — there is no `/api/v1`
//! mount anywhere in `crate::serve::http::build_axum_router`. This resolves
//! the ambiguity by following the established code convention (unprefixed
//! `/workstreams/...`) rather than the spec's literal path text, exactly as
//! every prior REST slice has; a future ticket that wants to introduce an
//! `/api/v1` prefix would need to re-mount every existing resource, not just
//! this one.
//!
//! **Scope note (issue #3294):** only `activate`/`deactivate` land here —
//! `POST /workstreams` (create), `GET /workstreams`, `GET /workstreams/{id}`,
//! and `POST /workstreams/{id}/close` are issue #3295's REST surface.
//!
//! What: [`routes`] builds a standalone `axum::Router<()>` mapping:
//!   - `POST /workstreams/{id}/activate` -> `workstream.activate{id, force?}`
//!     (JSON body `{"force": bool}`, defaulting `force` to `false` when the
//!     body omits it or is `{}`)
//!   - `POST /workstreams/{id}/deactivate` -> `workstream.deactivate{id}`
//!     (no body, mirroring `super::sessions_write`'s `POST
//!     /sessions/{id}/cancel`)
//!
//! `crate::serve::http::build_axum_router` merges this in alongside every
//! other REST slice; none of its paths collide.
//! Test: `tests::*`.

use std::sync::Arc;

use axum::Json;
use axum::Router as AxumRouter;
use axum::extract::{Path, State};
use axum::routing::post;
use serde::Deserialize;
use serde_json::json;

use crate::jsonrpc::Router;

use super::{RestResult, respond};

/// Shared axum state for this route group: just the JSON-RPC router,
/// mirroring every other REST slice's `*State` struct.
#[derive(Clone)]
struct WorkstreamsState {
    router: Arc<Router>,
}

/// Build the `POST /workstreams/{id}/activate|deactivate` route group.
///
/// Why: kept separate from `crate::serve::http::build_axum_router` so this
/// resource group is unit-testable via `tower::util::ServiceExt::oneshot`
/// on its own, exactly like every other REST slice.
/// What: two `POST` routes (see module docs), sharing one
/// `WorkstreamsState { router }`, `with_state`-erased to `axum::Router<()>`.
/// Test: `tests::activate_returns_200_with_active_id`.
pub fn routes(router: Arc<Router>) -> AxumRouter {
    AxumRouter::new()
        .route("/workstreams/{id}/activate", post(activate))
        .route("/workstreams/{id}/deactivate", post(deactivate))
        .with_state(WorkstreamsState { router })
}

/// Request body for `POST /workstreams/{id}/activate` — `id` comes from the
/// path, so only `force` remains. `#[serde(default)]` means `{}` (or a body
/// that just omits the field) is a valid, non-`force` request.
#[derive(Deserialize, Default)]
struct ActivateBody {
    #[serde(default)]
    force: bool,
}

/// `POST /workstreams/{id}/activate` -> `workstream.activate`.
///
/// Why: the REST entry point for DOC-48 §6.1's activation-lock exclusivity
/// model.
/// What: `200` with `{"active_id", "prior_id"}` on success; `404` for an
/// unknown `id` (`-32002 not_found`); `409` when a DIFFERENT workstream is
/// active and `force` was not set (`-32008 active_conflict`, see
/// `crate::jsonrpc::error::RpcError::active_conflict`'s docs and
/// `super::rpc_error_to_status`'s `-32008 -> 409` mapping).
/// Test: `tests::activate_returns_200_with_active_id`,
/// `tests::activate_conflict_returns_409`,
/// `tests::activate_unknown_id_returns_404`.
async fn activate(
    State(state): State<WorkstreamsState>,
    Path(id): Path<String>,
    Json(body): Json<ActivateBody>,
) -> RestResult {
    respond(
        &state.router,
        "workstream.activate",
        json!({"id": id, "force": body.force}),
    )
    .await
}

/// `POST /workstreams/{id}/deactivate` -> `workstream.deactivate`.
///
/// Why: the REST entry point for clearing the active pointer — never needs
/// a body, `id` from the path is the whole request (mirrors
/// `super::sessions_write::cancel_session`).
/// What: `200` with `{}` on success — idempotent for an idle/unknown `id`
/// (see `crate::workstreams::activation::deactivate`'s docs), so this route
/// has no documented error path of its own beyond a lower-level store
/// failure (`500`).
/// Test: `tests::deactivate_returns_200_empty_object`.
async fn deactivate(State(state): State<WorkstreamsState>, Path(id): Path<String>) -> RestResult {
    respond(&state.router, "workstream.deactivate", json!({"id": id})).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use serde_json::Value;
    use tower::util::ServiceExt;

    /// Build the route group over a fresh, tempdir-backed workstream store
    /// with one workstream already created — mirrors
    /// `crate::workstreams::protocol_tests`' own helper, but exercised
    /// through the REST layer rather than calling the handler functions
    /// directly.
    async fn app_and_id() -> (AxumRouter, String, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("workstreams-test.json");
        let mut store = crate::workstreams::WorkstreamStore::load(path)
            .await
            .expect("load fresh store");
        let id = store.create("t").await.expect("create");
        let store = Arc::new(tokio::sync::Mutex::new(store));

        let mut router = Router::new();
        crate::workstreams::protocol::register(&mut router, store);
        let app = routes(Arc::new(router));
        (app, id.to_string(), dir)
    }

    async fn body_json(response: axum::response::Response) -> Value {
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn post(app: &AxumRouter, uri: &str, body: Value) -> axum::response::Response {
        app.clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    /// Activating with no prior active workstream must return `200` with
    /// the new `active_id` and a null `prior_id`.
    #[tokio::test]
    async fn activate_returns_200_with_active_id() {
        let (app, id, _dir) = app_and_id().await;

        let resp = post(&app, &format!("/workstreams/{id}/activate"), json!({})).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["active_id"], json!(id));
        assert_eq!(v["prior_id"], Value::Null);
    }

    /// Activating a DIFFERENT workstream without `force` while one is
    /// active must return a real HTTP `409`, not a 200-wrapped error.
    #[tokio::test]
    async fn activate_conflict_returns_409() {
        let (app, id, dir) = app_and_id().await;
        let mut store =
            crate::workstreams::WorkstreamStore::load(dir.path().join("workstreams-test.json"))
                .await
                .expect("reload store");
        let other = store.create("other").await.expect("create other");

        post(&app, &format!("/workstreams/{id}/activate"), json!({})).await;

        let resp = post(
            &app,
            &format!("/workstreams/{other}/activate"),
            json!({"force": false}),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        let v = body_json(resp).await;
        assert_eq!(v["error"]["code"], -32008);
    }

    /// Activating an id that names no existing workstream must return a
    /// real HTTP `404`.
    #[tokio::test]
    async fn activate_unknown_id_returns_404() {
        let (app, _id, _dir) = app_and_id().await;

        let resp = post(
            &app,
            &format!("/workstreams/{}/activate", uuid::Uuid::new_v4()),
            json!({}),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let v = body_json(resp).await;
        assert_eq!(v["error"]["code"], -32002);
    }

    /// Deactivating must return `200` with an empty JSON object.
    #[tokio::test]
    async fn deactivate_returns_200_empty_object() {
        let (app, id, _dir) = app_and_id().await;
        post(&app, &format!("/workstreams/{id}/activate"), json!({})).await;

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/workstreams/{id}/deactivate"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v, json!({}));
    }
}
