//! Deterministic project status-aggregation route (#2117, DOC-35 §4.1).
//!
//! Why: `tm projects status <name>` (#2115) and the multipane-TUI Projects-pane
//! aggregate glyph (#2118) need a single-call rollup of a project's session
//! landscape without every consumer re-implementing the count/max logic. DOC-35
//! §11 draws a hard boundary: this endpoint is L3-substrate (deterministic
//! control plane) — it polls and reports already-computed state and MUST NEVER
//! call an LLM, reason across projects, or infer anything. It is a pure function
//! of already-materialized state: re-running it with no state change between
//! calls yields byte-identical output.
//! What: defines the response shapes ([`SessionStateCounts`],
//! [`ProjectConfigFlags`], [`ProjectStatusResponse`]), the pure aggregation
//! function [`aggregate_project_status`], and the axum handler
//! [`project_status_route`] for `GET /api/v1/projects/{name}/status`.
//! Test: `aggregate_project_status_*` unit tests in `tests()` below (pure-rollup
//! contract) and `status_route_returns_deterministic_rollup` /
//! `status_route_unknown_project_is_404` HTTP handler tests in
//! `tests/project_status_route.rs`.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path as AxumPath, State},
    http::StatusCode,
    response::IntoResponse,
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use tracing::warn;

use crate::daemon::state::DaemonState;
use crate::project::{Project, ProjectStoreError, fleet_by_project};
use crate::session_manager::{ManagedSessionState, SessionRecord};

/// Histogram of a project's sessions by [`ManagedSessionState`].
///
/// Why: the aggregate view needs a per-state count so a consumer can render
/// "3 active, 1 errored" without walking the raw session list. A typed struct
/// (one field per variant) keeps the wire shape explicit and self-documenting
/// rather than a map keyed by stringly-typed states.
/// What: one `usize` per lifecycle variant plus `total` (the sum of all five —
/// the number of session records bound to the project, tombstones included).
/// Every field is a pure count over already-persisted `SessionRecord.state`.
/// Test: `aggregate_project_status_counts_by_state`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionStateCounts {
    /// Count of sessions in [`ManagedSessionState::Provisioning`].
    pub provisioning: usize,
    /// Count of sessions in [`ManagedSessionState::Active`].
    pub active: usize,
    /// Count of sessions in [`ManagedSessionState::Stopped`].
    pub stopped: usize,
    /// Count of sessions in [`ManagedSessionState::Errored`].
    pub errored: usize,
    /// Count of sessions in [`ManagedSessionState::Decommissioned`] (tombstones).
    pub decommissioned: usize,
    /// Total session records bound to the project (sum of all five counts).
    pub total: usize,
}

/// Boolean config-completeness flags for a project (DOC-35 §4.1).
///
/// Why: the aggregate view surfaces at-a-glance whether a project is fully
/// configured for delegated `gh` operations without the consumer reading the
/// full [`Project`] record. Every flag is a pure `Option::is_some()` check over
/// already-persisted config — no validation, no network call, no `gh auth`
/// probe (that mutating/side-effecting check belongs to the PATCH route, §4).
/// What: one bool per config field that gates project automation. `jira_config`
/// is intentionally absent until #2082/#2122 land its persisted field; per §4.1
/// it will be added here additively as `jira_config_set` at that point.
/// Test: `aggregate_project_status_config_flags`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectConfigFlags {
    /// Whether [`Project::gh_user`] is set (preferred `gh` login, #2081).
    pub gh_user_set: bool,
    /// Whether [`Project::github`] (per-project `gh` identity binding, #2184) is set.
    pub github_binding_set: bool,
}

/// Deterministic rollup of one project's status (`GET .../{name}/status`).
///
/// Why: the single response body for #2117 — the pure-rollup contract of
/// DOC-35 §4.1. It is designed to extend additively: #2382 (Wave 2) adds
/// `deliverables`/`milestones` histogram fields here once the §10 data model
/// (#2378) lands, computed the same way over the same kind of already-persisted
/// state. Adding serde-serialized fields is non-breaking for existing consumers,
/// so no version bump or shape change is required.
/// What: the project identity (`project_name`/`repo_url`), the session-state
/// histogram, the most-recent `last_activity_at` across the project's sessions,
/// and the config-completeness flags. Every field is a pure function of the
/// inputs — zero inference, zero LLM, single-project scope.
/// Test: `aggregate_project_status_counts_by_state`,
/// `aggregate_project_status_max_activity`, `aggregate_project_status_config_flags`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectStatusResponse {
    /// Registered project name (the request path key).
    pub project_name: String,
    /// Repository URL for the project.
    pub repo_url: String,
    /// Per-state session histogram.
    pub sessions: SessionStateCounts,
    /// Most recent `last_activity_at` across the project's sessions, if any.
    ///
    /// `None` when no bound session has ever recorded activity.
    pub last_activity_at: Option<DateTime<Utc>>,
    /// Config-completeness flags.
    pub config: ProjectConfigFlags,
}

