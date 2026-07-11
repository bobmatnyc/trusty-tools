//! Registry-B project HTTP surface (DOC-35 §4, #2114) — list / register / get.
//!
//! Why: `tm projects list/register/show` (#2115) are thin deterministic HTTP
//! clients (DOC-35 §1.3) and need `GET/POST /api/v1/projects` and
//! `GET /api/v1/projects/{name}` to call. Registry B (`crate::project`) was until
//! now reachable only via the MCP tools (`mcp_project.rs`) and the `/status`
//! rollup route; the plain-HTTP list/register/get surface those verbs require did
//! not exist, so this module adds it. Each handler is a thin, deterministic
//! composition over [`crate::project::ProjectRegistry`] — no LLM, no cross-store
//! reasoning (§11) — mirroring the existing `mcp_project` bodies so the HTTP and
//! MCP surfaces stay behaviourally identical (idempotent upsert keyed on `name`,
//! preserving the per-project `gh`/commit identity binding across a re-register).
//! What: [`RegisterProjectBody`], [`ProjectsListResponse`], and the three axum
//! handlers [`list_projects_registry_route`], [`register_project_registry_route`],
//! and [`get_project_registry_route`]. `PATCH` (the `tm projects config` field
//! editor, §3.1) is intentionally NOT added here — it belongs to the config-verb
//! work, not #2115's read/register slice.
//! Test: `register_body_deserializes`, `register_body_minimal`, and the HTTP
//! handler tests in `tests/project_registry_routes.rs`.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path as AxumPath, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::daemon::state::DaemonState;
use crate::project::{Project, ProjectStoreError};

/// JSON body for `POST /api/v1/projects` — an idempotent project upsert.
///
/// Why: registering a project from the deterministic CLI (#2115) mirrors the
/// `project_register` MCP tool's arguments so the two surfaces never drift. Only
/// `name` and `repo_url` are required; the rest are optional descriptive config.
/// The per-project `gh`/commit identity binding (#2184) is deliberately absent —
/// like `project_register`, a re-register PRESERVES any existing binding rather
/// than wiping it (operators set those via the config path).
/// What: the register arguments, all optional beyond `name`/`repo_url`, with
/// `default_branch` defaulting to `"main"` when omitted.
/// Test: `register_body_deserializes`, `register_body_minimal`.
#[derive(Debug, Deserialize)]
pub struct RegisterProjectBody {
    /// Registry key (non-empty).
    pub name: String,
    /// Full repository URL (non-empty).
    pub repo_url: String,
    /// Default branch; defaults to `"main"` when omitted.
    #[serde(default)]
    pub default_branch: Option<String>,
    /// Free-form description.
    #[serde(default)]
    pub description: Option<String>,
    /// Classification tags.
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    /// Technology-stack hint (e.g. `rust`).
    #[serde(default)]
    pub stack_hint: Option<String>,
    /// Preferred `gh` login for this project (#2081).
    #[serde(default)]
    pub gh_user: Option<String>,
}

/// Response body for `GET /api/v1/projects`.
///
/// Why: mirrors the `project_list` MCP tool's `{ projects, count }` shape so a
/// consumer can read the inventory and its size in one call.
/// What: the registered projects plus their count.
/// Test: `tests/project_registry_routes.rs::list_returns_registered_projects`.
#[derive(Debug, Serialize)]
pub struct ProjectsListResponse {
    /// Every registered project.
    pub projects: Vec<Project>,
    /// `projects.len()`, surfaced so scripts need not count client-side.
    pub count: usize,
}

/// `GET /api/v1/projects` — list every registered registry-B project.
///
/// Why: backs `tm projects list` (#2115). A pure read over
/// [`ProjectRegistry::list`](crate::project::ProjectRegistry::list).
/// What: returns `{ projects, count }`; a store read failure degrades to 500 with
/// a logged warning rather than a panic (the library never `unwrap`s a store
/// result).
/// Test: `tests/project_registry_routes.rs::list_returns_registered_projects`.
pub async fn list_projects_registry_route(
    State(state): State<Arc<DaemonState>>,
) -> impl IntoResponse {
    let registry = state.project_registry().await;
    match registry.list().await {
        Ok(projects) => {
            let count = projects.len();
            Json(ProjectsListResponse { projects, count }).into_response()
        }
        Err(e) => {
            warn!(error = %e, "list_projects_registry_route: registry read failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "project registry read failed".to_string(),
            )
                .into_response()
        }
    }
}

