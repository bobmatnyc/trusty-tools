//! `GET`/`POST /workstreams*` REST routes over the full `workstream.*`
//! JSON-RPC surface (issues #3294, #3295, epic #3292).
//!
//! # Spec References
//!
//! - [`SPEC-WS-05~draft`](docs/specs/DOC-48-tcode-workstreams.md#SPEC-WS-05~draft)
//!
//! Why: mirrors every other resource group in this gateway (`super::sessions`,
//! `super::tasks`, …) — a REST-only client gets the same `workstream.*`
//! surface `POST /rpc` exposes, without duplicating
//! `crate::workstreams::protocol`'s business logic as bespoke axum handlers
//! (see `super` module docs' "forking the two surfaces" rationale). This is
//! where BOTH concurrent tickets against that RPC surface land their REST
//! twins: #3295's CRUD/inspection routes (`create`/`get`/`list`/`close`) and
//! #3294's activation-lock routes (`activate`/`deactivate`).
//!
//! **Path prefix deviation from DOC-48 §5.2 (documented, deliberate):** the
//! spec's table writes `/api/v1/workstreams`, but every REST resource group
//! this crate has shipped since #2983 (`/sessions`, `/tasks`, `/fs`, …) is
//! UNPREFIXED — there is no `/api/v1` anywhere in `crate::serve::rest`. today.
//! Introducing a prefix for exactly one resource group would make the API
//! surface internally inconsistent (a client would need to special-case
//! which resource lives under which prefix) for no behavioural gain; this
//! ticket follows the shipped convention instead of the spec's literal path,
//! matching every curl example in §12 except for the prefix itself.
//!
//! What: [`routes`] builds a standalone `axum::Router<()>` (its own
//! `WorkstreamsState` carrying just the `Arc<Router>`, `with_state`-erased
//! before return, mirroring `super::sessions::SessionsState`) mapping:
//!   - `POST /workstreams` -> `workstream.create` (`201 Created`)
//!   - `GET /workstreams/{id}` -> `workstream.get`
//!   - `GET /workstreams?include_closed=..` -> `workstream.list`
//!   - `POST /workstreams/{id}/close` -> `workstream.close`
//!   - `POST /workstreams/{id}/activate` -> `workstream.activate{id, force?}`
//!     (JSON body `{"force": bool}`, defaulting `force` to `false` when the
//!     body omits it or is `{}`)
//!   - `POST /workstreams/{id}/deactivate` -> `workstream.deactivate{id}`
//!     (no body, mirroring `super::sessions_write`'s `POST
//!     /sessions/{id}/cancel`)
//!   - `POST /workstreams/{id}/rename` -> `workstream.rename{id, name}`
//!     (JSON body `{"name": string}`, issue #3300, Phase C — DOC-48 §5.1
//!     marked this verb "future, Phase C" when Phase 1A shipped; this is
//!     that phase)
//!
//! `crate::serve::http::build_axum_router` merges this into the daemon's main
//! router alongside `POST /rpc`, `GET /health`, and the other REST resource
//! groups — none of those paths collide with the ones here.
//! Test: `tests::*`.

use std::sync::Arc;

use axum::Json;
use axum::Router as AxumRouter;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use serde::Deserialize;
use serde_json::{Value, json};
use trusty_common::mcp::Response;

use crate::jsonrpc::Router;

use super::{RestResult, respond};

/// Shared axum state for every route in this module: just the JSON-RPC
/// router, mirroring `super::sessions::SessionsState` — every handler here
/// goes through [`super::respond`]/[`respond_created`] rather than touching
/// `WorkstreamStore` directly.
#[derive(Clone)]
struct WorkstreamsState {
    router: Arc<Router>,
}