/// Compute the deterministic status rollup for a project.
///
/// Why: the pure core of #2117, extracted from the handler so the rollup logic
/// is unit-testable with hand-constructed inputs and so its determinism is
/// self-evident — it takes owned/borrowed state, performs only counting and a
/// `max`, and does zero I/O, no LLM call, and no cross-project reasoning
/// (DOC-35 §11 boundary contract). Given the same `project` and `all_sessions`
/// it returns a byte-identical [`ProjectStatusResponse`] every time.
/// What: filters `all_sessions` to those bound to `project` (reusing the shared
/// [`fleet_by_project`] URL-matching so binding stays consistent with the
/// `/fleet` route), tallies a per-state histogram, takes the maximum
/// `last_activity_at` across the bound sessions, and reads the two config flags
/// off the record. Sessions with no `repo_url` (or an unmatched one) are omitted,
/// exactly as `fleet_by_project` specifies.
/// Test: `aggregate_project_status_counts_by_state`,
/// `aggregate_project_status_max_activity`, `aggregate_project_status_config_flags`,
/// `aggregate_project_status_is_deterministic`.
pub fn aggregate_project_status(
    project: &Project,
    all_sessions: &[SessionRecord],
) -> ProjectStatusResponse {
    // Reuse the shared fleet grouping so "which sessions belong to this project"
    // is answered identically here and by the /fleet route. Passing a
    // single-project slice yields exactly one ProjectFleet.
    let bound: Vec<SessionRecord> = fleet_by_project(all_sessions, std::slice::from_ref(project))
        .into_iter()
        .next()
        .map(|fleet| fleet.sessions)
        .unwrap_or_default();

    let mut counts = SessionStateCounts {
        provisioning: 0,
        active: 0,
        stopped: 0,
        errored: 0,
        decommissioned: 0,
        total: 0,
    };
    let mut last_activity_at: Option<DateTime<Utc>> = None;

    for session in &bound {
        match session.state {
            ManagedSessionState::Provisioning => counts.provisioning += 1,
            ManagedSessionState::Active => counts.active += 1,
            ManagedSessionState::Stopped => counts.stopped += 1,
            ManagedSessionState::Errored => counts.errored += 1,
            ManagedSessionState::Decommissioned => counts.decommissioned += 1,
        }
        counts.total += 1;

        if let Some(ts) = session.last_activity_at {
            last_activity_at = Some(match last_activity_at {
                Some(current) => current.max(ts),
                None => ts,
            });
        }
    }

    ProjectStatusResponse {
        project_name: project.name.clone(),
        repo_url: project.repo_url.clone(),
        sessions: counts,
        last_activity_at,
        config: ProjectConfigFlags {
            gh_user_set: project.gh_user.is_some(),
            github_binding_set: project.github.is_some(),
        },
    }
}

