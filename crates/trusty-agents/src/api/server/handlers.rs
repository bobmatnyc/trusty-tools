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
    /// full workflow (via `trusty-agents --direct <agent>`) for
    /// `IntentClass::Implementation`. #3223 (Trusty Assistant agent roster,
    /// epic #3052) additionally threads this into the in-process
    /// Conversational/Research path (see [`agent_override`] and
    /// `submit_task`), so the same field selects the active persona for
    /// both dispatch shapes — the GUI's roster never needs to know which
    /// path a given message will take.
    #[serde(default)]
    pub agent: Option<String>,
    /// Tauri GUI: when set, run the spawned `trusty-agents` subprocess with this
    /// directory as its working directory so project-scoped PMs can operate
    /// on a specific project without the caller having to `cd` first.
    ///
    /// Why: The desktop chat interface allows users to register multiple
    /// project paths and chat with a per-project PM; each task must execute
    /// in that project's root so `.trusty-agents/`, file paths, and shell tools
    /// resolve relative to the correct codebase.
    /// What: Optional absolute path. When present and pointing at a directory,
    /// `run_task` sets it as the child process's `current_dir`.
    /// Test: Submit a task with `project_path: "/tmp"`, assert the spawned
    /// subprocess inherits that cwd (observable via child tracing or stdout
    /// from a task that prints `std::env::current_dir()`).
    #[serde(default)]
    pub project_path: Option<String>,
    /// GUI model/provider picker (#3245, epic #3052): pins a specific model
    /// id for this turn, mirroring the REPL's `/model <id>` slash command.
    ///
    /// Why: `GET /api/models` (#3243) gives the picker a live catalog of
    /// models the user can choose from; without this field the choice had
    /// nowhere to go and the request always fell back to the agent
    /// config's default model. Naming matches the `/api/models` response
    /// shape (`ModelProviderEntry.default_model`'s selected value) so the
    /// GUI can round-trip a catalog entry straight into the request body.
    /// What: When `Some`, threaded into `SessionOverrides::model` and
    /// applied to `cfg.agent.model` before dispatch. `None` (the default —
    /// omitted entirely by existing callers) preserves the pre-#3245
    /// behavior byte-for-byte: the agent config's own model is used.
    /// Test: `session_overrides_for_passes_through_model_and_provider`,
    /// `session_overrides_for_defaults_to_none`.
    #[serde(default)]
    pub model_id: Option<String>,
    /// GUI model/provider picker (#3245, epic #3052): pins a credential-
    /// routing path for this turn, mirroring the REPL's `/provider <name>`
    /// slash command.
    ///
    /// Why: Selecting a model from `GET /api/models` often implies a
    /// specific provider (e.g. Bedrock vs. OpenRouter) rather than letting
    /// the normal env-credential probe pick one. Only known values are
    /// meaningful — see `resolve_overridden_credentials` for the accepted
    /// set (`"claude-code"`, `"openrouter"`, `"bedrock"`, `"local"`); an
    /// unrecognized value fails the turn with a clear error rather than
    /// silently falling back, so a picker bug surfaces immediately instead
    /// of routing through the wrong credential path.
    /// What: When `Some`, threaded into `SessionOverrides::provider`.
    /// `None` (the default) preserves the pre-#3245 behavior byte-for-byte:
    /// the normal `pick_credentials()` env probe runs unchanged.
    /// Test: `session_overrides_for_passes_through_model_and_provider`,
    /// `session_overrides_for_defaults_to_none`.
    #[serde(default)]
    pub provider_id: Option<String>,
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

/// Directory under `.trusty-agents/` holding per-project TOML configs.
///
/// Why: `tell` routing and project lookups need to load `<project>.toml`.
/// Centralizing the path here so tests/CLI/tm handlers agree on the layout.
/// What: Returns `.trusty-agents/projects`.
/// Test: Indirectly via `get_project_config_*` and tm `tell` tests.
pub(super) fn projects_config_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(".trusty-agents/projects")
}

/// Directory under `.trusty-agents/` holding per-agent TOML manifests.
///
/// Why: `GET /api/agents` (`projects::list_agents_route`) and
/// `PATCH /api/agents/:name` (#3246, `agent_patch::patch_agent_route`) must
/// agree on where agent manifests live on disk. Centralizing the literal
/// here (mirroring [`projects_config_dir`]) keeps the two routes from
/// drifting on a hand-typed path.
/// What: Returns `.trusty-agents/agents`, relative to the process cwd.
/// Test: Indirectly via `list_agents_*` and `patch_agent_*` route tests.
pub(super) fn agents_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(".trusty-agents/agents")
}

