//! Claude Code configuration analyzer HTTP routes.
//!
//! Why: the daemon's Claude Code config endpoints — analyze, apply a
//! recommendation, checkpoint / restore / delete, list and deploy profiles,
//! restart — form a cohesive cluster that, kept inline in `api.rs`, dominated
//! the file. Splitting them into their own route module keeps `api.rs` focused
//! on the core session / hook / tmux surface.
//! What: the `#[utoipa::path]`-annotated handlers for the `/claude-config/*`
//! endpoints, plus their request/query structs. They are wired into the router
//! by `api::router` and registered in the OpenAPI document by `openapi.rs`.
//! Test: `cargo test -p trusty-mpm-daemon` drives these via the `api_tests`
//! module, which references them through `crate::daemon::api::claude_config_routes::*`.

use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};

use crate::daemon::api::types::{
    ApplyConfigResponse, CheckpointsResponse, ClaudeConfigResponse, CreateCheckpointResponse,
    DeleteCheckpointResponse, DeployProfileResponse, ProfilesResponse, RestartResponse,
    RestoreResponse,
};
use crate::daemon::state::DaemonState;

// ---- Claude Code configuration analyzer ---------------------------------

/// Query parameters for `GET /claude-config`.
///
/// Why: the analyzer inspects the config for a specific project directory.
/// What: the absolute project path to analyze.
/// Test: `get_claude_config_returns_recommendations`.
#[derive(serde::Deserialize)]
pub struct ClaudeConfigQuery {
    /// Project directory whose Claude Code config to analyze.
    pub project: PathBuf,
}

/// `GET /claude-config?project=<path>` — analyze Claude Code config.
///
/// Why: trusty-mpm can recommend config changes (hooks, permission scoping,
/// agent deployment) for a project's Claude Code setup.
/// What: resolves the user- and project-level config paths, reads and merges
/// them, and returns `{ config, recommendations }`.
/// Test: `get_claude_config_returns_recommendations`.
#[utoipa::path(
    get,
    path = "/claude-config",
    tag = "claude-config",
    params(("project" = String, Query, description = "Project directory")),
    responses((status = 200, description = "Analyzed config plus recommendations"))
)]
pub async fn get_claude_config(
    State(_state): State<Arc<DaemonState>>,
    Query(query): Query<ClaudeConfigQuery>,
) -> Json<ClaudeConfigResponse> {
    use crate::core::claude_config::ClaudeConfigReader;
    let paths = ClaudeConfigReader::paths_for_project(&query.project);
    let config = crate::daemon::claude_config::ClaudeConfigAnalyzer::read_config(&paths);
    let recommendations = crate::daemon::claude_config::ClaudeConfigAnalyzer::analyze(&config);
    Json(ClaudeConfigResponse {
        config,
        recommendations,
    })
}

/// JSON body for `POST /claude-config/apply`.
///
/// Why: applying a recommendation needs the project path and the rec id.
/// What: the project directory and the recommendation id to apply.
/// Test: `apply_claude_config_unknown_rec_is_404`.
#[derive(serde::Deserialize, utoipa::ToSchema)]
pub struct ApplyConfigRequest {
    /// Project directory the recommendation applies to.
    #[schema(value_type = String)]
    pub project: PathBuf,
    /// Id of the recommendation to apply.
    pub recommendation_id: String,
}