/// GET /api/v1/projects/{name}/status — deterministic project status rollup.
///
/// Why: exposes [`aggregate_project_status`] over HTTP so the deterministic CLI
/// (#2115) and TUI (#2118) — and any other consumer, including #2109 per the
/// §11 "expose data over MCP/HTTP for ANY consumer to read" row — can read the
/// rollup without an MCP client. The handler is a thin composition over
/// `ProjectRegistry::get` + `SessionManager::list`; it adds no persistence and
/// no reasoning.
/// What: looks up the named project (404 if unregistered), lists all live
/// session records, and returns `Json(aggregate_project_status(..))`. A project
/// store read error (other than not-found) degrades to 500 with a logged warning
/// rather than a panic — the library never `unwrap`s a store result.
/// Test: `status_route_returns_deterministic_rollup`,
/// `status_route_unknown_project_is_404` in `tests/project_status_route.rs`.
pub async fn project_status_route(
    State(state): State<Arc<DaemonState>>,
    AxumPath(name): AxumPath<String>,
) -> impl IntoResponse {
    let registry = state.project_registry().await;
    let project = match registry.get(&name).await {
        Ok(project) => project,
        Err(ProjectStoreError::NotFound(_)) => {
            return (StatusCode::NOT_FOUND, format!("project {name} not found")).into_response();
        }
        Err(e) => {
            warn!(
                error = %e,
                project = %name,
                "project_status_route: project registry read failed"
            );
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "project registry read failed".to_string(),
            )
                .into_response();
        }
    };

    let mgr = state.session_manager().await;
    let sessions = mgr.list().await;

    Json(aggregate_project_status(&project, &sessions)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_manager::{ManagedSessionId, SessionRecord};
    use chrono::TimeZone;
    use std::path::PathBuf;

    /// Build a minimal [`Project`] fixture with the given name/url and no config.
    fn project(name: &str, repo_url: &str) -> Project {
        Project {
            name: name.to_string(),
            repo_url: repo_url.to_string(),
            default_branch: "main".to_string(),
            stack_hint: None,
            tags: vec![],
            description: None,
            gh_user: None,
            github: None,
            commit_name: None,
            commit_email: None,
        }
    }

    /// Build a [`SessionRecord`] fixture in `state` bound to `repo_url`, with an
    /// optional `last_activity_at`.
    fn session(
        state: ManagedSessionState,
        repo_url: Option<&str>,
        activity: Option<DateTime<Utc>>,
    ) -> SessionRecord {
        SessionRecord {
            id: ManagedSessionId::new(),
            tmux_name: "tm-test".to_string(),
            cwd: PathBuf::from("/tmp"),
            task: "fixture".to_string(),
            state,
            created_at: Utc::now(),
            last_activity_at: activity,
            workspace_path: None,
            repo_url: repo_url.map(str::to_string),
            branch: None,
            pending_decision: None,
            proposed_default: None,
            correlation: Default::default(),
            runtime: Default::default(),
            ephemeral: false,
            workspace_owned: false,
            source_id: None,
            claude_session_id: None,
            scrollback_path: None,
            last_cwd: None,
            deliverable_id: None,
        }
    }

    /// The histogram counts each state exactly once and `total` is their sum;
    /// sessions bound to a DIFFERENT repo (or none) are excluded.
    #[test]
    fn aggregate_project_status_counts_by_state() {
        let url = "https://github.com/acme/widget";
        let proj = project("widget", url);
        let sessions = vec![
            session(ManagedSessionState::Active, Some(url), None),
            session(ManagedSessionState::Active, Some(url), None),
            session(ManagedSessionState::Stopped, Some(url), None),
            session(ManagedSessionState::Errored, Some(url), None),
            session(ManagedSessionState::Provisioning, Some(url), None),
            session(ManagedSessionState::Decommissioned, Some(url), None),
            // Bound to a different project — must be excluded.
            session(
                ManagedSessionState::Active,
                Some("https://github.com/acme/other"),
                None,
            ),
            // No repo_url — must be excluded.
            session(ManagedSessionState::Active, None, None),
        ];

        let out = aggregate_project_status(&proj, &sessions);

        assert_eq!(out.sessions.active, 2);
        assert_eq!(out.sessions.stopped, 1);
        assert_eq!(out.sessions.errored, 1);
        assert_eq!(out.sessions.provisioning, 1);
        assert_eq!(out.sessions.decommissioned, 1);
        assert_eq!(out.sessions.total, 6, "only the six bound sessions count");
        assert_eq!(out.project_name, "widget");
        assert_eq!(out.repo_url, url);
    }

    /// `last_activity_at` is the maximum across bound sessions; sessions with no
    /// activity contribute nothing, and an all-`None` set yields `None`.
    #[test]
    fn aggregate_project_status_max_activity() {
        let url = "https://github.com/acme/widget";
        let proj = project("widget", url);
        let t_old = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let t_new = Utc.with_ymd_and_hms(2026, 7, 10, 12, 0, 0).unwrap();

        let out = aggregate_project_status(
            &proj,
            &[
                session(ManagedSessionState::Stopped, Some(url), Some(t_old)),
                session(ManagedSessionState::Active, Some(url), Some(t_new)),
                session(ManagedSessionState::Active, Some(url), None),
            ],
        );
        assert_eq!(out.last_activity_at, Some(t_new));

        // No activity anywhere → None.
        let none = aggregate_project_status(
            &proj,
            &[session(ManagedSessionState::Provisioning, Some(url), None)],
        );
        assert_eq!(none.last_activity_at, None);

        // No bound sessions at all → all zero, None activity.
        let empty = aggregate_project_status(&proj, &[]);
        assert_eq!(empty.sessions.total, 0);
        assert_eq!(empty.last_activity_at, None);
    }

    /// Config flags are pure `is_some()` reads over the project record.
    #[test]
    fn aggregate_project_status_config_flags() {
        let url = "https://github.com/acme/widget";
        let mut proj = project("widget", url);
        let bare = aggregate_project_status(&proj, &[]);
        assert!(!bare.config.gh_user_set);
        assert!(!bare.config.github_binding_set);

        proj.gh_user = Some("acme-bot".to_string());
        let with_user = aggregate_project_status(&proj, &[]);
        assert!(with_user.config.gh_user_set);
        assert!(!with_user.config.github_binding_set);
    }

    /// Re-running the rollup with unchanged inputs yields identical output —
    /// the DOC-35 §11 determinism test made executable.
    #[test]
    fn aggregate_project_status_is_deterministic() {
        let url = "https://github.com/acme/widget";
        let proj = project("widget", url);
        let t = Utc.with_ymd_and_hms(2026, 7, 10, 12, 0, 0).unwrap();
        let sessions = vec![
            session(ManagedSessionState::Active, Some(url), Some(t)),
            session(ManagedSessionState::Errored, Some(url), None),
        ];
        let a = aggregate_project_status(&proj, &sessions);
        let b = aggregate_project_status(&proj, &sessions);
        assert_eq!(a, b);
    }
}
