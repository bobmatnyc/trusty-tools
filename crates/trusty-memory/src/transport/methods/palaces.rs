//! Daemon summary, config, one palace, and drawer CRUD (#6286).
//!
//! Why: these six were `/api/v1/status`, `/config`, `GET /palaces/{id}` and the
//! three drawer routes. `palace_list`, `palace_create`, `palace_update` and
//! `palace_delete` are NOT here — they were already tool methods the
//! dispatcher routes, so folding them again would be a second implementation of
//! the same call.
//! What: each handler delegates to `MemoryService`, which is where the
//! behaviour lived all along; the axum extractors become one params struct.
//! Test: `super::super::uds::tests` — `rpc_status_*`, `rpc_drawer_*`,
//! `rpc_palace_get_*`.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::transport::api_error::ApiError;
use crate::{ActivitySource, AppState};

use super::{to_value, CallerParams, NoParams, PalaceParams};

pub use crate::service::StatusPayload;

/// `memory.status` — daemon and palace summary.
///
/// The console header and external health tooling read this for the palace,
/// drawer and triple counts.
pub async fn status(state: &AppState, _params: NoParams) -> Result<Value, ApiError> {
    to_value(
        crate::service::MemoryService::new(state.clone())
            .status()
            .await,
    )
}

/// What `memory.config` reports.
///
/// The OpenRouter key itself is never included — only whether one is set.
#[derive(Debug, Serialize)]
pub struct ConfigPayload {
    /// Whether an OpenRouter key is configured.
    pub openrouter_configured: bool,
    /// The model that would be used.
    pub model: String,
    /// Where this daemon keeps its palaces.
    pub data_root: String,
}

/// `memory.config` — the current daemon configuration, minus the secret.
pub async fn config(state: &AppState, _params: NoParams) -> Result<Value, ApiError> {
    let cfg = crate::service::load_user_config().unwrap_or_default();
    to_value(ConfigPayload {
        openrouter_configured: !cfg.openrouter_api_key.is_empty(),
        model: cfg.openrouter_model,
        data_root: state.data_root.display().to_string(),
    })
}

/// `memory.palace_get` — one palace by id.
pub async fn get_palace(state: &AppState, params: PalaceParams) -> Result<Value, ApiError> {
    to_value(
        crate::service::MemoryService::new(state.clone())
            .get_palace(&params.palace_id)
            .await?,
    )
}

/// Params for `memory.drawers_list`.
///
/// Flattens the former `{id}` path segment and the tag/search/pagination query
/// string into one object.
#[derive(Debug, Deserialize)]
pub struct ListDrawersParams {
    /// Palace to list.
    pub palace_id: String,
    /// The filters `MemoryService::list_drawers` already understood.
    #[serde(flatten)]
    pub query: crate::service::ListDrawersQuery,
}

/// `memory.drawers_list` — drawers in one palace, filtered and paged.
pub async fn list_drawers(state: &AppState, params: ListDrawersParams) -> Result<Value, ApiError> {
    Ok(crate::service::MemoryService::new(state.clone())
        .list_drawers(&params.palace_id, params.query)
        .await?)
}

/// Params for `memory.drawer_create`.
#[derive(Debug, Deserialize)]
pub struct CreateDrawerParams {
    /// Palace to write into.
    pub palace_id: String,
    /// The drawer body, exactly as the REST route took it.
    #[serde(flatten)]
    pub body: crate::service::CreateDrawerBody,
    /// Who is asking — see [`CallerParams`] for why this is a field now.
    #[serde(flatten)]
    pub caller: CallerParams,
}

/// `memory.drawer_create` — write one drawer, attributed to its caller.
///
/// Test: `rpc_drawer_create_attributes_the_caller_it_was_given`.
pub async fn create_drawer(
    state: &AppState,
    params: CreateDrawerParams,
) -> Result<Value, ApiError> {
    let creator = params.caller.creator();
    let drawer_id = crate::service::MemoryService::new(state.clone())
        .create_drawer(
            &params.palace_id,
            params.body,
            creator,
            ActivitySource::Http,
        )
        .await?;
    Ok(json!({ "id": drawer_id }))
}

/// Params for `memory.drawer_delete`.
#[derive(Debug, Deserialize)]
pub struct DeleteDrawerParams {
    /// Palace holding the drawer.
    pub palace_id: String,
    /// Drawer to remove.
    pub drawer_id: String,
}

/// `memory.drawer_delete` — remove one drawer.
///
/// Answers `{"deleted": true}` rather than the former `204 No Content`: a
/// JSON-RPC success frame always carries a result, and a `null` would be
/// indistinguishable from a method that forgot to return one. A drawer id that
/// does not exist is still `-32004` (#5231).
pub async fn delete_drawer(
    state: &AppState,
    params: DeleteDrawerParams,
) -> Result<Value, ApiError> {
    crate::service::MemoryService::new(state.clone())
        .delete_drawer(&params.palace_id, &params.drawer_id, ActivitySource::Http)
        .await?;
    Ok(json!({ "deleted": true }))
}