/// `POST /claude-config/apply` — apply a Claude Code config recommendation.
///
/// Why: lets an operator act on a recommendation without hand-editing JSON.
/// What: re-analyzes the project, finds the recommendation by id, and applies
/// it via `ClaudeConfigAnalyzer::apply_recommendation`, which checkpoints the
/// config first. Returns `{ applied: true, checkpoint_id }` so the caller can
/// undo. An unknown id is `404`.
/// Test: `apply_claude_config_unknown_rec_is_404`.
#[utoipa::path(
    post,
    path = "/claude-config/apply",
    tag = "claude-config",
    request_body = ApplyConfigRequest,
    responses(
        (status = 200, description = "Recommendation applied; returns checkpoint id"),
        (status = 404, description = "No recommendation with that id"),
        (status = 500, description = "Applying the recommendation failed"),
    )
)]
pub async fn apply_claude_config(
    State(_state): State<Arc<DaemonState>>,
    Json(body): Json<ApplyConfigRequest>,
) -> Result<Json<ApplyConfigResponse>, StatusCode> {
    use crate::core::claude_config::ClaudeConfigReader;
    let paths = ClaudeConfigReader::paths_for_project(&body.project);
    let config = crate::daemon::claude_config::ClaudeConfigAnalyzer::read_config(&paths);
    let recommendations = crate::daemon::claude_config::ClaudeConfigAnalyzer::analyze(&config);
    let rec = recommendations
        .iter()
        .find(|r| r.id == body.recommendation_id)
        .ok_or(StatusCode::NOT_FOUND)?;
    let checkpoint_id = crate::daemon::claude_config::ClaudeConfigAnalyzer::apply_recommendation(
        rec,
        &paths,
        &body.project,
    )
    .map_err(|e| {
        tracing::warn!("applying recommendation {} failed: {e}", rec.id);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    Ok(Json(ApplyConfigResponse {
        applied: true,
        recommendation_id: body.recommendation_id,
        checkpoint_id,
    }))
}

// ---- checkpoints & deployment profiles ----------------------------------

/// Query parameters for the checkpoint list / delete endpoints.
///
/// Why: checkpoints are project-scoped; the project path identifies which
/// `.trusty-mpm/checkpoints` directory to operate on.
/// What: the project directory.
/// Test: `list_checkpoints_returns_array`.
#[derive(serde::Deserialize)]
pub struct CheckpointQuery {
    /// Project directory whose checkpoints to operate on.
    pub project: PathBuf,
}

/// `GET /claude-config/checkpoints?project=<path>` — list config checkpoints.
///
/// Why: the dashboard offers a restore picker; this feeds it.
/// What: returns `{ checkpoints: [ConfigCheckpoint, ...] }`, newest first.
/// Test: `list_checkpoints_returns_array`.
#[utoipa::path(
    get,
    path = "/claude-config/checkpoints",
    tag = "claude-config",
    params(("project" = String, Query, description = "Project directory")),
    responses((status = 200, description = "Config checkpoints, newest first"))
)]
pub async fn list_checkpoints(
    State(_state): State<Arc<DaemonState>>,
    Query(query): Query<CheckpointQuery>,
) -> Json<CheckpointsResponse> {
    let checkpoints = crate::daemon::claude_config::ConfigCheckpointer::list(&query.project)
        .unwrap_or_else(|e| {
            tracing::warn!("listing checkpoints failed: {e}");
            Vec::new()
        });
    Json(CheckpointsResponse { checkpoints })
}

/// JSON body for `POST /claude-config/checkpoints`.
///
/// Why: creating a checkpoint needs the project and an optional human label.
/// What: the project directory and an optional label.
/// Test: `create_checkpoint_returns_id`.
#[derive(serde::Deserialize, utoipa::ToSchema)]
pub struct CreateCheckpointRequest {
    /// Project directory to checkpoint.
    #[schema(value_type = String)]
    pub project: PathBuf,
    /// Optional human-readable label for the checkpoint.
    #[serde(default)]
    pub label: Option<String>,
}

