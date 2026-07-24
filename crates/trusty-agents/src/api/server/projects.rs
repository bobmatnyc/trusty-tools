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

use std::collections::{HashMap, HashSet};

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

/// `GET /api/agents` (#407, #3741) — list discovered agents.
///
/// Why: The web UI's agent picker needs the SAME set of personas the runtime
/// actually dispatches — Assistant, Izzie, CTO Bot, and the specialist roster.
/// The pre-#3741 version scanned a SINGLE cwd-relative directory
/// (`.trusty-agents/agents`) and only globbed FLAT `*.toml` files. That broke the
/// packaged macOS `.app` two ways: (1) the Tauri sidecar runs with cwd `/`
/// (sealed APFS volume) — so `.trusty-agents/agents` resolved to `/.trusty-agents/
/// agents`, which does not exist, and the catalog came back EMPTY (the picker
/// showed only the frontend's hard-coded base "Assistant" — Bob's exact
/// "no Izzie / CTO Bot" report); and (2) `izzie`/`cto-assistant`/`assistant`
/// are shipped as directory PACKAGES (`<name>/agent.toml`), which a flat glob
/// never sees. This now enumerates the SAME candidate directories the runtime
/// loader resolves against ([`crate::agents::agents_dir_candidates`] — the
/// project-local tier PLUS the `$HOME/.trusty-agents/agents` bundle tier) and
/// resolves both packages and flat files, so the catalog matches dispatch.
/// What: Scans every [`crate::agents::agents_dir_candidates`] directory via
/// [`scan_agent_catalog`] (package + flat resolution, deduped) and returns the
/// `{"agents": [...]}` envelope.
/// Test: `scan_agent_catalog_dedupes_across_dirs`,
/// `scan_agents_dir_resolves_packages_flat_and_dedupes`.
pub(super) async fn list_agents_route(State(_state): State<AppState>) -> Json<serde_json::Value> {
    Json(agent_roster().await)
}

/// True when a parsed catalog entry's `role` field is exactly `"assistant"`.
///
/// Why (Bob directive, agent-picker role filter): the picker/roster surface
/// must show ONLY assistant-type personas (Assistant, Izzie, CTO Bot,
/// custom ones like `researcher`) — never the 16 bundled coding/engineer/
/// support agents. `parse_agent_toml`/`md_agent_to_catalog_json` already
/// default a missing/unparseable `role` to `""` (never `None`), so a plain
/// equality check against the literal `"assistant"` — with no special-casing
/// for absence — naturally treats any unparseable or missing role as
/// NOT-assistant. That is a deliberate fail-closed default: it is the only
/// way a bundled coding agent can never leak back into the picker through a
/// malformed or role-less manifest.
/// What: Reads `v["role"]` as a string and compares against `"assistant"`.
/// Test: `agent_roster_filters_to_assistant_role_only`.
pub(super) fn is_assistant_role(v: &serde_json::Value) -> bool {
    v.get("role").and_then(|r| r.as_str()) == Some("assistant")
}

/// Shared agent-roster computation — the `{"agents": [...]}` envelope returned
/// by both `GET /api/agents` ([`list_agents_route`]) and the `list_agents`
/// MCP tool ([`crate::runtime::mcp_serve`], #3633 core).
///
/// Why: the MCP `list_agents` tool must surface the EXACT same roster the HTTP
/// route (and therefore the GUI picker) shows — Assistant, Izzie, CTO Bot, and
/// custom assistant-role personas resolved from
/// [`crate::agents::agents_dir_candidates`]. Extracting the computation out of
/// the axum handler into one plain async fn keeps the two surfaces from
/// drifting, exactly as [`parse_agent_toml`] keeps the GET and PATCH shapes
/// aligned. Per Bob's directive this is a PICKER surface, so it is filtered to
/// `role == "assistant"` — the 16 bundled coding/engineer/support agents must
/// never appear here. This does NOT affect delegation: `delegate_to_agent`
/// (`crate::tools::delegate`) resolves its target directly via
/// `AgentConfig::by_name_in` against the candidate directories, never through
/// this roster, so hidden-but-functional agents (ctrl, pm, coding agents)
/// remain fully dispatchable by name even though they no longer list here.
/// [`scan_agent_catalog`] itself stays UNFILTERED (and is exercised directly
/// by its own raw-scan tests) — the role filter is applied only at this
/// picker-facing envelope boundary, so a future consumer that legitimately
/// needs the full catalog can call `scan_agent_catalog` directly instead of
/// this fn.
/// What: scans every candidate agents directory via [`scan_agent_catalog`]
/// (package + flat resolution, deduped, name-sorted), filters to
/// [`is_assistant_role`], and wraps the result in the `{"agents": [...]}`
/// envelope. Read-only; no side effects.
/// Test: covered indirectly by `scan_agent_catalog_dedupes_across_dirs` (the
/// scan) and the `mcp_serve` dispatcher's `tools/call` unit test (the
/// envelope); role filter itself by
/// `agent_roster_filters_to_assistant_role_only`.
pub(crate) async fn agent_roster() -> serde_json::Value {
    let dirs = crate::agents::agents_dir_candidates();
    let agents: Vec<serde_json::Value> = scan_agent_catalog(&dirs)
        .await
        .into_iter()
        .filter(is_assistant_role)
        .collect();
    serde_json::json!({ "agents": agents })
}

