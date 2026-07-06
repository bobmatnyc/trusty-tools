//! Daemon-side implementation of the project-registry MCP tools
//! (WI-2 + WI-5, #1519 / #1517).
//!
//! Why: the MCP `StateBackend` (in `mcp_backend.rs`) must service project-registry
//! and NL-resolver tools, but inlining their bodies there would push that file
//! over the 500-SLOC production cap. This module holds the wrapping logic,
//! mirroring the `mcp_session.rs` / `mcp_console.rs` pattern.
//!
//! Four free async functions are exposed:
//! `project_list`, `project_register`, `project_get` (registry CRUD, WI-2) and
//! `project_resolve` (NL→project resolver, WI-5).
//! Each takes `&Arc<DaemonState>` plus parsed arguments and returns a JSON value
//! or a human-readable error string.
//!
//! Test: the dispatch-level tests in `crate::mcp::tests` exercise these
//! functions through the mock backend.

use std::sync::Arc;

use serde_json::{Value, json};

use crate::daemon::state::DaemonState;
use crate::project::record::Project;
use crate::project::resolver;

/// Return all registered projects as a JSON array.
///
/// Why: `project_list` gives the driver skill and operators a typed,
/// JSON-native inventory of known repos without scraping CLI text.
/// What: delegates to `DaemonState::project_registry` and serializes all
/// entries; an empty registry returns `{ "projects": [] }`.
/// Test: `dispatch_project_list_tool` in `crate::mcp::tests`.
pub async fn project_list(state: &Arc<DaemonState>) -> Result<Value, String> {
    let registry = state.project_registry().await;
    let projects = registry
        .list()
        .await
        .map_err(|e| format!("project_list: registry error: {e}"))?;
    let values = projects
        .into_iter()
        .map(|p| serde_json::to_value(p).map_err(|e| format!("project_list: serialize error: {e}")))
        .collect::<Result<Vec<Value>, String>>()?;
    let count = values.len();
    Ok(json!({ "projects": values, "count": count }))
}

/// Register or update a project in the registry.
///
/// Why: `project_register` is the typed, JSON-native way to add a project
/// without editing `config.yaml` or restarting the daemon. `gh_user` (#2081)
/// lets an operator declare the project's preferred `gh` account so callers
/// can scope `gh` operations to it instead of relying on a per-session
/// reminder — see [`crate::core::gh_account::resolve_gh_account_env`] for the
/// non-mutating resolution mechanism.
/// What: builds a `Project` from the supplied fields (defaulting
/// `default_branch` to `"main"` when omitted), calls `registry.register`,
/// and returns the persisted record.
/// Test: `dispatch_project_register_tool` in `crate::mcp::tests`.
#[allow(clippy::too_many_arguments)]
pub async fn project_register(
    state: &Arc<DaemonState>,
    name: &str,
    repo_url: &str,
    default_branch: Option<&str>,
    stack_hint: Option<&str>,
    tags: Option<Vec<String>>,
    description: Option<&str>,
    gh_user: Option<&str>,
) -> Result<Value, String> {
    let project = Project {
        name: name.to_string(),
        repo_url: repo_url.to_string(),
        default_branch: default_branch.unwrap_or("main").to_string(),
        stack_hint: stack_hint.map(str::to_string),
        tags: tags.unwrap_or_default(),
        description: description.map(str::to_string),
        gh_user: gh_user.map(str::to_string),
    };
    let registry = state.project_registry().await;
    registry
        .register(project.clone())
        .await
        .map_err(|e| format!("project_register: registry error: {e}"))?;
    serde_json::to_value(&project).map_err(|e| e.to_string())
}

/// Look up a single project by name.
///
/// Why: `project_get` is the typed point-lookup that lets a driver retrieve
/// a project's `repo_url` and `default_branch` without listing all projects.
/// What: calls `registry.get(name)` and serializes the result; returns a
/// descriptive error string when the name is not registered.
/// Test: `dispatch_project_get_tool` in `crate::mcp::tests`.
pub async fn project_get(state: &Arc<DaemonState>, name: &str) -> Result<Value, String> {
    let registry = state.project_registry().await;
    let project = registry
        .get(name)
        .await
        .map_err(|e| format!("project `{name}` not found: {e}"))?;
    serde_json::to_value(&project).map_err(|e| e.to_string())
}

/// Resolve a natural-language query to the best-matching registered project.
///
/// Why: `project_resolve` is the NL→repo entry point for all surfaces
/// (Telegram free-text, coordinator chat, driver skills) that need to route a
/// human-written query to a specific project without knowing its exact name or
/// URL. Centralising it here keeps routing logic in one testable place.
/// What: lists all registered projects, calls [`resolver::resolve_project`],
/// and serialises the result as:
/// ```json
/// {
///   "primary": {
///     "project": { "name": "…", "repo_url": "…", … },
///     "confidence": 0.95,
///     "reason": "url match"
///   },
///   "needs_disambiguation": false,
///   "matches": [
///     { "project": { … }, "confidence": 0.95, "reason": "url match" }
///   ]
/// }
/// ```
/// On no-match or empty registry `primary` is `null` and `error` explains
/// the failure.
/// Test: `dispatch_project_resolve_tool` in `crate::mcp::tests`.
pub async fn project_resolve(state: &Arc<DaemonState>, query: &str) -> Result<Value, String> {
    let registry = state.project_registry().await;
    let projects = registry
        .list()
        .await
        .map_err(|e| format!("project_resolve: registry error: {e}"))?;

    match resolver::resolve_project(query, &projects) {
        Ok(resolution) => {
            let primary_val = resolution.primary.as_ref().map(|m| {
                json!({
                    "project": serde_json::to_value(&m.project)
                        .unwrap_or(Value::Null),
                    "confidence": m.confidence,
                    "reason": m.reason.label(),
                })
            });
            let all_matches: Vec<Value> = resolution
                .matches
                .iter()
                .map(|m| {
                    json!({
                        "project": serde_json::to_value(&m.project)
                            .unwrap_or(Value::Null),
                        "confidence": m.confidence,
                        "reason": m.reason.label(),
                    })
                })
                .collect();
            Ok(json!({
                "primary": primary_val,
                "needs_disambiguation": resolution.needs_disambiguation(),
                "matches": all_matches,
            }))
        }
        Err(resolver::ResolverError::NoMatch { query: q }) => Ok(json!({
            "primary": null,
            "needs_disambiguation": false,
            "matches": [],
            "error": format!("no project matched query: {q:?}"),
        })),
        Err(resolver::ResolverError::EmptyRegistry) => Ok(json!({
            "primary": null,
            "needs_disambiguation": false,
            "matches": [],
            "error": "project registry is empty; register at least one project first",
        })),
    }
}
