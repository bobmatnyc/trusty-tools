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
//! functions through the mock backend. `project_register`'s merge-with-
//! existing behaviour (#3025 review follow-up item 4) needs a REAL
//! `DaemonState`/`ProjectRegistry` (a mock cannot observe persistence), so
//! that is covered by this module's own inline `#[cfg(test)]` instead.

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
/// reminder. `gh_account` (#3025) is the SEPARATE spawn-time pinning field:
/// when set, every session spawn/relaunch for this project resolves and
/// injects `GH_TOKEN`/`GH_USER` via
/// [`crate::core::gh_account::resolve_gh_account_env`].
/// What: builds a `Project` from the supplied fields (defaulting
/// `default_branch` to `"main"` when omitted); a `None` param for
/// `stack_hint`/`tags`/`description`/`gh_user`/`gh_account` PRESERVES the
/// current persisted value rather than clearing it (#3025 review follow-up
/// item 4 — mirrors [`crate::daemon::managed_routes::project_registry_routes::register_project_registry_route`]'s
/// identical fix; see that function's doc for the full rationale and the
/// deliberate `default_branch` exception). Calls `registry.register` and
/// returns the persisted record.
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
    gh_account: Option<&str>,
) -> Result<Value, String> {
    let registry = state.project_registry().await;
    // #2184: `register` REPLACES the whole record and `project_register`'s
    // params don't carry the github/commit-identity binding (operators set
    // those via `config_write`'s per-project fields instead) — preserve any
    // existing binding across a `project_register` call rather than silently
    // wiping it.
    let existing = registry.get(name).await.ok();
    let project = Project {
        name: name.to_string(),
        repo_url: repo_url.to_string(),
        default_branch: default_branch.unwrap_or("main").to_string(),
        stack_hint: stack_hint
            .map(str::to_string)
            .or_else(|| existing.as_ref().and_then(|p| p.stack_hint.clone())),
        tags: tags
            .or_else(|| existing.as_ref().map(|p| p.tags.clone()))
            .unwrap_or_default(),
        description: description
            .map(str::to_string)
            .or_else(|| existing.as_ref().and_then(|p| p.description.clone())),
        gh_user: gh_user
            .map(str::to_string)
            .or_else(|| existing.as_ref().and_then(|p| p.gh_user.clone())),
        gh_account: gh_account
            .map(str::to_string)
            .or_else(|| existing.as_ref().and_then(|p| p.gh_account.clone())),
        github: existing.as_ref().and_then(|p| p.github.clone()),
        commit_name: existing.as_ref().and_then(|p| p.commit_name.clone()),
        commit_email: existing.as_ref().and_then(|p| p.commit_email.clone()),
        // #3455: no param for this tool (set via `tm projects config` /
        // `config_write` instead) — preserve any existing value rather than
        // silently resetting it back to worktree-ON.
        worktree: existing.as_ref().and_then(|p| p.worktree),
    };
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

#[cfg(test)]
mod tests {
    use super::*;

    async fn isolated_state() -> Arc<DaemonState> {
        let root = tempfile::tempdir().expect("tempdir").keep();
        Arc::new(DaemonState::with_root_isolated_managed(root).await)
    }

    /// Why (#3025 review follow-up item 4): a `project_register` call that
    /// omits `stack_hint`/`tags`/`description`/`gh_user` must PRESERVE the
    /// currently-persisted values, not wipe them — the MCP-tool counterpart
    /// of `register_preserves_unspecified_optional_fields_on_existing_project`
    /// in `tests/project_registry_routes.rs`.
    /// Test: itself.
    #[tokio::test]
    async fn project_register_preserves_unspecified_optional_fields() {
        let state = isolated_state().await;
        project_register(
            &state,
            "widget",
            "https://github.com/acme/widget",
            None,
            Some("rust"),
            Some(vec!["backend".to_string()]),
            Some("the widget project"),
            Some("bobmatnyc"),
            None,
        )
        .await
        .expect("initial register");

        // Second call carries ONLY gh_account.
        let result = project_register(
            &state,
            "widget",
            "https://github.com/acme/widget",
            None,
            None,
            None,
            None,
            None,
            Some("bob-work"),
        )
        .await
        .expect("gh_account-only re-register");

        assert_eq!(result["gh_account"], "bob-work");
        assert_eq!(result["stack_hint"], "rust");
        assert_eq!(result["description"], "the widget project");
        assert_eq!(result["gh_user"], "bobmatnyc");
        assert_eq!(result["tags"][0], "backend");
    }

    /// Why: a value the SECOND call DOES supply must still override the
    /// existing one — merge-with-existing must never become
    /// merge-that-ignores-new-values.
    /// Test: itself.
    #[tokio::test]
    async fn project_register_explicit_fields_still_override_existing() {
        let state = isolated_state().await;
        project_register(
            &state,
            "widget",
            "https://github.com/acme/widget",
            None,
            Some("rust"),
            None,
            None,
            None,
            Some("bobmatnyc"),
        )
        .await
        .expect("initial register");

        let result = project_register(
            &state,
            "widget",
            "https://github.com/acme/widget",
            None,
            Some("python"),
            None,
            None,
            None,
            Some("bob-work"),
        )
        .await
        .expect("overriding re-register");

        assert_eq!(result["stack_hint"], "python");
        assert_eq!(result["gh_account"], "bob-work");
    }
}
