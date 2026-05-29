//! Core task-lifecycle + docs HTTP handlers (#151, #187, #371).
//!
//! Why: These handlers form the primary task-submission REST surface the
//! WebUI / `om` CLI consume. Project/session/agent listing lives in
//! `super::projects`; CTRL sessions, tmux, SSE, and subprocess execution live
//! in their own focused sibling modules.
//! What: `POST /api/task`, `GET /api/task/:id`, `GET /api/tasks`,
//! `POST /api/clear-context`, `GET /api/health`, `GET /api/docs/search`, plus
//! the recap retrieval route and the request/response body structs they use.
//! Test: `super::tests` drives every route end-to-end via the axum router.

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};

use super::state::{AppState, state_dir};
use super::task_runner::{maybe_emit_recap, run_task};
use crate::api::types::{PmResponse, PmStatus};
use crate::events::{self, Event};
use crate::recap;

/// Request body for `POST /api/task`.
///
/// Why: Carries the user task text plus optional workflow/agent/output knobs
/// the WebUI and Tauri GUI set per submission.
/// What: All optional fields default to `None`; `task` is required.
/// Test: `submit_task_returns_running` (integration) + serde round-trip.
#[derive(Debug, Clone, Deserialize)]
pub struct TaskRequest {
    pub task: String,
    #[serde(default)]
    pub workflow: Option<String>,
    #[serde(default)]
    pub out_dir: Option<String>,
    #[serde(default)]
    pub task_file: Option<String>,
    /// #151 phase-4: when set, dispatch to a single sub-agent instead of a
    /// full workflow (via `open-mpm --direct <agent>`).
    #[serde(default)]
    pub agent: Option<String>,
    /// Tauri GUI: when set, run the spawned `open-mpm` subprocess with this
    /// directory as its working directory so project-scoped PMs can operate
    /// on a specific project without the caller having to `cd` first.
    ///
    /// Why: The desktop chat interface allows users to register multiple
    /// project paths and chat with a per-project PM; each task must execute
    /// in that project's root so `.open-mpm/`, file paths, and shell tools
    /// resolve relative to the correct codebase.
    /// What: Optional absolute path. When present and pointing at a directory,
    /// `run_task` sets it as the child process's `current_dir`.
    /// Test: Submit a task with `project_path: "/tmp"`, assert the spawned
    /// subprocess inherits that cwd (observable via child tracing or stdout
    /// from a task that prints `std::env::current_dir()`).
    #[serde(default)]
    pub project_path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct TaskSubmittedBody {
    id: String,
    status: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct HealthBody {
    status: &'static str,
    version: &'static str,
}

/// Directory under `.open-mpm/` holding per-project TOML configs.
///
/// Why: `tell` routing and project lookups need to load `<project>.toml`.
/// Centralizing the path here so tests/CLI/tm handlers agree on the layout.
/// What: Returns `.open-mpm/projects`.
/// Test: Indirectly via `get_project_config_*` and tm `tell` tests.
pub(super) fn projects_config_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(".open-mpm/projects")
}

/// `GET /api/health` — liveness + version probe.
pub(super) async fn health() -> Json<HealthBody> {
    Json(HealthBody {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
    })
}

/// `POST /api/task` — kick off a workflow/agent/conversational run.
///
/// Why: Single entry point the WebUI / CLI hit to submit work; the server
/// classifies intent and routes to the cheapest viable execution path.
/// What: Stores a `running` placeholder, announces the session on the event
/// bus, classifies intent (with a CTRL-command short-circuit), then spawns a
/// background future on the appropriate path, returning `202 {id, running}`.
/// Test: `submit_task_returns_running` (integration); intent routing covered
/// by `crate::intent` unit tests.
pub(super) async fn submit_task(
    State(state): State<AppState>,
    Json(req): Json<TaskRequest>,
) -> impl IntoResponse {
    let id = uuid::Uuid::new_v4().to_string();
    let placeholder = PmResponse::running(&id);
    state.upsert(id.clone(), placeholder).await;

    // #192 Phase B: announce the new session immediately on the event bus so
    // SSE subscribers (Sidebar task list, ChatView session bootstrap) update
    // before the child subprocess has even spawned.
    let project = req
        .project_path
        .clone()
        .unwrap_or_else(|| "(default)".to_string());
    events::publish(Event::SessionStarted {
        session_id: id.clone(),
        project,
    });

    // #199 / #203: Intent-based workflow inference. Three routes:
    //   - Conversational ("Hello", "Thanks") — in-process, no tools (~1-3s).
    //   - Research ("explain X", "what does Y do") — in-process tool-armed
    //     PM loop (`run_pm_task_with_session` falls through past its own
    //     Conversational fast-path since the input is Research, not
    //     Conversational). Lets `delegate_to_agent` fire when needed without
    //     paying for the prescriptive subprocess pipeline.
    //   - Implementation ("fix X", "build Y", slash commands) — full
    //     subprocess prescriptive workflow (~60-90s).
    use crate::intent::{IntentClass, classify_intent};

    // #208: CTRL management commands must short-circuit before intent
    // classification. Verbs like "add" and "remove" are in ACTION_VERBS
    // (correctly — "add authentication" is Implementation), but
    // "add project /path" needs CTRL's in-process tool registry
    // (AddProjectTool, RemoveProjectTool, …) which only exists in
    // `run_pm_task_with_session`. Routing these to the prescriptive
    // subprocess pipeline would lose access to those tools.
    let normalized = req.task.trim().to_lowercase();
    let is_ctrl_command = normalized.starts_with("add project ")
        || normalized.starts_with("remove project ")
        || normalized.starts_with("stop task ")
        || normalized.starts_with("set active ")
        || normalized == "list projects"
        || normalized == "list tasks";

    let intent = if is_ctrl_command {
        // Force the in-process Research path so CTRL tools are available.
        IntentClass::Research
    } else {
        classify_intent(&req.task)
    };

    match intent {
        IntentClass::Conversational | IntentClass::Research => {
            // Both run in-process via run_pm_task_with_session. The function
            // re-classifies internally: Conversational hits the no-tools fast
            // path; Research falls through to the tool-armed PM loop.
            let state_bg = state.clone();
            let id_bg = id.clone();
            let task_text = req.task.clone();
            let project_path = req
                .project_path
                .clone()
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
            let intent_label = match intent {
                IntentClass::Conversational => "conversational",
                IntentClass::Research => "research",
                IntentClass::Implementation => "implementation",
            };

            tokio::spawn(async move {
                let result = crate::ctrl::run_pm_task_with_session(
                    &project_path,
                    &task_text,
                    Some(id_bg.clone()),
                )
                .await;

                let resp = match result {
                    Ok(content) => {
                        let mut r = PmResponse::running(&id_bg);
                        r.response_type = crate::api::types::PmResponseType::AgentResponse;
                        r.status = PmStatus::Success;
                        r.narrative = content;
                        r
                    }
                    Err(e) => {
                        PmResponse::error(&id_bg, format!("{intent_label} handler failed: {e:#}"))
                    }
                };

                let status_str = match resp.status {
                    PmStatus::Success => "success",
                    PmStatus::Failed => "error",
                    PmStatus::Partial => "partial",
                    PmStatus::Running => "running",
                }
                .to_string();
                state_bg.upsert(id_bg.clone(), resp).await;
                maybe_emit_recap(&state_bg, &id_bg).await;
                events::publish(Event::SessionDone {
                    session_id: id_bg,
                    status: status_str,
                });
            });
        }
        IntentClass::Implementation => {
            // Spawn the workflow in the background. We reuse the current binary so
            // the child inherits full env/init (build counter, tracing, run_id).
            let state_bg = state.clone();
            let id_bg = id.clone();
            tokio::spawn(async move {
                let resp = run_task(&id_bg, req, state_bg.clone())
                    .await
                    .unwrap_or_else(|e| {
                        PmResponse::error(&id_bg, format!("server failed to run task: {e:#}"))
                    });
                let status_str = match resp.status {
                    PmStatus::Success => "success",
                    PmStatus::Failed => "error",
                    PmStatus::Partial => "partial",
                    PmStatus::Running => "running",
                }
                .to_string();
                state_bg.upsert(id_bg.clone(), resp).await;
                maybe_emit_recap(&state_bg, &id_bg).await;
                events::publish(Event::SessionDone {
                    session_id: id_bg,
                    status: status_str,
                });
            });
        }
    }

    (
        StatusCode::ACCEPTED,
        Json(TaskSubmittedBody {
            id,
            status: "running",
        }),
    )
}

/// `GET /api/task/:id` — fetch a cached task response.
///
/// Why: Polling clients read results here after submitting a task.
/// What: Returns the stored `PmResponse` or 404 JSON when unknown.
/// Test: `unknown_task_id_returns_404`.
pub(super) async fn get_task(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<PmResponse>, (StatusCode, Json<serde_json::Value>)> {
    match state.get(&id).await {
        Some(r) => Ok(Json(r)),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "unknown task id", "id": id })),
        )),
    }
}