/// Build the `GET`/`POST /workstreams*` route group.
///
/// Why: kept separate from `crate::serve::http::build_axum_router` so this
/// resource group is unit-testable via `tower::util::ServiceExt::oneshot` on
/// its own, exactly like every other `rest::*` group.
/// What: six routes (see module docs), all sharing one
/// `WorkstreamsState { router }`, `with_state`-erased to `axum::Router<()>`
/// so the caller can `.merge()` it alongside the other resource groups.
/// Test: `tests::create_workstream_returns_201_with_id`,
/// `tests::activate_returns_200_with_active_id`.
pub fn routes(router: Arc<Router>) -> AxumRouter {
    AxumRouter::new()
        .route(
            "/workstreams",
            get(list_workstreams).post(create_workstream),
        )
        .route("/workstreams/{id}", get(get_workstream))
        .route("/workstreams/{id}/close", post(close_workstream))
        .route("/workstreams/{id}/activate", post(activate))
        .route("/workstreams/{id}/deactivate", post(deactivate))
        .route("/workstreams/{id}/rename", post(rename))
        .with_state(WorkstreamsState { router })
}

/// Request body for `POST /workstreams`, shaped like
/// `workstreams::protocol`'s private `CreateParams`.
#[derive(Deserialize, Default)]
struct CreateBody {
    #[serde(default)]
    name: Option<String>,
}

/// Query params for `GET /workstreams`, shaped like
/// `workstreams::protocol`'s private `ListParams`.
#[derive(Deserialize, Default)]
struct ListQuery {
    #[serde(default)]
    include_closed: bool,
}

/// Request body for `POST /workstreams/{id}/activate` — `id` comes from the
/// path, so only `force` remains. `#[serde(default)]` means `{}` (or a body
/// that just omits the field) is a valid, non-`force` request.
#[derive(Deserialize, Default)]
struct ActivateBody {
    #[serde(default)]
    force: bool,
}

/// Request body for `POST /workstreams/{id}/rename` — `id` comes from the
/// path, so only `name` remains.
#[derive(Deserialize)]
struct RenameBody {
    name: String,
}

/// Like [`super::respond`] but reports `201 Created` on success — the one
/// route in this module that mints a brand-new resource (mirrors
/// `sessions_write::respond_created`, `tasks::respond_accepted`).
async fn respond_created(
    router: &Router,
    method: &str,
    params: Value,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Response>)> {
    respond(router, method, params)
        .await
        .map(|Json(v)| (StatusCode::CREATED, Json(v)))
}

/// `POST /workstreams` -> `workstream.create`.
///
/// Why: the REST entry point for minting a new workstream in the daemon's
/// project — no `project_id` in the body, matching the RPC twin (§2.3: the
/// daemon's own `ProjectBinding` is implicit).
/// What: `201 Created` with `{"id": ..}` on success. An empty JSON body
/// (`{}`) is valid — `name` defaults to empty (§5.1); a syntactically
/// malformed or missing body is rejected by axum's `Json` extractor before
/// this handler runs, matching every other `POST` route in this gateway
/// (`sessions_write::create_session`, `tasks::run_task`).
/// Test: `tests::create_workstream_returns_201_with_id`,
/// `tests::create_workstream_empty_body_defaults_name`.
async fn create_workstream(
    State(state): State<WorkstreamsState>,
    Json(body): Json<CreateBody>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Response>)> {
    respond_created(
        &state.router,
        "workstream.create",
        json!({ "name": body.name }),
    )
    .await
}

/// `GET /workstreams/{id}` -> `workstream.get`.
///
/// Why: point lookup for one workstream's current (inferred) state.
/// What: `404` with a `not_found` envelope for an unknown `id`; otherwise the
/// `Workstream` JSON (id, name, computed `state`, `session_ids`, timestamps).
/// Test: `tests::get_workstream_found_returns_200`,
/// `tests::get_workstream_missing_returns_404`.
async fn get_workstream(
    State(state): State<WorkstreamsState>,
    Path(id): Path<String>,
) -> RestResult {
    respond(&state.router, "workstream.get", json!({"id": id})).await
}