/// Parse one agent TOML file's `[agent]` table into the JSON shape shared by
/// `GET /api/agents` and `PATCH /api/agents/:name` (#3246).
///
/// Why: Extracted from `scan_agents_dir` so the PATCH handler
/// (`super::agent_patch`) can return the freshly-written agent through the
/// exact same field-extraction logic instead of duplicating it — keeping the
/// two routes' response shapes from drifting apart.
/// What: Parses `raw` as TOML; reads `[agent]`'s `name` (falling back to
/// `fallback_name`, typically the file stem), `role`, `model`, `runner`,
/// `provider_id`, and `description` (each defaulting to `""` when absent),
/// plus `display_name` — which, per the shared cross-PR contract (#3740),
/// is NEVER empty: a blank/absent/whitespace-only `display_name` falls back
/// to the agent `name` rather than `""`. Returns `None` when `raw` fails to
/// parse as TOML.
/// Test: `scan_agents_dir_parses_toml`, `patch_agent_persists_model_and_round_trips`,
/// `parse_agent_toml_surfaces_display_name`, `scan_agents_dir_exposes_display_name`.
pub(super) fn parse_agent_toml(raw: &str, fallback_name: &str) -> Option<serde_json::Value> {
    let parsed: toml::Value = toml::from_str(raw).ok()?;
    let agent = parsed.get("agent");
    let get_str = |key: &str| -> Option<String> {
        agent
            .and_then(|a| a.get(key))
            .and_then(|v| v.as_str())
            .map(str::to_string)
    };
    let name = get_str("name").unwrap_or_else(|| fallback_name.to_string());
    // #3738/#3739: surface `display_name` ("Assistant" / "Izzie" / "CTO Bot")
    // so the GUI's per-message speaker attribution reads the persona's
    // human-facing label straight from the catalog. Per the cross-PR contract
    // (#3740) it is NEVER empty: falls back to `name` (matching
    // `AgentInfo::display_label`'s contract) — and the `.filter(!empty)` guard
    // (#3739) additionally treats a blank/whitespace-only value (e.g. a
    // hand-edited manifest) as absent, so a consumer can always render SOME
    // non-empty label without re-deriving the mapping client-side.
    let display_name = get_str("display_name")
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| name.clone());
    Some(serde_json::json!({
        "name": name,
        "role": get_str("role").unwrap_or_default(),
        "model": get_str("model").unwrap_or_default(),
        "runner": get_str("runner").unwrap_or_default(),
        "provider_id": get_str("provider_id").unwrap_or_default(),
        "description": get_str("description").unwrap_or_default(),
        "display_name": display_name,
    }))
}

/// Sort a parsed-agent array in place by the `name` field (stable catalog order).
fn sort_agents_by_name(agents: &mut [serde_json::Value]) {
    agents.sort_by(|a, b| {
        a.get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .cmp(b.get("name").and_then(|v| v.as_str()).unwrap_or(""))
    });
}

// Per-directory resolution tiers, matching
// `AgentConfig::by_name_unresolved_src_in`'s order (directory package > flat
// `.toml` > flat `.md`). Higher rank wins for the same name within a dir.
const RANK_MD: u8 = 0;
const RANK_TOML: u8 = 1;
const RANK_PACKAGE: u8 = 2;

/// The dispatch `name` from a parsed catalog entry, falling back to `fallback`.
fn catalog_entry_name(v: &serde_json::Value, fallback: &str) -> String {
    v.get("name")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(fallback)
        .to_string()
}