/// `POST /api/v1/projects` — register (idempotent upsert) a registry-B project.
///
/// Why: backs `tm projects register` (#2115). Mirrors the `project_register` MCP
/// tool: upsert keyed on `name`, preserving any existing `gh`/commit identity
/// binding so a plain re-register never silently wipes config set elsewhere.
/// What: rejects a blank `name`/`repo_url` with 400; builds a [`Project`]
/// (defaulting `default_branch` to `"main"`), preserves the existing binding,
/// persists via [`ProjectRegistry::register`](crate::project::ProjectRegistry::register),
/// and returns 201 with the stored record.
/// Test: `tests/project_registry_routes.rs::register_then_get_round_trips`,
/// `register_preserves_existing_identity_binding`.
pub async fn register_project_registry_route(
    State(state): State<Arc<DaemonState>>,
    Json(body): Json<RegisterProjectBody>,
) -> impl IntoResponse {
    if body.name.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "project name must not be empty").into_response();
    }
    if body.repo_url.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "repo_url must not be empty").into_response();
    }

    let registry = state.project_registry().await;
    // #2184 parity with `project_register`: a re-register REPLACES the record, so
    // carry any existing per-project `gh`/commit identity binding forward rather
    // than clobbering it (this route's body cannot express those fields).
    let existing = registry.get(&body.name).await.ok();
    let project = Project {
        name: body.name,
        repo_url: body.repo_url,
        default_branch: body.default_branch.unwrap_or_else(|| "main".to_string()),
        stack_hint: body.stack_hint,
        tags: body.tags.unwrap_or_default(),
        description: body.description,
        gh_user: body.gh_user,
        github: existing.as_ref().and_then(|p| p.github.clone()),
        commit_name: existing.as_ref().and_then(|p| p.commit_name.clone()),
        commit_email: existing.as_ref().and_then(|p| p.commit_email.clone()),
    };
    if let Err(e) = registry.register(project.clone()).await {
        warn!(error = %e, project = %project.name, "register_project_registry_route: register failed");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "project registry write failed".to_string(),
        )
            .into_response();
    }
    (StatusCode::CREATED, Json(project)).into_response()
}

/// `GET /api/v1/projects/{name}` — fetch one registry-B project by name.
///
/// Why: backs `tm projects show` (#2115) — the config half of the read-only
/// project view (the nested sessions half comes from the fleet endpoint, §3.1).
/// What: returns the project (200) or a 404 when unregistered; any other store
/// error degrades to 500 with a logged warning.
/// Test: `tests/project_registry_routes.rs::register_then_get_round_trips`,
/// `get_unknown_project_is_404`.
pub async fn get_project_registry_route(
    State(state): State<Arc<DaemonState>>,
    AxumPath(name): AxumPath<String>,
) -> impl IntoResponse {
    let registry = state.project_registry().await;
    match registry.get(&name).await {
        Ok(project) => Json(project).into_response(),
        Err(ProjectStoreError::NotFound(_)) => {
            (StatusCode::NOT_FOUND, format!("project {name} not found")).into_response()
        }
        Err(e) => {
            warn!(error = %e, project = %name, "get_project_registry_route: registry read failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "project registry read failed".to_string(),
            )
                .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A full register body deserializes with every field populated.
    #[test]
    fn register_body_deserializes() {
        let json = serde_json::json!({
            "name": "widget",
            "repo_url": "https://github.com/acme/widget",
            "default_branch": "develop",
            "description": "the widget",
            "tags": ["backend", "oss"],
            "stack_hint": "rust",
            "gh_user": "acme-bot"
        });
        let body: RegisterProjectBody = serde_json::from_value(json).unwrap();
        assert_eq!(body.name, "widget");
        assert_eq!(body.repo_url, "https://github.com/acme/widget");
        assert_eq!(body.default_branch.as_deref(), Some("develop"));
        assert_eq!(
            body.tags.as_deref(),
            Some(&["backend".to_string(), "oss".to_string()][..])
        );
        assert_eq!(body.gh_user.as_deref(), Some("acme-bot"));
    }

    /// Only `name`/`repo_url` are required; the rest default to absent.
    #[test]
    fn register_body_minimal() {
        let json = serde_json::json!({
            "name": "minimal",
            "repo_url": "https://github.com/acme/minimal"
        });
        let body: RegisterProjectBody = serde_json::from_value(json).unwrap();
        assert_eq!(body.name, "minimal");
        assert!(body.default_branch.is_none());
        assert!(body.tags.is_none());
        assert!(body.stack_hint.is_none());
        assert!(body.gh_user.is_none());
    }
}