/// `GET /workstreams?include_closed=..` -> `workstream.list`.
///
/// Why: enumerates every workstream in the daemon's project for a switcher
/// UI/CLI, without a `get` per id.
/// What: `200` with `{"active_workstream_id", "workstreams": [...]}`;
/// `include_closed` defaults to `false` (§4.4).
/// Test: `tests::list_workstreams_returns_active_id_and_records`.
async fn list_workstreams(
    State(state): State<WorkstreamsState>,
    Query(q): Query<ListQuery>,
) -> RestResult {
    respond(
        &state.router,
        "workstream.list",
        json!({"include_closed": q.include_closed}),
    )
    .await
}

/// `POST /workstreams/{id}/close` -> `workstream.close`.
///
/// Why: the REST entry point for irreversibly closing a workstream (§4.4);
/// no body needed, `id` from the path is the whole request.
/// What: `200` with `{}` on success; `404` for an unknown `id`.
/// Test: `tests::close_workstream_returns_200_empty_object`,
/// `tests::close_workstream_missing_returns_404`.
async fn close_workstream(
    State(state): State<WorkstreamsState>,
    Path(id): Path<String>,
) -> RestResult {
    respond(&state.router, "workstream.close", json!({"id": id})).await
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

/// `POST /workstreams/{id}/rename` -> `workstream.rename`.
///
/// Why: the REST entry point for the GUI switcher's rename action (issue
/// #3300, Phase C).
/// What: `200` with the updated `Workstream` JSON on success; `404` for an
/// unknown `id`.
/// Test: `tests::rename_returns_200_with_updated_name`,
/// `tests::rename_missing_returns_404`.
async fn rename(
    State(state): State<WorkstreamsState>,
    Path(id): Path<String>,
    Json(body): Json<RenameBody>,
) -> RestResult {
    respond(
        &state.router,
        "workstream.rename",
        json!({"id": id, "name": body.name}),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use tower::util::ServiceExt;

    /// Build the route group over a fresh, tempdir-backed workstream store
    /// with no workstreams yet — used by the `create`/`get`/`list`/`close`
    /// tests, which each create whatever records they need via `POST
    /// /workstreams`.
    async fn app() -> AxumRouter {
        let dir = tempfile::tempdir().expect("tempdir");
        let store =
            crate::workstreams::WorkstreamStore::load(dir.path().join("workstreams-test.json"))
                .await
                .expect("load");
        std::mem::forget(dir);
        let mut router = Router::new();
        crate::workstreams::protocol::register(
            &mut router,
            std::sync::Arc::new(tokio::sync::Mutex::new(store)),
        );
        routes(Arc::new(router))
    }

    /// Like [`app`] but pre-seeds one workstream directly through the store
    /// (bypassing the REST layer) and returns its id — used by the
    /// `activate`/`deactivate` tests, which need an existing workstream to
    /// act on. Also returns the backing `TempDir` so a test can reload the
    /// same file directly (e.g. to seed a SECOND workstream for a conflict
    /// scenario).
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

    async fn post(app: &AxumRouter, uri: &str, body: &str) -> axum::response::Response {
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

    #[tokio::test]
    async fn create_workstream_returns_201_with_id() {
        let app = app().await;
        let resp = post(&app, "/workstreams", r#"{"name": "Token rotation"}"#).await;
        assert_eq!(resp.status(), StatusCode::CREATED);
        let v = body_json(resp).await;
        assert!(v["id"].is_string());
    }

    #[tokio::test]
    async fn create_workstream_empty_body_defaults_name() {
        let app = app().await;
        let resp = post(&app, "/workstreams", "{}").await;
        assert_eq!(resp.status(), StatusCode::CREATED);
        let id = body_json(resp).await["id"].as_str().unwrap().to_string();

        let get_resp = get(&app, &format!("/workstreams/{id}")).await;
        let v = body_json(get_resp).await;
        assert_eq!(v["name"], "");
    }

    #[tokio::test]
    async fn get_workstream_found_returns_200() {
        let app = app().await;
        let created = body_json(post(&app, "/workstreams", r#"{"name": "A"}"#).await).await;
        let id = created["id"].as_str().unwrap();

        let resp = get(&app, &format!("/workstreams/{id}")).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["name"], "A");
        assert_eq!(v["state"], "idle");
    }

    #[tokio::test]
    async fn get_workstream_missing_returns_404() {
        let app = app().await;
        let resp = get(&app, "/workstreams/00000000-0000-0000-0000-000000000000").await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let v = body_json(resp).await;
        assert_eq!(v["error"]["code"], -32002);
    }

    #[tokio::test]
    async fn list_workstreams_returns_active_id_and_records() {
        let app = app().await;
        post(&app, "/workstreams", r#"{"name": "A"}"#).await;
        post(&app, "/workstreams", r#"{"name": "B"}"#).await;

        let resp = get(&app, "/workstreams").await;
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["active_workstream_id"], Value::Null);
        assert_eq!(v["workstreams"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn close_workstream_returns_200_empty_object() {
        let app = app().await;
        let created = body_json(post(&app, "/workstreams", r#"{"name": "A"}"#).await).await;
        let id = created["id"].as_str().unwrap();

        let resp = post(&app, &format!("/workstreams/{id}/close"), "").await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(body_json(resp).await, json!({}));

        let get_resp = get(&app, &format!("/workstreams/{id}")).await;
        assert_eq!(body_json(get_resp).await["state"], "closed");
    }

    #[tokio::test]
    async fn close_workstream_missing_returns_404() {
        let app = app().await;
        let resp = post(
            &app,
            "/workstreams/00000000-0000-0000-0000-000000000000/close",
            "",
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let v = body_json(resp).await;
        assert_eq!(v["error"]["code"], -32002);
    }

    /// `GET /workstreams?include_closed=..` must round-trip the query param
    /// through to `workstream.list` (proving the merge/extraction wiring,
    /// not just the RPC layer `protocol_tests` already cover).
    #[tokio::test]
    async fn list_include_closed_query_param_includes_closed_workstream() {
        let app = app().await;
        let created = body_json(post(&app, "/workstreams", r#"{"name": "A"}"#).await).await;
        let id = created["id"].as_str().unwrap();
        post(&app, &format!("/workstreams/{id}/close"), "").await;

        let default_resp = get(&app, "/workstreams").await;
        assert!(
            body_json(default_resp).await["workstreams"]
                .as_array()
                .unwrap()
                .is_empty(),
            "closed workstream must be excluded by default"
        );

        let included_resp = get(&app, "/workstreams?include_closed=true").await;
        let v = body_json(included_resp).await;
        assert_eq!(v["workstreams"].as_array().unwrap().len(), 1);
    }

    /// Activating with no prior active workstream must return `200` with
    /// the new `active_id` and a null `prior_id`.
    #[tokio::test]
    async fn activate_returns_200_with_active_id() {
        let (app, id, _dir) = app_and_id().await;

        let resp = post(&app, &format!("/workstreams/{id}/activate"), "{}").await;
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

        post(&app, &format!("/workstreams/{id}/activate"), "{}").await;

        let resp = post(
            &app,
            &format!("/workstreams/{other}/activate"),
            r#"{"force": false}"#,
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
            "{}",
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
        post(&app, &format!("/workstreams/{id}/activate"), "{}").await;

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

    /// Renaming must return `200` with the updated `name` (issue #3300).
    #[tokio::test]
    async fn rename_returns_200_with_updated_name() {
        let (app, id, _dir) = app_and_id().await;

        let resp = post(
            &app,
            &format!("/workstreams/{id}/rename"),
            r#"{"name": "renamed"}"#,
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["name"], "renamed");
    }

    /// Renaming an unknown id must return a real HTTP `404`.
    #[tokio::test]
    async fn rename_missing_returns_404() {
        let app = app().await;

        let resp = post(
            &app,
            "/workstreams/00000000-0000-0000-0000-000000000000/rename",
            r#"{"name": "renamed"}"#,
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let v = body_json(resp).await;
        assert_eq!(v["error"]["code"], -32002);
    }
}
