//! Fleet-by-project route for the managed session API.
//!
//! Why: extracted from `managed_routes/mod.rs` to keep that file under the
//! 500-SLOC production cap. The fleet endpoint is a cohesive unit with its own
//! request/response types and is large enough to warrant a sibling file.
//! What: defines [`FleetProjectGroup`], [`FleetByProjectResponse`], and the
//! [`fleet_by_project_route`] axum handler.
//! Test: `fleet_route_groups_by_project` in `tests/session_manager_mvp.rs`.

use std::sync::Arc;

use axum::{Json, extract::State, response::IntoResponse};
use serde::Serialize;
use tracing::warn;

use crate::daemon::state::DaemonState;
use crate::session_manager::SessionRecord;

use super::{SessionSummary, record_to_summary};

/// Per-project group entry for GET /api/v1/sessions/managed/fleet.
///
/// Why: the fleet-by-project response nests sessions under their owning project;
/// a typed struct keeps the shape explicit and forward-compatible.
/// What: the project name/URL and the session summaries bound to it.
/// Test: `fleet_route_groups_by_project` in `tests/session_manager_mvp.rs`.
#[derive(Debug, Serialize)]
pub struct FleetProjectGroup {
    /// Registered project name.
    pub project_name: String,
    /// Repository URL for the project.
    pub repo_url: String,
    /// Sessions bound to this project (may be empty).
    pub sessions: Vec<SessionSummary>,
}

/// Response for GET /api/v1/sessions/managed/fleet.
///
/// Why: wraps the per-project groups so the wire shape is consistent with other
/// list endpoints (which all wrap their arrays under a key).
/// What: a `projects` array of [`FleetProjectGroup`] entries.
/// Test: `fleet_route_groups_by_project` in `tests/session_manager_mvp.rs`.
#[derive(Debug, Serialize)]
pub struct FleetByProjectResponse {
    /// Per-project session groups.
    pub projects: Vec<FleetProjectGroup>,
}

/// GET /api/v1/sessions/managed/fleet — list managed sessions grouped by project.
///
/// Why: an operator working across multiple repos benefits from a per-project
/// view of active sessions rather than a flat list. `fleet_by_project` from the
/// project resolver provides the grouping; this route exposes it over HTTP so
/// every adapter (Telegram `/fleet`, TUI, CLI) can render the same grouped view.
/// What: reads all live sessions from the `SessionManager`, all registered
/// projects from the `ProjectRegistry`, runs `fleet_by_project` to group them,
/// and returns a `FleetByProjectResponse` with one `FleetProjectGroup` per
/// project (empty `sessions` list included — the operator sees the full project
/// landscape, not just active ones).
/// Test: `fleet_route_groups_by_project` in `tests/session_manager_mvp.rs`.
pub async fn fleet_by_project_route(State(state): State<Arc<DaemonState>>) -> impl IntoResponse {
    let mgr = state.session_manager().await;
    let registry = state.project_registry().await;

    let raw_sessions: Vec<SessionRecord> = mgr.list().await;
    // Gracefully degrade: if the project store is unreadable, return an empty
    // project list rather than a 500 — the fleet view is non-critical.
    // Log at warn so operators can distinguish a broken registry from a
    // legitimately empty one (an empty registry silently returns `projects: []`).
    let projects = match registry.list().await {
        Ok(p) => p,
        Err(e) => {
            warn!(
                error = %e,
                "fleet_by_project_route: project registry read failed; \
                 returning empty project list (sessions will not be grouped)"
            );
            vec![]
        }
    };

    let fleet = crate::project::fleet_by_project(&raw_sessions, &projects);

    let project_groups: Vec<FleetProjectGroup> = fleet
        .into_iter()
        .map(|pf| FleetProjectGroup {
            project_name: pf.project.name.clone(),
            repo_url: pf.project.repo_url.clone(),
            sessions: pf.sessions.iter().map(record_to_summary).collect(),
        })
        .collect();

    Json(FleetByProjectResponse {
        projects: project_groups,
    })
}
