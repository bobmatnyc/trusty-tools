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
use crate::session_manager::record::{ManagedSessionState, SessionRecord};

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

/// Select the `pane_id` to restart into from the full managed-session record
/// set, given the target tmux session name (#2514 review, minor finding 4).
///
/// Why: `tmux_name` is recycled once a session is decommissioned — a fresh
/// session provisioned later can be assigned the SAME tmux name a
/// decommissioned record used. A naive `.find(|r| r.tmux_name == name)` over
/// the full record list can therefore match the stale, decommissioned
/// tombstone instead of the live record, silently threading a dead/garbage
/// `pane_id` (or a `None` that masks a real one) into the restart call.
/// What: filters to records matching `tmux_session` whose state is NOT
/// `Decommissioned` (the terminal, unresumable state — see
/// [`ManagedSessionState`]'s doc), then — since more than one non-terminal
/// record can theoretically share a recycled name during a narrow
/// provisioning race — prefers the most recently created match. Returns
/// `None` when no live record matches (unmanaged/legacy session name) or the
/// surviving record never captured a `pane_id`; the session-scoped restart
/// fallback then applies exactly as before #2468.
/// Test: `select_restart_pane_id_skips_decommissioned_record`,
/// `select_restart_pane_id_prefers_most_recent_when_multiple_live_match`,
/// `select_restart_pane_id_none_when_no_match`.
fn select_restart_pane_id(records: &[SessionRecord], tmux_session: &str) -> Option<String> {
    records
        .iter()
        .filter(|r| r.tmux_name == tmux_session)
        .filter(|r| !matches!(r.state, ManagedSessionState::Decommissioned))
        .max_by_key(|r| r.created_at)
        .and_then(|r| r.pane_id.clone())
}

/// `POST /claude-config/restart` — restart Claude Code in a tmux session.
///
/// Why: after applying config changes the operator wants a clean Claude Code
/// process; this sends Ctrl-C then `claude` into the session's pane. The
/// managed-session record's `pane_id`, when one is tracked for this tmux
/// session, is threaded through so the restart lands in the record's OWN
/// pane rather than tmux's session-scoped "active pane" resolution — the
/// same sibling-window hijack risk #2467 fixed for resume/restart respawn
/// (issue #2468).
/// What: looks up `body.tmux_session` against the managed-session store via
/// [`select_restart_pane_id`], which excludes decommissioned (tombstone)
/// records and prefers the most recently created live match — a recycled
/// `tmux_name` can otherwise resolve to a stale decommissioned record (#2514
/// review). `None` (unmanaged/legacy session, or no matching live record)
/// falls back to the session-scoped restart exactly as before #2468. Then
/// calls `ClaudeCodeRestarter::restart_in_session`. tmux being absent, or a
/// confirmed-gone recorded pane, both surface as `500`.
/// Test: `restart_claude_code_handles_missing_tmux`;
/// `ClaudeCodeRestarter::restart_target`'s pane/session decision is
/// unit-tested directly in `daemon::claude_config::restarter`;
/// [`select_restart_pane_id`]'s own unit tests cover the stale-record fix.
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
    State(state): State<Arc<DaemonState>>,
    Json(body): Json<RestartRequest>,
) -> Result<Json<RestartResponse>, StatusCode> {
    let records = state.session_manager().await.list().await;
    let pane_id = select_restart_pane_id(&records, &body.tmux_session);
    crate::daemon::claude_config::ClaudeCodeRestarter::restart_in_session(
        &body.tmux_session,
        pane_id.as_deref(),
    )
    .map_err(|e| {
        tracing::warn!("restart in {} failed: {e}", body.tmux_session);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    Ok(Json(RestartResponse {
        restarted: body.tmux_session,
    }))
}

#[cfg(test)]
mod restart_pane_selection_tests {
    use super::*;
    use crate::session_manager::record::ManagedSessionId;

    /// Builds a minimal, otherwise-default [`SessionRecord`] for the pure
    /// selection-fn tests below — only `tmux_name`, `state`, `pane_id`, and
    /// `created_at` matter to [`select_restart_pane_id`].
    fn make_record(
        tmux_name: &str,
        state: ManagedSessionState,
        pane_id: Option<&str>,
        created_at: chrono::DateTime<chrono::Utc>,
    ) -> SessionRecord {
        SessionRecord {
            id: ManagedSessionId::new(),
            tmux_name: tmux_name.to_string(),
            cwd: PathBuf::from("/tmp"),
            task: "task".to_string(),
            state,
            created_at,
            last_activity_at: None,
            workspace_path: None,
            repo_url: None,
            branch: None,
            pending_decision: None,
            proposed_default: None,
            correlation: crate::driver::SessionCorrelation::new(),
            runtime: crate::runtime::RuntimeKind::default(),
            ephemeral: true,
            workspace_owned: false,
            source_id: None,
            claude_session_id: None,
            scrollback_path: None,
            last_cwd: None,
            deliverable_id: None,
            pane_id: pane_id.map(str::to_owned),
            injection_status: Default::default(),
            worktree_owner: None,
        }
    }

    #[test]
    fn select_restart_pane_id_skips_decommissioned_record() {
        // A decommissioned record's tmux_name has been recycled by a live
        // record; the stale tombstone must never win.
        let now = chrono::Utc::now();
        let stale = make_record(
            "tmpm-proj-1",
            ManagedSessionState::Decommissioned,
            Some("%old"),
            now - chrono::Duration::hours(1),
        );
        let live = make_record(
            "tmpm-proj-1",
            ManagedSessionState::Active,
            Some("%new"),
            now,
        );
        let records = vec![stale, live];

        assert_eq!(
            select_restart_pane_id(&records, "tmpm-proj-1"),
            Some("%new".to_string())
        );
    }

    #[test]
    fn select_restart_pane_id_prefers_most_recent_when_multiple_live_match() {
        let now = chrono::Utc::now();
        let older = make_record(
            "tmpm-proj-1",
            ManagedSessionState::Stopped,
            Some("%older"),
            now - chrono::Duration::minutes(5),
        );
        let newer = make_record(
            "tmpm-proj-1",
            ManagedSessionState::Active,
            Some("%newer"),
            now,
        );
        let records = vec![older, newer];

        assert_eq!(
            select_restart_pane_id(&records, "tmpm-proj-1"),
            Some("%newer".to_string())
        );
    }

    #[test]
    fn select_restart_pane_id_none_when_no_match() {
        let now = chrono::Utc::now();
        let records = vec![make_record(
            "tmpm-other",
            ManagedSessionState::Active,
            Some("%x"),
            now,
        )];

        assert_eq!(select_restart_pane_id(&records, "tmpm-proj-1"), None);
    }
}