/// Project a flat-`.md` overlay's resolved `AgentConfig` into the same catalog
/// JSON shape [`parse_agent_toml`] emits (#3745, critic HIGH-2).
///
/// Why: flat `<name>.md` files are the PRIMARY documented `extends:`
/// personalization surface (`loader.rs::by_name_unresolved_src_in` tier 3), so
/// the picker must surface them — otherwise a user's `.md` overlay dispatches
/// fine but is invisible in (and shadowed by) the catalog. Parsing through the
/// SAME `parse_md_agent` the registry/loader uses keeps the surfaced
/// name/role/model/display_name identical to what actually dispatches.
/// What: reads `agent.{name,role,model,description,display_name,runner}` off
/// the parsed config; `display_name` honors the never-empty contract (falls
/// back to `name`); `provider_id` is `""` (`.md` frontmatter has no such key).
fn md_agent_to_catalog_json(
    cfg: &crate::agents::AgentConfig,
    fallback_name: &str,
) -> serde_json::Value {
    let a = &cfg.agent;
    let name = if a.name.trim().is_empty() {
        fallback_name.to_string()
    } else {
        a.name.clone()
    };
    let display_name = a
        .display_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| name.clone());
    let runner = match a.runner {
        crate::agents::RunnerKind::ClaudeCode => "claude-code",
        crate::agents::RunnerKind::Inline => "inline",
        crate::agents::RunnerKind::InProcess => "in-process",
        crate::agents::RunnerKind::Subprocess => "subprocess",
    };
    serde_json::json!({
        "name": name,
        "role": a.role.clone(),
        "model": a.model.clone(),
        "runner": runner,
        "provider_id": "",
        "description": a.description.clone(),
        "display_name": display_name,
    })
}

/// Insert `v` for `name` at tier `rank`, keeping the higher tier on conflict
/// (equal rank → last write wins, matching a flat glob's duplicate handling).
fn consider_entry(
    by_name: &mut HashMap<String, (u8, serde_json::Value)>,
    name: String,
    rank: u8,
    v: serde_json::Value,
) {
    match by_name.get(&name) {
        Some((existing, _)) if *existing > rank => {}
        _ => {
            by_name.insert(name, (rank, v));
        }
    }
}

/// Scan ONE agents directory across all three resolution tiers, returning the
/// resolved entries plus the set of names BLOCKED by an incomplete package
/// (#3745, critic HIGH-1/HIGH-2).
///
/// Why: the catalog must reflect what the runtime can actually dispatch.
/// `load_agent_package` REQUIRES both `agent.toml` AND `persona.md` and
/// HARD-FAILS `by_name` if `persona.md` is missing (it never falls through to
/// a flat file) — so an `agent.toml`-only package is not just non-dispatchable
/// via the package tier, it makes that whole name unresolvable in the
/// directory. Surfacing it (or a same-named flat file it shadows) would make a
/// broken agent picker-selectable. This mirrors the runtime: a complete
/// package resolves; an incomplete one BLOCKS the name; only when no package
/// dir exists do flat `.toml` then flat `.md` apply.
/// What: skips hidden/`*.stale.bak` entries; uses `tokio::fs::metadata`
/// (follows symlinks, matching `Path::is_dir`) for the package-dir check; a
/// directory with `agent.toml`+`persona.md` is a package (rank 2), one with
/// `agent.toml` but no `persona.md` is BLOCKED; flat `<name>.toml` is rank 1
/// and flat `<name>.md` (via [`crate::agents::registry::parse_md_agent`]) is
/// rank 0. Higher tier wins per name; blocked names are dropped from the
/// output. Returns `(entries, blocked_names)`.
/// Test: `scan_agents_dir_resolves_packages_flat_and_dedupes`,
/// `scan_agents_dir_requires_persona_md_for_package`,
/// `scan_agents_dir_surfaces_flat_md_overlay`.
async fn scan_agents_dir_tiered(
    dir: &std::path::Path,
) -> (Vec<serde_json::Value>, HashSet<String>) {
    let mut by_name: HashMap<String, (u8, serde_json::Value)> = HashMap::new();
    let mut blocked: HashSet<String> = HashSet::new();
    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(e) => e,
        Err(e) => {
            tracing::debug!(?e, dir = %dir.display(), "list_agents: dir read failed");
            return (Vec::new(), blocked);
        }
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        let Some(entry_name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        // Skip hidden entries (`.bundled-stamp`, …) and archived backups
        // (`*.stale.bak`, produced by the bundled reprovision) so they never
        // enter the catalog or shadow a live agent of the same name.
        if entry_name.starts_with('.')
            || entry_name.contains(".stale.bak")
            || entry_name.ends_with(".bak")
        {
            continue;
        }
        // `metadata` follows symlinks, matching `Path::is_dir` used by the
        // runtime's `load_agent_package` (critic MEDIUM-4).
        let Ok(meta) = tokio::fs::metadata(&path).await else {
            continue;
        };
        if meta.is_dir() {
            // Directory package: `<dir>/agent.toml` is the dispatch key.
            let manifest = path.join("agent.toml");
            if tokio::fs::metadata(&manifest).await.is_err() {
                continue; // an ordinary subdir, not an agent package
            }
            // Require `persona.md` — `load_agent_package` hard-fails without it,
            // so an `agent.toml`-only package is broken. Block the name so a
            // same-named flat file (here or in a lower-priority dir) can't make
            // it falsely picker-selectable.
            if tokio::fs::metadata(&path.join("persona.md")).await.is_err() {
                blocked.insert(entry_name.to_string());
                continue;
            }
            let Ok(raw) = tokio::fs::read_to_string(&manifest).await else {
                continue;
            };
            if let Some(v) = parse_agent_toml(&raw, entry_name) {
                let name = catalog_entry_name(&v, entry_name);
                consider_entry(&mut by_name, name, RANK_PACKAGE, v);
            }
        } else {
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string();
            match path.extension().and_then(|s| s.to_str()) {
                Some("toml") => match tokio::fs::read_to_string(&path).await {
                    Ok(raw) => {
                        if let Some(v) = parse_agent_toml(&raw, &stem) {
                            let name = catalog_entry_name(&v, &stem);
                            consider_entry(&mut by_name, name, RANK_TOML, v);
                        }
                    }
                    Err(e) => {
                        tracing::debug!(?e, path = %path.display(), "list_agents: read failed")
                    }
                },
                Some("md") => match crate::agents::registry::parse_md_agent(&path) {
                    Ok(cfg) => {
                        let v = md_agent_to_catalog_json(&cfg, &stem);
                        let name = catalog_entry_name(&v, &stem);
                        consider_entry(&mut by_name, name, RANK_MD, v);
                    }
                    Err(e) => {
                        tracing::debug!(?e, path = %path.display(), "list_agents: md parse failed")
                    }
                },
                _ => {}
            }
        }
    }
    // Drop any name that also had an incomplete package in this directory.
    for b in &blocked {
        by_name.remove(b);
    }
    let out = by_name.into_values().map(|(_, v)| v).collect();
    (out, blocked)
}

