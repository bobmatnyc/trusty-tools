//! Palace and drawer CRUD handlers, plus status and config endpoints.
//!
//! Why: Palace and drawer operations form the core data-plane REST API. The
//! status and config endpoints are small and co-located here because they
//! read the same service layer.
//! What: `GET /api/v1/status`, `GET /api/v1/config`, the full palace CRUD
//! (`/api/v1/palaces` + `/{id}` variants), and drawer CRUD under
//! `/api/v1/palaces/{id}/drawers` (with the `/memories` alias).
//! Test: `create_then_list_palace`, `delete_palace_*`,
//! `update_palace_name_*`, `status_endpoint_returns_payload`,
//! `memories_alias_routes_to_drawers` in `web::tests`.

use axum::{
    extract::{Path as AxumPath, Query, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{ActivitySource, AppState};

use super::error::ApiError;
use super::rpc::creator_info_from_http;

pub(crate) use crate::service::StatusPayload;

/// `GET /api/v1/status` — daemon and palace summary.
///
/// Why: The admin UI header and external health tooling need a quick summary
/// of how many palaces, drawers, and KG triples exist.
/// What: delegates to `MemoryService::status()`.
/// Test: `status_endpoint_returns_payload`.
pub(super) async fn status(State(state): State<AppState>) -> Json<StatusPayload> {
    Json(crate::service::MemoryService::new(state).status().await)
}

/// Wire shape for `GET /api/v1/config`.
///
/// Why: The admin UI settings panel needs to show whether an OpenRouter key
/// is configured and which model is active, without exposing the key itself.
/// What: `openrouter_configured` bool + `model` string + `data_root` path.
/// Test: Indirectly through integration; shape is stable.
#[derive(Serialize)]
pub(super) struct ConfigPayload {
    openrouter_configured: bool,
    model: String,
    data_root: String,
}

/// `GET /api/v1/config` — serialise the current daemon configuration.
///
/// Why: The admin UI settings panel reads this to pre-fill the configuration
/// form on load.
/// What: Loads the user config file, maps relevant fields to the response
/// struct, and returns JSON.
/// Test: Indirectly covered by integration; no dedicated unit test at this time.
pub(super) async fn config(State(state): State<AppState>) -> Json<ConfigPayload> {
    let cfg = crate::service::load_user_config().unwrap_or_default();
    Json(ConfigPayload {
        openrouter_configured: !cfg.openrouter_api_key.is_empty(),
        model: cfg.openrouter_model,
        data_root: state.data_root.display().to_string(),
    })
}

pub(crate) use crate::service::load_user_config;
#[allow(unused_imports)]
pub(crate) use crate::service::LoadedUserConfig;

pub(crate) use crate::service::{palace_info_from, CreatePalaceBody, PalaceInfo};

/// `GET /api/v1/palaces` — list user-visible palaces.
///
/// Why: The admin UI palace sidebar and the MCP tool both need a structured
/// list. Using the service layer keeps hidden palaces (`__`-prefixed) out of
/// the response automatically.
/// What: Delegates to `MemoryService::list_palaces`.
/// Test: `create_then_list_palace`.
pub(super) async fn list_palaces(
    State(state): State<AppState>,
) -> Result<Json<Vec<PalaceInfo>>, ApiError> {
    Ok(Json(
        crate::service::MemoryService::new(state)
            .list_palaces()
            .await?,
    ))
}

/// `POST /api/v1/palaces` — create a new palace.
///
/// Why: The admin UI "New Palace" form and external tooling need an HTTP
/// endpoint to provision a palace directory without using MCP.
/// What: Delegates to `MemoryService::create_palace`; returns `{"id": "<id>"}`.
/// Test: `create_then_list_palace`.
pub(super) async fn create_palace(
    State(state): State<AppState>,
    Json(body): Json<CreatePalaceBody>,
) -> Result<Json<Value>, ApiError> {
    let id = crate::service::MemoryService::new(state)
        .create_palace(body, ActivitySource::Http)
        .await?;
    Ok(Json(json!({ "id": id })))
}

/// `GET /api/v1/palaces/{id}` — fetch a single palace by id.
///
/// Why: The admin UI detail view and tooling need a way to fetch a single
/// palace without listing all palaces.
/// What: Delegates to `MemoryService::get_palace`; returns the `PalaceInfo`
/// struct as JSON.
/// Test: Covered implicitly by `create_then_list_palace`.
pub(super) async fn get_palace_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<PalaceInfo>, ApiError> {
    Ok(Json(
        crate::service::MemoryService::new(state)
            .get_palace(&id)
            .await?,
    ))
}

/// Query parameters for `DELETE /api/v1/palaces/{id}`.
///
/// Why: Issue #180 — `force=true` is the explicit opt-in to delete a
/// palace that still has drawers. Defaulting to `false` keeps the
/// "must be empty" guard active when callers omit the flag.
/// What: a single optional bool that the handler unwraps to `false`.
/// Test: `delete_palace_refuses_when_drawers_present`,
/// `delete_palace_force_removes_populated_palace`.
#[derive(Deserialize, Default)]
pub(super) struct DeletePalaceQuery {
    #[serde(default)]
    force: Option<bool>,
}

/// `DELETE /api/v1/palaces/{id}?force=<bool>` — drop an entire palace.
///
/// Why: Issue #180 — operators need a single call to clean up a palace
/// they no longer want. The legacy drawer-by-drawer delete path is too
/// noisy and leaves the palace's KG / vector index behind.
/// What: delegates to `MemoryService::delete_palace`. Returns
/// `204 No Content` on success, `404 Not Found` when the id is unknown,
/// and `409 Conflict` when the palace still has drawers and `force` is
/// not set. Other failures bubble up as 500.
/// Test: `delete_palace_removes_dir_when_empty`,
/// `delete_palace_refuses_when_drawers_present`,
/// `delete_palace_force_removes_populated_palace`,
/// `delete_palace_returns_not_found_for_missing_id`.
pub(super) async fn delete_palace_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Query(q): Query<DeletePalaceQuery>,
) -> Result<StatusCode, ApiError> {
    crate::service::MemoryService::new(state)
        .delete_palace(&id, q.force.unwrap_or(false))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Request body for `PATCH /api/v1/palaces/{id}`.
///
/// Why: The only mutable palace metadata exposed today is the display name;
/// keeping the body to a single field keeps the wire contract obvious and
/// lets us extend later without breaking older clients (additive fields
/// only). Issue #180 follow-up.
/// What: a single required `name` string. Empty / whitespace-only values
/// are rejected with 400 by the handler.
/// Test: `update_palace_name_renames_palace`,
/// `update_palace_name_rejects_empty_name`.
#[derive(Deserialize)]
pub(super) struct UpdatePalaceBody {
    name: String,
}

/// `PATCH /api/v1/palaces/{id}` — rename a palace's display name.
///
/// Why: Issue #180 follow-up — operators need to relabel palaces without
/// re-creating them (which would lose all stored drawers / KG / vectors).
/// Only the human-readable `name` changes; the directory name (which is the
/// palace id) is immutable.
/// What: delegates to `MemoryService::update_palace_name_typed`. Returns
/// `200 OK` with the updated palace info on success, `404 Not Found` when
/// the id is unknown, and `400 Bad Request` when the supplied name is
/// empty after trimming.
/// Test: `update_palace_name_renames_palace`,
/// `update_palace_name_rejects_empty_name`,
/// `update_palace_name_returns_not_found_for_missing_id`.
pub(super) async fn update_palace_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<UpdatePalaceBody>,
) -> Result<Json<Value>, ApiError> {
    let value = crate::service::MemoryService::new(state)
        .update_palace_name_typed(&id, &body.name)
        .await?;
    Ok(Json(value))
}

// ---------------------------------------------------------------------------
// Drawers
// ---------------------------------------------------------------------------

pub(crate) use crate::service::{CreateDrawerBody, ListDrawersQuery};

/// `GET /api/v1/palaces/{id}/drawers` — list drawers in a palace.
///
/// Why: The admin UI drawer panel and external tooling need a structured
/// list with optional filters (tags, search, pagination).
/// What: Delegates to `MemoryService::list_drawers`.
/// Test: `create_then_list_palace`, `memories_alias_routes_to_drawers`.
pub(super) async fn list_drawers(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Query(q): Query<ListDrawersQuery>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        crate::service::MemoryService::new(state)
            .list_drawers(&id, q)
            .await?,
    ))
}

