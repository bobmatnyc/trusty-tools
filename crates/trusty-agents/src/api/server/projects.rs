//! Project / session / agent listing + registration handlers
//! (#341, #405, #407, #451, #465).
//!
//! Why: These read-mostly endpoints back the WebUI's project navigator,
//! session-history sidebar, and agent catalogue. Split from the task-lifecycle
//! handlers so each file stays focused and under the 500-line cap.
//! What: `GET /api/projects`, `POST /api/projects`, `GET /api/projects/:name`,
//! `GET /api/agents`, and `GET /api/sessions`, plus the on-disk readers they
//! delegate to.
//! Test: `list_projects_*`, `connect_project_*`, `get_project_config_*`,
//! `list_agents_*`, `list_sessions_*` in `super::tests`.

use std::collections::HashMap;

use axum::{
    Json,
    extract::{Query, State},
};
use serde::{Deserialize, Serialize};

use super::handlers::projects_config_dir;
use super::state::AppState;
use crate::registry::{ProjectEntry, ProjectRegistry, discover_active_projects};
use crate::tm::project::TmProject;

/// Session summary for the projects API response. (#341)
///
/// Why: Lighter shape than the internal `SessionSummary`/`AdapterType`
/// structs — keeps the wire format flat and stable for the WebUI.
/// What: Mirrors the `name`, `adapter_type`, and `status` fields from
/// `crate::tm::project::SessionSummary` as plain strings.
/// Test: `list_projects_returns_empty_when_registry_unavailable` (smoke).
#[derive(Debug, Clone, Serialize)]
pub(super) struct ProjectSessionSummary {
    name: String,
    adapter_type: String,
    status: String,
}

/// Response body shape for `GET /api/projects`. (#341)
///
/// Why: Joins the global `ProjectRegistry` (lifecycle + git/issue counts)
/// with the TM session registry (framework detection + live tmux sessions)
/// into a single record the UI can render without cross-referencing two
/// endpoints.
/// What: Each entry carries an id (the registry path string), human name,
/// path, optional git origin and issue/PR counts, an ISO-8601
/// `last_active` timestamp, optional framework label, and the list of
/// associated sessions.
/// Test: `list_projects_returns_array` exercises the empty/no-registry path.
#[derive(Debug, Clone, Serialize)]
pub(super) struct ProjectResponse {
    id: String,
    name: String,
    path: String,
    git_origin: Option<String>,
    last_active: Option<String>,
    open_issues_count: Option<u32>,
    open_prs_count: Option<u32>,
    framework: Option<String>,
    sessions: Vec<ProjectSessionSummary>,
}

