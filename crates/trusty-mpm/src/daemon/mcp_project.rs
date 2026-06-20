//! Daemon-side implementation of the three project-registry MCP tools
//! (#1519 WI-2).
//!
//! Why: the MCP `StateBackend` (in `mcp_backend.rs`) must service the new
//! `project_*` tools, but inlining their bodies there would push that file
//! over the 500-SLOC production cap. This module holds the wrapping logic,
//! mirroring the `mcp_session.rs` / `mcp_console.rs` pattern.
//! What: three free async functions — `project_list`, `project_register`, and
//! `project_get` — each taking `&Arc<DaemonState>` plus parsed arguments and
//! returning a JSON value or a human-readable error string.
//! Test: the dispatch-level tests in `crate::mcp::tests` exercise these
//! functions through the mock backend.

use std::sync::Arc;

use serde_json::{Value, json};

use crate::daemon::state::DaemonState;
use crate::project::record::Project;

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
    let values: Vec<Value> = projects
        .into_iter()
        .map(|p| serde_json::to_value(p).unwrap_or(Value::Null))
        .collect();
    Ok(json!({ "projects": values, "count": values.len() }))
}

/// Register or update a project in the registry.
///
/// Why: `project_register` is the typed, JSON-native way to add a project
/// without editing `config.yaml` or restarting the daemon.
/// What: builds a `Project` from the supplied fields (defaulting
/// `default_branch` to `"main"` when omitted), calls `registry.register`,
/// and returns the persisted record.
/// Test: `dispatch_project_register_tool` in `crate::mcp::tests`.
pub async fn project_register(
    state: &Arc<DaemonState>,
    name: &str,
    repo_url: &str,
    default_branch: Option<&str>,
    stack_hint: Option<&str>,
    tags: Option<Vec<String>>,
    description: Option<&str>,
) -> Result<Value, String> {
    let project = Project {
        name: name.to_string(),
        repo_url: repo_url.to_string(),
        default_branch: default_branch.unwrap_or("main").to_string(),
        stack_hint: stack_hint.map(str::to_string),
        tags: tags.unwrap_or_default(),
        description: description.map(str::to_string),
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