/// `GET /api/health` — liveness + version probe.
pub(super) async fn health() -> Json<HealthBody> {
    Json(HealthBody {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
    })
}

/// Extract a non-empty agent override from a task request (#3223).
///
/// Why: The GUI's agent roster only sends a non-`None` `agent` (e.g.
/// `Some("cto-assistant")`) when the user has EXPLICITLY picked a roster
/// entry — the roster's `activeAgentId` store defaults to `null`
/// specifically so the default, no-selection GUI message omits this field
/// entirely (regression fixed post-PR #3279: an earlier version of the
/// frontend defaulted `activeAgentId` to the base `assistant` id and
/// forwarded it unconditionally, which forced EVERY default chat message
/// through the tools-off `run_pm_task_with_persona` path below — see
/// `submit_task`'s `agent_for_chat` — silently losing delegation/tool
/// capability for ordinary chat). This function's blank/whitespace
/// normalization is a defensive second layer (e.g. a stale/cleared roster
/// selection serialized as `agent: ""`), not the primary guard — the
/// primary guard is the frontend not sending the field at all by default.
/// What: Trims `req.agent` and returns `None` when absent or blank,
/// otherwise the trimmed owned string.
/// Test: `agent_override_normalizes_blank_to_none`,
/// `agent_override_passes_through_trimmed_name`,
/// `submit_task_without_agent_uses_session_path`,
/// `submit_task_with_agent_uses_persona_path`.
fn agent_override(req: &TaskRequest) -> Option<String> {
    req.agent
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Resolve which Conversational/Research dispatch path a request should
/// take (#3223; extracted as a pure function during the PR #3279
/// code-critic regression fix).
///
/// Why: The choice between the tools-armed `run_pm_task_with_session` path
/// and the tools-off `run_pm_task_with_persona` path (see `submit_task`'s
/// match arm below) is the single most consequential decision in this
/// file — get it wrong and ordinary GUI chat silently loses delegation/tool
/// capability (see `agent_override`'s doc comment for the regression this
/// guards against). Pulling it out of `submit_task`'s body into its own
/// pure function lets that decision be pinned by a fast unit test that
/// needs no HTTP router, background task, or LLM/agent-loading machinery.
/// What: Returns `None` (→ session path) when `is_ctrl_command` is `true`
/// OR `agent_override(req)` is `None` (no roster selection, or a
/// blank/whitespace one); otherwise the trimmed agent name (→ persona
/// path).
/// Test: `submit_task_without_agent_uses_session_path`,
/// `submit_task_with_agent_uses_persona_path`,
/// `submit_task_ctrl_command_ignores_agent_override`.
fn resolve_agent_for_chat(req: &TaskRequest, is_ctrl_command: bool) -> Option<String> {
    if is_ctrl_command {
        None
    } else {
        agent_override(req)
    }
}

/// Build the model/provider `SessionOverrides` for a Conversational/Research
/// dispatch (#3245, epic #3052).
///
/// Why: `TaskRequest.model_id`/`provider_id` let the GUI's model picker pin
/// a model or credential-routing path for a single turn, mirroring the
/// REPL's `/model`/`/provider` slash commands (see `SessionOverrides`'s own
/// doc comment in `ctrl::config`). Extracted as its own pure function —
/// following the `agent_override`/`resolve_agent_for_chat` pattern above —
/// so the wire-field-to-override mapping is unit-testable without the HTTP
/// router or an LLM call.
/// What: `model` = `req.model_id` verbatim (`None` → the agent config's
/// own `cfg.agent.model` is used, exactly as before this field existed).
/// `provider` = `req.provider_id` verbatim (`None` → the normal
/// `pick_credentials()` env probe runs unchanged; `Some` is validated by
/// `resolve_overridden_credentials`, which errors clearly on an
/// unrecognized value). `user` is always `None` — the HTTP path carries no
/// authenticated caller identity to forward (unlike the Slack/Telegram
/// bridges).
/// Test: `session_overrides_for_defaults_to_none`,
/// `session_overrides_for_passes_through_model_and_provider`.
fn session_overrides_for(req: &TaskRequest) -> crate::ctrl::SessionOverrides {
    crate::ctrl::SessionOverrides {
        model: req.model_id.clone(),
        provider: req.provider_id.clone(),
        user: None,
    }
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

    // #3223 (Trusty Assistant agent roster, epic #3052): CTRL management
    // commands ("add project …", "list projects", …) must always run
    // through the full CTRL session (`run_pm_task_with_session`) regardless
    // of the roster's active agent — that's the only dispatch path wired to
    // CTRL's project-management tools (AddProjectTool, RemoveProjectTool,
    // ListProjectsTool, SetActiveProjectTool, StopTaskTool). The roster's
    // agent override only applies to ordinary conversational/research turns.
    let agent_for_chat = resolve_agent_for_chat(&req, is_ctrl_command);
    // #3245: GUI model/provider picker overrides for this turn. Computed
    // once so both Conversational/Research dispatch paths below (persona
    // and session) apply the same selection.
    let overrides = session_overrides_for(&req);

    match intent {
        IntentClass::Conversational | IntentClass::Research => {
            // Both run in-process. With no roster agent selected, this goes
            // through run_pm_task_with_session (which re-classifies
            // internally: Conversational hits the no-tools fast path,
            // Research falls through to the tool-armed PM loop). With a
            // roster agent selected (#3223), it instead goes through
            // run_pm_task_with_persona — the same persona-chat path the REPL
            // `/agent` command and the Slack/Telegram bridges use — so
            // chatting with a named agent (bundled `.toml`/directory-package
            // or a user's `~/.trusty-agents/agents/<slug>.md` personalization
            // overlay, #3224) resolves identically everywhere.
            let state_bg = state.clone();
            let id_bg = id.clone();
            let task_text = req.task.clone();
            let project_path = req
                .project_path
                .clone()
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
            let agent_bg = agent_for_chat.clone();
            let overrides_bg = overrides.clone();
            let intent_label = match intent {
                IntentClass::Conversational => "conversational",
                IntentClass::Research => "research",
                IntentClass::Implementation => "implementation",
            };

            let join = tokio::spawn(async move {
                let result = if let Some(agent_name) = agent_bg {
                    crate::ctrl::run_pm_task_with_persona(
                        &project_path,
                        &agent_name,
                        &task_text,
                        &[],
                        Some(id_bg.clone()),
                        overrides_bg,
                    )
                    .await
                } else {
                    // #3245: threading `overrides_bg` requires the
                    // history-aware entry point — `run_pm_task_with_session`
                    // hardcodes `SessionOverrides::default()` internally and
                    // has no override parameter. This call is exactly what
                    // `run_pm_task_with_session` does under the hood (empty
                    // history, single turn) plus the model/provider pin.
                    crate::ctrl::run_pm_task_with_history(
                        &project_path,
                        &task_text,
                        &[],
                        Some(id_bg.clone()),
                        overrides_bg,
                    )
                    .await
                };

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

                let status_str = resp.status.as_str().to_string();
                // #3063: finalize_task (not upsert) so a result that was
                // still in flight when the client cancelled doesn't clobber
                // the already-recorded Cancelled state.
                state_bg.finalize_task(id_bg.clone(), resp).await;
                maybe_emit_recap(&state_bg, &id_bg).await;
                events::publish(Event::SessionDone {
                    session_id: id_bg,
                    status: status_str,
                });
            });
            // #3063: register the abort handle so DELETE /api/task/:id (or
            // clear-context) can cancel this in-process future.
            state.register_handle(&id, join.abort_handle()).await;
        }
        IntentClass::Implementation => {
            // Spawn the workflow in the background. We reuse the current binary so
            // the child inherits full env/init (build counter, tracing, run_id).
            let state_bg = state.clone();
            let id_bg = id.clone();
            let join = tokio::spawn(async move {
                let resp = run_task(&id_bg, req, state_bg.clone())
                    .await
                    .unwrap_or_else(|e| {
                        PmResponse::error(&id_bg, format!("server failed to run task: {e:#}"))
                    });
                let status_str = resp.status.as_str().to_string();
                // #3063: see the Conversational/Research branch above for why
                // finalize_task (not upsert) is used here.
                state_bg.finalize_task(id_bg.clone(), resp).await;
                maybe_emit_recap(&state_bg, &id_bg).await;
                events::publish(Event::SessionDone {
                    session_id: id_bg,
                    status: status_str,
                });
            });
            // #3063: register the abort handle. Aborting this outer task
            // drops `run_task`'s frame — including its `Child` — at whatever
            // `.await` it's suspended at; `kill_on_drop` (set in
            // `task_runner::run_task`) then kills the OS subprocess.
            state.register_handle(&id, join.abort_handle()).await;
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
/// What: Reads `.trusty-agents/state/recaps/{id}.json` via `recap::load_recap`.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn base_request(agent: Option<&str>) -> TaskRequest {
        TaskRequest {
            task: "hi".to_string(),
            workflow: None,
            out_dir: None,
            task_file: None,
            agent: agent.map(str::to_string),
            project_path: None,
            model_id: None,
            provider_id: None,
        }
    }

    #[test]
    fn agent_override_normalizes_blank_to_none() {
        assert_eq!(agent_override(&base_request(None)), None);
        assert_eq!(agent_override(&base_request(Some(""))), None);
        assert_eq!(agent_override(&base_request(Some("   "))), None);
    }

    #[test]
    fn agent_override_passes_through_trimmed_name() {
        assert_eq!(
            agent_override(&base_request(Some("  cto-assistant  "))).as_deref(),
            Some("cto-assistant")
        );
        assert_eq!(
            agent_override(&base_request(Some("assistant"))).as_deref(),
            Some("assistant")
        );
    }

    /// Regression pin (code-critic BLOCK on PR #3279): a default GUI
    /// message — no `agent` field on the wire, because the frontend's
    /// `activeAgentId` now defaults to `null` rather than the base
    /// `assistant` id — must resolve to `None`, which routes `submit_task`
    /// through `run_pm_task_with_session` (tools-armed), NOT
    /// `run_pm_task_with_persona` (tools-off). Before the fix,
    /// `activeAgentId` defaulted to `"assistant"` and was forwarded
    /// unconditionally, so EVERY default chat message silently lost
    /// delegation/tool capability.
    #[test]
    fn submit_task_without_agent_uses_session_path() {
        assert_eq!(resolve_agent_for_chat(&base_request(None), false), None);
    }

    /// An explicitly-selected roster entry (any non-blank `agent`,
    /// including the base `"assistant"` — picking it is an intentional
    /// opt-in, not the passive default) resolves to `Some`, which routes
    /// `submit_task` through `run_pm_task_with_persona`.
    #[test]
    fn submit_task_with_agent_uses_persona_path() {
        assert_eq!(
            resolve_agent_for_chat(&base_request(Some("cto-assistant")), false).as_deref(),
            Some("cto-assistant")
        );
    }

    /// CTRL management commands ("add project …", "list projects", …) must
    /// always use the tools-armed session path — that's the only path
    /// wired to CTRL's project-management tools — regardless of whatever
    /// the roster's active agent happens to be.
    #[test]
    fn submit_task_ctrl_command_ignores_agent_override() {
        assert_eq!(
            resolve_agent_for_chat(&base_request(Some("cto-assistant")), true),
            None
        );
    }

    /// #3245: omitting `model_id`/`provider_id` entirely (every pre-#3245
    /// caller, and any GUI submission before the user touches the picker)
    /// must produce a no-op `SessionOverrides`, so dispatch behaves exactly
    /// as it did before this field existed — the agent config's own model
    /// and the normal env-credential probe are used unchanged.
    #[test]
    fn session_overrides_for_defaults_to_none() {
        let overrides = session_overrides_for(&base_request(None));
        assert_eq!(overrides.model, None);
        assert_eq!(overrides.provider, None);
        assert!(overrides.user.is_none());
    }

    /// #3245: the GUI model/provider picker's selection threads through
    /// verbatim — this is the field-mapping contract the frontend's
    /// `POST /api/task` body relies on.
    #[test]
    fn session_overrides_for_passes_through_model_and_provider() {
        let mut req = base_request(None);
        req.model_id = Some("anthropic/claude-opus-4-6".to_string());
        req.provider_id = Some("bedrock".to_string());
        let overrides = session_overrides_for(&req);
        assert_eq!(
            overrides.model.as_deref(),
            Some("anthropic/claude-opus-4-6")
        );
        assert_eq!(overrides.provider.as_deref(), Some("bedrock"));
    }

    /// `POST /api/task` must accept a body carrying `model_id`/`provider_id`
    /// and a body omitting them entirely (serde `#[serde(default)]`) — the
    /// omission case is what every existing caller (CLI, older GUI builds)
    /// sends, and it must keep deserializing to `None` rather than erroring.
    #[test]
    fn task_request_deserializes_model_and_provider_fields() {
        let with_fields: TaskRequest = serde_json::from_str(
            r#"{"task":"hi","model_id":"claude-opus-4-6","provider_id":"openrouter"}"#,
        )
        .expect("should deserialize with model_id/provider_id present");
        assert_eq!(with_fields.model_id.as_deref(), Some("claude-opus-4-6"));
        assert_eq!(with_fields.provider_id.as_deref(), Some("openrouter"));

        let without_fields: TaskRequest =
            serde_json::from_str(r#"{"task":"hi"}"#).expect("should deserialize when omitted");
        assert_eq!(without_fields.model_id, None);
        assert_eq!(without_fields.provider_id, None);
    }
}