/// `GET /api/projects` — list known projects with session + GitHub metadata. (#341)
///
/// Why: The WebUI needs a single endpoint that surfaces every project the
/// user has touched, with enough context (origin, issue/PR counts, live
/// sessions) to navigate without shelling out. We default to "active" (last
/// 14 days OR has a tmux session) so the list stays focused; `?all=true`
/// returns every registry entry for power users / debugging.
/// What: Loads `ProjectRegistry` (best-effort; empty list on failure), reads
/// the TM project list from `.trusty-agents/state/tm_sessions.json` directly
/// (we don't currently hold a `TmManager` in `AppState`), filters via
/// `discover_active_projects` unless `all=true`, and joins by canonical
/// path to attach framework + sessions.
/// Test: `list_projects_returns_array` — smoke-test the empty path.
pub(super) async fn list_projects(
    State(_state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Json<Vec<ProjectResponse>> {
    let show_all = matches!(
        params.get("all").map(String::as_str),
        Some("true") | Some("1") | Some("yes")
    );

    // Load registry entries; on any error, fall back to an empty list so the
    // UI renders a graceful empty state instead of a 500.
    let mut registry_entries: Vec<ProjectEntry> = match ProjectRegistry::new() {
        Ok(reg) => match reg.load().await {
            Ok(map) => map.into_values().collect(),
            Err(e) => {
                tracing::debug!(error = %e, "list_projects: registry load failed");
                Vec::new()
            }
        },
        Err(e) => {
            tracing::debug!(error = %e, "list_projects: registry init failed");
            Vec::new()
        }
    };

    // #465: Merge in per-project TOML configs (`.trusty-agents/projects/*.toml`)
    // for any project paths not already in the global registry. Projects
    // created via the REPL `/connect` slash command land only in the TOML
    // store; without this merge they vanish from `GET /api/projects` after
    // a restart even though they are valid registrations.
    match crate::tm::ProjectConfigStore::open(&projects_config_dir()) {
        Ok(store) => match store.list() {
            Ok(toml_configs) => {
                let known_paths: std::collections::HashSet<std::path::PathBuf> =
                    registry_entries.iter().map(|e| e.path.clone()).collect();
                for cfg in toml_configs {
                    if known_paths.contains(&cfg.project.path) {
                        continue;
                    }
                    registry_entries.push(ProjectEntry {
                        path: cfg.project.path.clone(),
                        name: cfg.project.name.clone(),
                        last_run: None,
                        status: crate::registry::ProjectStatus::Active,
                        last_connected: None,
                        pm_count: 0,
                        is_self: false,
                        git_origin: None,
                        open_issues_count: None,
                        open_prs_count: None,
                    });
                }
            }
            Err(e) => {
                tracing::debug!(error = %e, "list_projects: TOML store list failed");
            }
        },
        Err(e) => {
            tracing::debug!(error = %e, "list_projects: TOML store open failed");
        }
    }

    // Load TM projects from the on-disk JSON. Best-effort; missing/corrupt
    // file → empty.
    let tm_projects: Vec<TmProject> = load_tm_projects_from_disk().await;
    let tm_session_paths: Vec<std::path::PathBuf> =
        tm_projects.iter().map(|p| p.path.clone()).collect();

    // Filter to active (14 days OR has tmux session) unless ?all=true.
    // Always exclude temp directories regardless of the show_all flag.
    let filtered: Vec<ProjectEntry> = if show_all {
        let mut v: Vec<ProjectEntry> = registry_entries
            .into_iter()
            .filter(|e| e.is_real_project())
            .collect();
        v.sort_by_key(|b| std::cmp::Reverse(b.last_active()));
        v
    } else {
        let window = chrono::Duration::days(14);
        // discover_active_projects already applies is_real_project filtering.
        discover_active_projects(&registry_entries, &tm_session_paths, window)
            .into_iter()
            .cloned()
            .collect()
    };

    // Build the response, joining TM data by canonical path.
    let out: Vec<ProjectResponse> = filtered
        .into_iter()
        .map(|entry| {
            let path_str = entry.path.to_string_lossy().to_string();
            let tm_match = tm_projects.iter().find(|tp| tp.path == entry.path);
            let framework = tm_match.and_then(|tp| {
                if tp.framework.is_known() {
                    Some(tp.framework.display())
                } else {
                    None
                }
            });
            let sessions: Vec<ProjectSessionSummary> = tm_match
                .map(|tp| {
                    tp.sessions
                        .iter()
                        .map(|s| ProjectSessionSummary {
                            name: s.name.clone(),
                            adapter_type: s.adapter_type.as_str().to_string(),
                            status: s.status.to_string(),
                        })
                        .collect()
                })
                .unwrap_or_default();
            ProjectResponse {
                id: path_str.clone(),
                name: entry.name.clone(),
                path: path_str,
                git_origin: entry.git_origin.clone(),
                last_active: entry
                    .last_active()
                    .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
                open_issues_count: entry.open_issues_count,
                open_prs_count: entry.open_prs_count,
                framework,
                sessions,
            }
        })
        .collect();

    Json(out)
}

/// Load TM projects from `.trusty-agents/state/tm_sessions.json` directly.
///
/// Why: `AppState` does not hold a `TmManager`; rather than threading one
/// through, we read the same on-disk JSON the manager owns. This is the
/// single source of truth so the snapshot can never disagree.
/// What: Reads the file, deserializes the envelope, returns the projects
/// vector. Any I/O or parse error yields an empty list (logged at debug).
/// Test: `load_tm_projects_from_disk_returns_empty_on_missing_file`.
async fn load_tm_projects_from_disk() -> Vec<TmProject> {
    #[derive(Deserialize)]
    struct Envelope {
        #[serde(default)]
        projects: Vec<TmProject>,
    }
    let path = std::path::PathBuf::from(".trusty-agents/state/tm_sessions.json");
    let bytes = match tokio::fs::read(&path).await {
        Ok(b) => b,
        Err(e) => {
            tracing::debug!(error = %e, "tm_sessions.json: read failed");
            return Vec::new();
        }
    };
    match serde_json::from_slice::<Envelope>(&bytes) {
        Ok(env) => env.projects,
        Err(e) => {
            tracing::debug!(error = %e, "tm_sessions.json: parse failed");
            Vec::new()
        }
    }
}

/// `GET /api/agents` (#407) — list discovered agents.
///
/// Why: The web UI and `om` CLI need to display which agent personas are
/// available without spawning a sub-agent. Reading TOML directly avoids
/// pulling the full `AgentRegistry` machinery into the request path. Wrapped
/// in a `{"agents": [...]}` envelope so future fields (e.g. pagination,
/// catalog version) can be added without breaking clients.
/// What: Scans `.trusty-agents/agents/*.toml` (relative to cwd) via
/// `scan_agents_dir` and returns the `{"agents": [...]}` envelope.
/// Test: `list_agents_returns_agents_envelope`.
pub(super) async fn list_agents_route(State(_state): State<AppState>) -> Json<serde_json::Value> {
    Json(
        serde_json::json!({ "agents": scan_agents_dir(&std::path::PathBuf::from(".trusty-agents/agents")).await }),
    )
}

/// Scan an agents directory and return a parsed JSON array.
///
/// Why: Extracted from `list_agents_route` so unit tests can drive it
/// against a `tempfile::TempDir` without juggling process cwd.
/// What: Reads `*.toml` files, extracts `[agent]` fields (name, role, model,
/// runner, description), sorts by name. Skips unreadable / unparseable files.
/// Test: `scan_agents_dir_parses_toml` — see tests module.
pub(super) async fn scan_agents_dir(dir: &std::path::Path) -> Vec<serde_json::Value> {
    let mut out: Vec<serde_json::Value> = Vec::new();
    let entries = match tokio::fs::read_dir(dir).await {
        Ok(e) => e,
        Err(e) => {
            tracing::debug!(?e, dir = %dir.display(), "list_agents: dir read failed");
            return out;
        }
    };
    let mut entries = entries;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("toml") {
            continue;
        }
        let raw = match tokio::fs::read_to_string(&path).await {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!(?e, path = %path.display(), "list_agents: read failed");
                continue;
            }
        };
        let parsed: toml::Value = match toml::from_str(&raw) {
            Ok(v) => v,
            Err(e) => {
                tracing::debug!(?e, path = %path.display(), "list_agents: parse failed");
                continue;
            }
        };
        let agent = parsed.get("agent");
        let get_str = |key: &str| -> Option<String> {
            agent
                .and_then(|a| a.get(key))
                .and_then(|v| v.as_str())
                .map(str::to_string)
        };
        let name = get_str("name").unwrap_or_else(|| {
            path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string()
        });
        out.push(serde_json::json!({
            "name": name,
            "role": get_str("role").unwrap_or_default(),
            "model": get_str("model").unwrap_or_default(),
            "runner": get_str("runner").unwrap_or_default(),
            "description": get_str("description").unwrap_or_default(),
        }));
    }
    out.sort_by(|a, b| {
        a.get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .cmp(b.get("name").and_then(|v| v.as_str()).unwrap_or(""))
    });
    out
}