/// `GET /api/sessions/:id/recap` — return the most recent stored recap for a
/// session, or 404 if none exists yet (#371).
///
/// Why: The GUI's `RecapPanel` polls this endpoint when a session loads so it
/// can render the latest summary + table without waiting for the next
/// `RecapGenerated` SSE event. 404 is the correct shape for "no recap yet"
/// since the resource genuinely doesn't exist on disk.
/// What: Reads `.open-mpm/state/recaps/{id}.json` via `recap::load_recap`.
/// Test: Save a recap then `curl /api/sessions/<id>/recap` → 200 + JSON;
/// missing session → 404.
pub(super) async fn get_session_recap(
    Path(session_id): Path<String>,
    State(_state): State<AppState>,
) -> Response {
    let dir = state_dir();
    match recap::load_recap(&dir, &session_id) {
        Some(r) => Json(r).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "no recap for session", "id": session_id })),
        )
            .into_response(),
    }
}

/// `GET /api/tasks` — list up to `MAX_RETAINED` recent responses, newest first.
pub(super) async fn list_tasks(State(state): State<AppState>) -> Json<Vec<PmResponse>> {
    Json(state.list().await)
}

/// Response body for `POST /api/clear-context`.
#[derive(Debug, Clone, Serialize)]
pub(super) struct ClearContextBody {
    cleared: bool,
    tasks_cancelled: usize,
}