/// `POST /api/v1/palaces/{id}/drawers` — create a drawer in a palace.
///
/// Why: The admin UI "Add Memory" form and the `trusty-memory note` CLI
/// write via this path when callers want HTTP rather than MCP.
/// What: Delegates to `MemoryService::create_drawer`; extracts creator
/// attribution from request headers and returns `{"id": "<uuid>"}`.
/// Test: `http_create_drawer_runs_auto_kg_extraction`.
pub(super) async fn create_drawer(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
    Json(body): Json<CreateDrawerBody>,
) -> Result<Json<Value>, ApiError> {
    let creator = creator_info_from_http(&headers);
    let drawer_id = crate::service::MemoryService::new(state)
        .create_drawer(&id, body, creator, ActivitySource::Http)
        .await?;
    Ok(Json(json!({ "id": drawer_id })))
}

/// `DELETE /api/v1/palaces/{id}/drawers/{drawer_id}` — delete a drawer.
///
/// Why: Provides the HTTP counterpart to the MCP forget tool.
/// What: Delegates to `MemoryService::delete_drawer`; returns `204 No Content`.
/// Test: `delete_palace_force_removes_populated_palace` uses this indirectly.
pub(super) async fn delete_drawer(
    State(state): State<AppState>,
    AxumPath((id, drawer_id)): AxumPath<(String, String)>,
) -> Result<StatusCode, ApiError> {
    crate::service::MemoryService::new(state)
        .delete_drawer(&id, &drawer_id, ActivitySource::Http)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