/// Query parameters for `GET /api/sessions` (#407).
#[derive(Debug, Deserialize)]
pub(super) struct SessionsQuery {
    /// Optional path filter: when provided, only sessions whose `project`
    /// (or `path`) field matches this value are returned.
    project: Option<String>,
}

/// `GET /api/sessions` (#407) — list recorded sessions.
///
/// Why: The web UI shows a session history sidebar; it needs a stable JSON
/// endpoint instead of falling through to the SPA catch-all (which returns
/// HTML and breaks JSON parsers).
/// What: Reads `.trusty-agents/state/sessions.json` (an envelope with a
/// `sessions` array), optionally filters by `?project=<path>`, and returns
/// the array under a `sessions` key. Missing file → empty array.
/// Test: With `sessions.json` present, the route returns its `sessions`;
/// with no file, it returns `{"sessions": []}`.
pub(super) async fn list_sessions_route(
    State(_state): State<AppState>,
    Query(q): Query<SessionsQuery>,
) -> Json<serde_json::Value> {
    let path = std::path::PathBuf::from(".trusty-agents/state/sessions.json");
    Json(serde_json::json!({
        "sessions": load_sessions_from(&path, q.project.as_deref()).await,
    }))
}

/// Read sessions from a file path and apply optional project filter.
///
/// Why: Extracted from `list_sessions_route` so unit tests can drive it
/// against a `tempfile::TempDir` without changing the process-global cwd
/// (which races with sibling tests in the same binary).
/// What: Reads the file (missing → empty), parses `{"sessions": [...]}`,
/// optionally filters by `project`/`path` field equality. Errors degrade
/// to an empty list — never propagated as 500 to the route.
/// Test: `list_sessions_empty_returns_envelope`,
/// `list_sessions_filters_by_project` in the tests module.
pub(super) async fn load_sessions_from(
    path: &std::path::Path,
    project_filter: Option<&str>,
) -> Vec<serde_json::Value> {
    let bytes = match tokio::fs::read(path).await {
        Ok(b) => b,
        Err(_) => return Vec::new(),
    };
    let parsed: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!(?e, "list_sessions: parse failed");
            return Vec::new();
        }
    };
    let sessions = parsed
        .get("sessions")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if let Some(want) = project_filter {
        sessions
            .into_iter()
            .filter(|s| {
                s.get("project")
                    .or_else(|| s.get("path"))
                    .and_then(|v| v.as_str())
                    .map(|p| p == want)
                    .unwrap_or(false)
            })
            .collect()
    } else {
        sessions
    }
}