/// `POST /api/clear-context` — wipe all in-memory task state.
///
/// Why: Provides a clean-slate action for the UI without restarting the
/// server process. Useful during development and when accumulated task
/// history causes the sidebar to become cluttered.
/// What: Clears the task store, emits `SessionCancelled` for running tasks,
/// and returns `{"cleared":true,"tasks_cancelled":<N>}`.
/// Test: POST /api/clear-context after submitting a task; assert 200, cleared
/// is true, then GET /api/tasks returns empty array.
pub(super) async fn clear_context(State(state): State<AppState>) -> Json<ClearContextBody> {
    let tasks_cancelled = state.clear_tasks().await;
    Json(ClearContextBody {
        cleared: true,
        tasks_cancelled,
    })
}

/// Query string for `GET /api/docs/search`. (#187)
#[derive(Debug, Deserialize)]
pub(super) struct DocsSearchQuery {
    q: Option<String>,
    /// Optional override for top-N (default 5, capped at 20).
    n: Option<usize>,
}

/// `GET /api/docs/search?q=<query>` — TF-IDF search over project docs. (#187)
///
/// Why: Lets the web UI add a "search docs" feature without spawning the
/// CLI or hitting an LLM. Backed by the same `DocsIndex` instance used by
/// the CTRL `search_docs` tool.
/// What: Returns `{"results":[{path,title,snippet,score}, …]}`. When the
/// index isn't attached (e.g. server started without `--api` wiring), the
/// route returns `{"results":[], "status":"no_index"}` with a 200 status so
/// clients can render a graceful empty state.
/// Test: `docs_search_returns_results_when_index_present`,
/// `docs_search_falls_back_when_index_missing`.
pub(super) async fn docs_search(
    State(state): State<AppState>,
    axum::extract::Query(params): axum::extract::Query<DocsSearchQuery>,
) -> Json<serde_json::Value> {
    let q = params.q.unwrap_or_default();
    let n = params.n.unwrap_or(5).clamp(1, 20);
    let Some(idx) = state.docs_index.as_ref() else {
        return Json(serde_json::json!({
            "results": [],
            "status": "no_index",
        }));
    };
    if q.trim().is_empty() {
        return Json(serde_json::json!({
            "results": [],
            "status": "empty_query",
        }));
    }
    let hits = idx.search(&q, n);
    Json(serde_json::json!({
        "results": hits,
        "status": "ok",
    }))
}