/// `POST /claude-config/checkpoints` — create a config checkpoint.
///
/// Why: lets the operator take a manual backup before a risky change.
/// What: snapshots the project's config and returns `{ id }`.
/// Test: `create_checkpoint_returns_id`.
#[utoipa::path(
    post,
    path = "/claude-config/checkpoints",
    tag = "claude-config",
    request_body = CreateCheckpointRequest,
    responses(
        (status = 200, description = "Checkpoint created; returns its id"),
        (status = 500, description = "Creating the checkpoint failed"),
    )
)]
pub async fn create_checkpoint(
    State(_state): State<Arc<DaemonState>>,
    Json(body): Json<CreateCheckpointRequest>,
) -> Result<Json<CreateCheckpointResponse>, StatusCode> {
    use crate::core::claude_config::ClaudeConfigReader;
    let paths = ClaudeConfigReader::paths_for_project(&body.project);
    let id = crate::daemon::claude_config::ConfigCheckpointer::create(
        &paths,
        &body.project,
        body.label.as_deref(),
    )
    .map_err(|e| {
        tracing::warn!("creating checkpoint failed: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    Ok(Json(CreateCheckpointResponse { id }))
}

/// JSON body for `POST /claude-config/restore`.
///
/// Why: restoring needs the project and the checkpoint id to revert to.
/// What: the project directory and the checkpoint id.
/// Test: `restore_unknown_checkpoint_is_500`.
#[derive(serde::Deserialize, utoipa::ToSchema)]
pub struct RestoreRequest {
    /// Project directory whose config to restore.
    #[schema(value_type = String)]
    pub project: PathBuf,
    /// Id of the checkpoint to restore.
    pub checkpoint_id: String,
}

/// `POST /claude-config/restore` — restore config from a checkpoint.
///
/// Why: the undo half of the safety model.
/// What: rewrites the project's config files to the checkpoint's state. A
/// missing or malformed checkpoint surfaces as `500`.
/// Test: `restore_unknown_checkpoint_is_500`.
#[utoipa::path(
    post,
    path = "/claude-config/restore",
    tag = "claude-config",
    request_body = RestoreRequest,
    responses(
        (status = 200, description = "Config restored from the checkpoint"),
        (status = 500, description = "Checkpoint missing or restore failed"),
    )
)]
pub async fn restore_checkpoint(
    State(_state): State<Arc<DaemonState>>,
    Json(body): Json<RestoreRequest>,
) -> Result<Json<RestoreResponse>, StatusCode> {
    crate::daemon::claude_config::ConfigCheckpointer::restore(&body.project, &body.checkpoint_id)
        .map_err(|e| {
        tracing::warn!("restoring checkpoint {} failed: {e}", body.checkpoint_id);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    Ok(Json(RestoreResponse {
        restored: true,
        checkpoint_id: body.checkpoint_id,
    }))
}

/// `DELETE /claude-config/checkpoints/{id}?project=<path>` — delete a checkpoint.
///
/// Why: checkpoints accumulate; the operator prunes them here.
/// What: removes the checkpoint file. A missing checkpoint surfaces as `404`.
/// Test: `delete_unknown_checkpoint_is_404`.
#[utoipa::path(
    delete,
    path = "/claude-config/checkpoints/{id}",
    tag = "claude-config",
    params(
        ("id" = String, Path, description = "Checkpoint id"),
        ("project" = String, Query, description = "Project directory"),
    ),
    responses(
        (status = 200, description = "Checkpoint deleted"),
        (status = 404, description = "No checkpoint with that id"),
    )
)]
pub async fn delete_checkpoint(
    State(_state): State<Arc<DaemonState>>,
    Path(id): Path<String>,
    Query(query): Query<CheckpointQuery>,
) -> Result<Json<DeleteCheckpointResponse>, StatusCode> {
    crate::daemon::claude_config::ConfigCheckpointer::delete(&query.project, &id).map_err(|e| {
        tracing::warn!("deleting checkpoint {id} failed: {e}");
        StatusCode::NOT_FOUND
    })?;
    Ok(Json(DeleteCheckpointResponse { deleted: id }))
}

/// `GET /claude-config/profiles` — list the built-in deployment profiles.
///
/// Why: the dashboard shows the available configuration presets.
/// What: returns `{ profiles: [DeploymentProfile, ...] }`.
/// Test: `list_profiles_returns_builtins`.
#[utoipa::path(
    get,
    path = "/claude-config/profiles",
    tag = "claude-config",
    responses((status = 200, description = "Built-in deployment profiles"))
)]
pub async fn list_profiles(State(_state): State<Arc<DaemonState>>) -> Json<ProfilesResponse> {
    let profiles = crate::daemon::claude_config::ProfileDeployer::builtin_profiles();
    Json(ProfilesResponse { profiles })
}

