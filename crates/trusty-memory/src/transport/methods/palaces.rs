//! Daemon summary, config, the palace roster, one palace, and drawer CRUD
//! (#6286).
//!
//! Why: these were `/api/v1/status`, `/config`, `GET /palaces`,
//! `GET /palaces/{id}` and the three drawer routes. `palace_create`,
//! `palace_update` and `palace_delete` are NOT here — they were already tool
//! methods the dispatcher routes, so folding them again would be a second
//! implementation of the same call.
//!
//! [`palaces_list`] is the one that is not a straight fold. `palace_list`, the
//! tool, answers bare ids; `GET /palaces` answered rows but with `peek`-based
//! placeholder zeros for any palace not already resident (#4640). Neither is
//! what a roster with counts needs, which is why the monitor was fanning out one
//! [`get_palace`] per id — see [`palaces_list`] and [`PalaceListRow`].
//!
//! What: each handler delegates to `MemoryService`, which is where the
//! behaviour lived all along; the axum extractors become one params struct.
//! Test: `super::super::uds::tests` — `rpc_status_*`, `rpc_drawer_*`,
//! `rpc_palace_get_*`, `rpc_palaces_list_*`.

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

/// One palace's row in [`palaces_list`], or why its counts are missing.
///
/// Why the error is a FIELD rather than an omitted row (#6286): the monitor's
/// predecessor — an N-call `memory.palace_get` fan-out — dropped a palace whose
/// call failed at `debug!`, so the panel could report "12 palaces" above 9 rows
/// with nothing to say which three were missing or why. A row that can carry
/// its own failure makes that impossible to reintroduce: every palace the
/// registry lists appears, and one that could not be read says so.
///
/// What: `id` is always present. Exactly one of `palace` and `error` is
/// non-null. `error` is serialised even when null so a consumer reads it
/// directly rather than testing for the key.
///
/// `palace` is nested rather than flattened because [`crate::service::PalaceInfo`]
/// carries its own `id`, and a flatten would put two spellings of the same
/// field in one object.
/// Test: `rpc_palaces_list_reports_counts_per_palace`,
/// `rpc_palaces_list_reports_an_unreadable_palace_rather_than_dropping_it`.
#[derive(Debug, Serialize)]
pub struct PalaceListRow {
    /// The palace id, present whether or not its counts could be read.
    pub id: String,
    /// The palace's real counts. Never a row of zeros standing in for unknown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub palace: Option<crate::service::PalaceInfo>,
    /// Why the counts are absent. `null` when `palace` is present.
    pub error: Option<String>,
}

/// `memory.palaces_list` — one row per palace, with real counts (#6286).
///
/// Why: this is the contract the retired `GET /api/v1/palaces` carried, with
/// the counts it could not. That route used `PalaceRegistry::peek` since #4640,
/// so it reported `cached: false` and placeholder zeros for any palace not
/// already resident — which is why the monitor fanned out one
/// `memory.palace_get` per id instead, and why the panel could disagree with
/// itself about how many palaces there are.
///
/// What: delegates to `MemoryService::list_palaces_with_counts`, which opens
/// each palace and keeps per-palace failures. See that method for why opening
/// every palace is the same cost as the fan-out it replaces, not a new one.
/// Answers `{"palaces": [PalaceListRow, …]}` — an object rather than a bare
/// array so a later addition (a total, a truncation flag) is not a breaking
/// shape change.
///
/// Test: `rpc_palaces_list_reports_counts_per_palace`,
/// `rpc_palaces_list_reports_an_unreadable_palace_rather_than_dropping_it`.
pub async fn palaces_list(state: &AppState, _params: NoParams) -> Result<Value, ApiError> {
    let rows = crate::service::MemoryService::new(state.clone())
        .list_palaces_with_counts()
        .await?;
    let rows: Vec<PalaceListRow> = rows
        .into_iter()
        .map(|(id, result)| match result {
            Ok(info) => PalaceListRow {
                id,
                palace: Some(info),
                error: None,
            },
            Err(error) => PalaceListRow {
                id,
                palace: None,
                error: Some(error),
            },
        })
        .collect();
    to_value(json!({ "palaces": rows }))
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