/// Scan ONE agents directory (all tiers) and return a name-sorted JSON array.
///
/// Why: single-directory entry point for the unit tests that drive the
/// tier/dedupe/persona-md logic directly against a `tempfile::TempDir`. Wraps
/// [`scan_agents_dir_tiered`] and drops the blocked-name set (relevant only to
/// the cross-directory merge in [`scan_agent_catalog`], which production uses).
/// `#[cfg(test)]` because the production route (`list_agents_route`) goes
/// through [`scan_agent_catalog`]; this wrapper has no non-test caller.
/// Test: `scan_agents_dir_parses_toml`,
/// `scan_agents_dir_resolves_packages_flat_and_dedupes`,
/// `scan_agents_dir_requires_persona_md_for_package`,
/// `scan_agents_dir_surfaces_flat_md_overlay`.
#[cfg(test)]
pub(super) async fn scan_agents_dir(dir: &std::path::Path) -> Vec<serde_json::Value> {
    let (mut out, _blocked) = scan_agents_dir_tiered(dir).await;
    sort_agents_by_name(&mut out);
    out
}

/// Scan MULTIPLE candidate agent directories in priority order and return one
/// deduplicated, name-sorted catalog (#3741, #3745).
///
/// Why: `list_agents_route` must present the same roster the runtime resolves,
/// which spans [`crate::agents::agents_dir_candidates`] — the project-local
/// tier (`TAGENT_CONFIG_DIR` / cwd `.trusty-agents/agents`) AND the
/// `$HOME/.trusty-agents/agents` bundle tier. Scanning only the first missed
/// every bundled persona whenever the process cwd had no project tier (the
/// packaged `.app`, cwd `/`). The `$HOME` tier is where
/// `ensure_bundled_agents_deployed` writes Assistant / Izzie / CTO Bot.
/// What: Scans each directory via [`scan_agents_dir_tiered`], merging by `name`
/// with the FIRST directory to define a name winning — matching
/// [`crate::agents::agents_dir_candidates`]'s documented precedence. An
/// incomplete package in a higher-priority directory BLOCKS that name from ALL
/// directories (the runtime `by_name` hard-errors on it and never reaches the
/// lower tier), so a broken package can never be papered over by a lower-tier
/// flat file — mirroring dispatch exactly.
/// Test: `scan_agent_catalog_dedupes_across_dirs`.
pub(super) async fn scan_agent_catalog(dirs: &[std::path::PathBuf]) -> Vec<serde_json::Value> {
    let mut by_name: HashMap<String, serde_json::Value> = HashMap::new();
    let mut blocked: HashSet<String> = HashSet::new();
    for dir in dirs {
        let (entries, dir_blocked) = scan_agents_dir_tiered(dir).await;
        blocked.extend(dir_blocked);
        for v in entries {
            let name = catalog_entry_name(&v, "");
            if name.is_empty() || blocked.contains(&name) {
                continue;
            }
            by_name.entry(name).or_insert(v);
        }
    }
    let mut out: Vec<serde_json::Value> = by_name.into_values().collect();
    sort_agents_by_name(&mut out);
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