/// JSON body for `POST /claude-config/deploy`.
///
/// Why: deploying a profile needs the project, the profile name, and an
/// optional target override.
/// What: the project directory, the profile name, and an optional deploy
/// target (`user`, `project`, `both`) overriding the profile's default.
/// Test: `deploy_profile_returns_checkpoint_id`.
#[derive(serde::Deserialize, utoipa::ToSchema)]
pub struct DeployProfileRequest {
    /// Project directory to deploy the profile onto.
    #[schema(value_type = String)]
    pub project: PathBuf,
    /// Name of the built-in profile to deploy.
    pub profile_name: String,
    /// Optional deploy-target override (`user`, `project`, `both`).
    #[serde(default)]
    pub target: Option<crate::core::claude_config::DeployTarget>,
}

/// `POST /claude-config/deploy` — deploy a built-in profile onto a project.
///
/// Why: lets the operator apply a configuration preset in one click; the deploy
/// checkpoints the config first so it is reversible.
/// What: looks up the named built-in profile (applying an optional `target`
/// override), deploys it, and returns `{ checkpoint_id }`. An unknown profile
/// name is `404`.
/// Test: `deploy_profile_returns_checkpoint_id`, `deploy_unknown_profile_is_404`.
#[utoipa::path(
    post,
    path = "/claude-config/deploy",
    tag = "claude-config",
    request_body = DeployProfileRequest,
    responses(
        (status = 200, description = "Profile deployed; returns checkpoint id"),
        (status = 404, description = "No built-in profile with that name"),
        (status = 500, description = "Deploying the profile failed"),
    )
)]
pub async fn deploy_profile(
    State(_state): State<Arc<DaemonState>>,
    Json(body): Json<DeployProfileRequest>,
) -> Result<Json<DeployProfileResponse>, StatusCode> {
    use crate::core::claude_config::ClaudeConfigReader;
    let mut profile = crate::daemon::claude_config::ProfileDeployer::builtin_profiles()
        .into_iter()
        .find(|p| p.name == body.profile_name)
        .ok_or(StatusCode::NOT_FOUND)?;
    if let Some(target) = body.target {
        profile.target = target;
    }
    let paths = ClaudeConfigReader::paths_for_project(&body.project);
    let checkpoint_id =
        crate::daemon::claude_config::ProfileDeployer::deploy(&profile, &paths, &body.project)
            .map_err(|e| {
                tracing::warn!("deploying profile {} failed: {e}", body.profile_name);
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
    Ok(Json(DeployProfileResponse {
        deployed: body.profile_name,
        checkpoint_id,
    }))
}

/// JSON body for `POST /claude-config/restart`.
///
/// Why: restarting Claude Code happens inside a named tmux session.
/// What: the tmux session in which to restart `claude`.
/// Test: `restart_claude_code_handles_missing_tmux`.
#[derive(serde::Deserialize, utoipa::ToSchema)]
pub struct RestartRequest {
    /// tmux session in which to restart Claude Code.
    pub tmux_session: String,
}

/// `POST /claude-config/restart` — restart Claude Code in a tmux session.
///
/// Why: after applying config changes the operator wants a clean Claude Code
/// process; this sends Ctrl-C then `claude` into the session's pane.
/// What: calls `ClaudeCodeRestarter::restart_in_session`. tmux being absent
/// surfaces as `500`.
/// Test: `restart_claude_code_handles_missing_tmux`.
#[utoipa::path(
    post,
    path = "/claude-config/restart",
    tag = "claude-config",
    request_body = RestartRequest,
    responses(
        (status = 200, description = "Restart command sent"),
        (status = 500, description = "tmux unavailable or restart failed"),
    )
)]
pub async fn restart_claude_code(
    State(_state): State<Arc<DaemonState>>,
    Json(body): Json<RestartRequest>,
) -> Result<Json<RestartResponse>, StatusCode> {
    crate::daemon::claude_config::ClaudeCodeRestarter::restart_in_session(&body.tmux_session)
        .map_err(|e| {
            tracing::warn!("restart in {} failed: {e}", body.tmux_session);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    Ok(Json(RestartResponse {
        restarted: body.tmux_session,
    }))
}
