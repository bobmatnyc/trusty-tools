//! Project registration + per-project config lookup (#405, #451, #465).
//!
//! Why: `POST /api/projects` (CLI `om connect` + WebUI "Add Project") and
//! `GET /api/projects/:name` both touch the global registry and the
//! per-project TOML store. Keeping them together — and apart from the
//! read-only listing endpoints — keeps each file focused and under the cap.
//! What: `connect_project` registers a path (optionally materializing the TOML
//! config) and `get_project_config` resolves one project's config with a
//! registry fallback.
//! Test: `connect_project_persists_to_registry`,
//! `get_project_config_*` in `super::tests`.

use axum::{Json, extract::Path, extract::State, http::StatusCode};
use serde::Deserialize;

use super::handlers::projects_config_dir;
use super::state::AppState;
use crate::registry::ProjectRegistry;

/// Request body for `POST /api/projects` (#405, extended in #451).
///
/// Why: The WebUI "Add Project" form posts `{path, adapter?, name?}`. `adapter`
/// is optional purely for backward compatibility with the original #405
/// callers (`om connect <path>`); when supplied, we route through
/// `ProjectConfigStore::find_or_create` so the on-disk shape matches `/connect`.
/// What: `path` required; `adapter`/`name` optional.
/// Test: `connect_project_persists_to_registry`.
#[derive(Debug, Deserialize)]
pub(super) struct ConnectProjectRequest {
    path: String,
    #[serde(default)]
    adapter: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

/// `POST /api/projects` — register a project directory with the server.
///
/// Why: Two clients call this: `om connect <path>` (#405, no adapter) and the
/// WebUI "Add Project" form (#451, supplies adapter). Both want the project to
/// show up in `GET /api/projects` afterwards; the WebUI additionally wants the
/// per-project TOML under `.trusty-agents/projects/<name>.toml` so subsequent
/// `tell <project>` calls and `/connect <path> <adapter>` invocations resolve.
/// Centralising both flows here means the on-disk shape can't drift between
/// CLI and HTTP entry points.
/// What:
///   1. Validate and canonicalize `path`.
///   2. Register in the global `~/.trusty-agents/projects.json` registry so
///      `GET /api/projects` sees it.
///   3. If `adapter` is supplied, also call `ProjectConfigStore::find_or_create`
///      to materialize `.trusty-agents/projects/<name>.toml` and return
///      `{name, path, default_harness, created}`.
///   4. If `adapter` is omitted, return the legacy `{id, path, name, created_at}`
///      shape for backward compatibility with #405 callers.
/// Test: `connect_project_persists_to_registry` (legacy shape) and
/// `connect_project_creates_tm_config_when_adapter_supplied` (new shape).
pub(super) async fn connect_project(
    State(_state): State<AppState>,
    Json(req): Json<ConnectProjectRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let path = std::path::PathBuf::from(&req.path);
    if !path.exists() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let abs = path.canonicalize().unwrap_or(path);

    // Persist to the same registry GET reads from. Failures here surface
    // as 500 — the client expects success to mean "the project is now
    // visible to GET /api/projects", so silently swallowing would
    // reproduce the original bug.
    let registry = ProjectRegistry::new().map_err(|e| {
        tracing::warn!(error = %e, "connect_project: ProjectRegistry::new failed");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    registry.register_pm_start(&abs).await.map_err(|e| {
        tracing::warn!(error = %e, "connect_project: register_pm_start failed");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // #451: New WebUI form supplies `adapter`; materialize the per-project
    // TOML so subsequent `tell`/`/connect` calls have something to resolve.
    if let Some(adapter) = req.adapter.as_deref() {
        let projects_dir = abs.join(".trusty-agents").join("projects");
        let store = crate::tm::ProjectConfigStore::open(&projects_dir).map_err(|e| {
            tracing::warn!(error = %e, "connect_project: ProjectConfigStore::open failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
        // Detect create-vs-reuse: a pre-existing entry at this path means we
        // are reusing, not creating. The find_by_path probe is cheap and
        // avoids a save when the config already exists.
        let pre_existing = store.find_by_path(&abs).map_err(|e| {
            tracing::warn!(error = %e, "connect_project: find_by_path failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
        let cfg = store
            .find_or_create(&abs, adapter, req.name.as_deref())
            .map_err(|e| {
                tracing::warn!(error = %e, "connect_project: find_or_create failed");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
        return Ok(Json(serde_json::json!({
            "name": cfg.project.name,
            "path": cfg.project.path.to_string_lossy(),
            "default_harness": cfg.project.default_harness,
            "created": pre_existing.is_none(),
        })));
    }

    // Legacy #405 shape — no adapter, no TM config.
    let name = req.name.unwrap_or_else(|| {
        abs.file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| abs.to_string_lossy().to_string())
    });
    let id = abs.to_string_lossy().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    Ok(Json(serde_json::json!({
        "id": id,
        "path": abs.to_string_lossy(),
        "name": name,
        "created_at": created_at,
    })))
}

/// `GET /api/projects/:name` (#451) — return one per-project TOML config.
///
/// Why: The WebUI project detail view needs to render the harness list and
/// default harness for a single project without scanning all of them. Reading
/// the matching `.trusty-agents/projects/<name>.toml` is the source of truth.
/// What: Loads the config via `ProjectConfigStore::load`. 404 when the named
/// project has no TOML; 500 on filesystem/parse errors. Returns the raw
/// `ProjectConfig` JSON (serde already round-trips it).
/// Test: `get_project_config_returns_404_when_missing` and
/// `get_project_config_returns_existing`.
pub(super) async fn get_project_config(
    State(_state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let store = crate::tm::ProjectConfigStore::open(&projects_config_dir()).map_err(|e| {
        tracing::warn!(error = %e, "get_project_config: open failed");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    match store.load(&name) {
        Ok(Some(cfg)) => Ok(Json(serde_json::to_value(&cfg).map_err(|e| {
            tracing::warn!(error = %e, "get_project_config: serialize failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?)),
        Ok(None) => {
            // #465: Fall back to the global `ProjectRegistry`. Projects added
            // via `POST /api/projects` without an `adapter` only land in
            // `~/.trusty-agents/projects.json`; without this fallback they appear
            // in `GET /api/projects` but 404 on the detail endpoint after a
            // restart. We match by entry name OR by path basename so both
            // `name`-keyed and dir-basename-keyed lookups resolve. Returns a
            // minimal `ProjectConfig` shape (no harnesses, no default).
            let registry = match ProjectRegistry::new() {
                Ok(r) => r,
                Err(e) => {
                    tracing::debug!(error = %e, "get_project_config: registry init failed");
                    return Err(StatusCode::NOT_FOUND);
                }
            };
            let entries = match registry.load().await {
                Ok(map) => map,
                Err(e) => {
                    tracing::debug!(error = %e, "get_project_config: registry load failed");
                    return Err(StatusCode::NOT_FOUND);
                }
            };
            let matched = entries.into_values().find(|e| {
                e.name == name
                    || e.path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .map(|b| b == name)
                        .unwrap_or(false)
            });
            match matched {
                Some(entry) => {
                    let cfg = crate::tm::ProjectConfig {
                        project: crate::tm::ProjectMeta {
                            name: entry.name.clone(),
                            path: entry.path.clone(),
                            default_harness: None,
                        },
                        harnesses: Vec::new(),
                    };
                    Ok(Json(serde_json::to_value(&cfg).map_err(|e| {
                        tracing::warn!(error = %e, "get_project_config: serialize failed");
                        StatusCode::INTERNAL_SERVER_ERROR
                    })?))
                }
                None => Err(StatusCode::NOT_FOUND),
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "get_project_config: load failed");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}
