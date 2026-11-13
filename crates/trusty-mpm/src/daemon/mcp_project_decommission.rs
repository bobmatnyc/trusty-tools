//! Daemon entry points for project decommission (#10988).
//!
//! Why: the MCP tool, the socket method and the HTTP route must run one body,
//! so the transports cannot disagree on what decommission does. A separate
//! file keeps `mcp_project.rs` and `project_registry_routes.rs` under the cap.
//! What: [`project_decommission_op`] maps
//! [`SessionManager::decommission_project`](crate::session_manager::SessionManager::decommission_project)
//! onto [`DaemonError`]; [`project_decommission`] is the MCP wrapper;
//! [`project_decommission_route`] the HTTP handler.
//! Test: `project_decommission_marks_the_project_and_lists_it`,
//! `project_decommission_unknown_project_is_not_found` below;
//! `dispatch_project_decommission_tool` in `crate::mcp::tests`.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use serde_json::Value;

use crate::daemon::error::DaemonError;
use crate::daemon::state::DaemonState;
use crate::project::ProjectStoreError;
use crate::session_manager::project_decommission::{
    ProjectDecommissionError, ProjectDecommissionReport,
};

/// The transport-neutral body of every project-decommission entry point.
///
/// # Errors
///
/// [`DaemonError::NotFound`] for an unknown project; [`DaemonError::Unprocessable`]
/// for an Architect refusal or a partial stop, carrying the full message (which
/// names every session that stopped and every one that failed);
/// [`DaemonError::Internal`] for a registry stamp failure after the stops
/// (its message names the stopped sessions) and for any other registry failure.
/// Test: `project_decommission_marks_the_project_and_lists_it`,
/// `project_decommission_unknown_project_is_not_found`.
pub async fn project_decommission_op(
    state: &Arc<DaemonState>,
    name: &str,
) -> Result<ProjectDecommissionReport, DaemonError> {
    let mgr = state.session_manager().await;
    let registry = state.project_registry().await;
    mgr.decommission_project(&registry, name)
        .await
        .map_err(|e| match e {
            ProjectDecommissionError::Registry(ProjectStoreError::NotFound(_)) => {
                DaemonError::NotFound(format!("project {name} not found"))
            }
            ProjectDecommissionError::Registry(other) => {
                DaemonError::Internal(format!("project registry error: {other}"))
            }
            // #10988: carry the full message, which names every stopped session.
            stamp @ ProjectDecommissionError::StampFailed { .. } => {
                DaemonError::Internal(stamp.to_string())
            }
            refused @ (ProjectDecommissionError::Protected(_)
            | ProjectDecommissionError::PartialStop { .. }) => {
                DaemonError::Unprocessable(refused.to_string())
            }
        })
}

/// `project_decommission` MCP tool: [`project_decommission_op`] as JSON.
///
/// Test: `project_decommission_marks_the_project_and_lists_it`.
pub async fn project_decommission(state: &Arc<DaemonState>, name: &str) -> Result<Value, String> {
    let report = project_decommission_op(state, name)
        .await
        .map_err(|e| format!("project_decommission: {e}"))?;
    serde_json::to_value(report).map_err(|e| e.to_string())
}

/// `POST /api/v1/projects/{name}/decommission`.
///
/// Test: `parity_projects_registry_decommission_agrees_across_transports`.
pub async fn project_decommission_route(
    State(state): State<Arc<DaemonState>>,
    Path(name): Path<String>,
) -> Response {
    match project_decommission_op(&state, &name).await {
        Ok(report) => Json(report).into_response(),
        Err(e) => e.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::Project;

    async fn isolated_state() -> Arc<DaemonState> {
        let root = tempfile::tempdir().expect("tempdir").keep();
        Arc::new(DaemonState::with_root_isolated_managed(root).await)
    }

    /// Criteria 1 and 3: the MCP tool stamps the project, and `project_get` /
    /// `project_list` and the fleet view show it.
    #[tokio::test]
    async fn project_decommission_marks_the_project_and_lists_it() {
        let state = isolated_state().await;
        state
            .project_registry()
            .await
            .register(Project {
                name: "widget".into(),
                repo_url: "https://github.com/acme/widget".into(),
                default_branch: "main".into(),
                stack_hint: None,
                tags: vec![],
                description: None,
                gh_user: None,
                gh_account: None,
                github: None,
                commit_name: None,
                commit_email: None,
                worktree: None,
                project_key: Some("acme/widget".into()),
                decommissioned_at: None,
                archived_at: None,
            })
            .await
            .expect("register");

        let out = project_decommission(&state, "widget")
            .await
            .expect("decommission");
        assert!(out["project"]["decommissioned_at"].is_string(), "{out}");

        let got = crate::daemon::mcp_project::project_get(&state, "widget")
            .await
            .expect("get");
        assert!(got["decommissioned_at"].is_string(), "project_get: {got}");
        let listed = crate::daemon::mcp_project::project_list(&state)
            .await
            .expect("list");
        assert!(
            listed["projects"][0]["decommissioned_at"].is_string(),
            "project_list: {listed}"
        );
        let fleet = crate::daemon::managed_routes::fleet::fleet_core(&state, true)
            .await
            .into_rpc()
            .expect("fleet");
        assert_eq!(fleet["projects"][0]["decommissioned"], true, "{fleet}");
    }

    /// An unknown project is a 404-class error, not a silent success.
    #[tokio::test]
    async fn project_decommission_unknown_project_is_not_found() {
        let state = isolated_state().await;
        let err = project_decommission_op(&state, "nope")
            .await
            .expect_err("unknown");
        assert!(matches!(err, DaemonError::NotFound(_)), "{err}");
    }
}
