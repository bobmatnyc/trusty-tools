//! Task lifecycle Tauri commands (`check_health`, `list_tasks`,
//! `send_message`, `cancel_task`).
//!
//! Why: These four commands translate frontend `invoke(...)` calls into REST
//! calls against the `trusty-agents --api` sidecar — split out of `main.rs`
//! (#3220 header-consolidation wave; `main.rs` had grown past the
//! workspace's 500-SLOC production cap) so the request/response translation
//! concern is self-contained, separate from sidecar process management
//! (`sidecar.rs`) and personalization overlay file I/O (`overlay.rs`).
//! What: `check_health`, `list_tasks`, `send_message`, `cancel_task`.
//! Test: `cargo check` in `ui/src-tauri/`; end-to-end behavior is exercised
//! manually (see each command's own doc comment) since it requires a live
//! `trusty-agents --api` sidecar.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter, State};

use crate::sidecar::{api_base, http_health, SharedApi};

/// Task-status poll interval (ms) for the `polls`-th poll (0-indexed) — 250ms
/// for the first `FAST_POLL_COUNT` polls, then 500ms (#3745, critic MEDIUM-3).
///
/// Why: a flat 1500ms poll added up to +1.5s of dead time on top of the LLM
/// call. Fast polling for the opening window (~5s, covering most turns)
/// surfaces a quick result almost immediately; the backoff keeps a genuinely
/// long turn from hammering the sidecar. Pulled out of `send_message`'s loop
/// so the 250/500/boundary contract is unit-testable without a live sidecar.
/// What: `polls < FAST_POLL_COUNT (20)` → 250, else 500. Kept in lock-step
/// with `pollIntervalMs` in `ui/src/lib/transport.ts` (browser fallback).
/// Test: `poll_interval_ms_backoff_contract`.
const FAST_POLL_COUNT: u32 = 20;
const FAST_POLL_MS: u64 = 250;
const SLOW_POLL_MS: u64 = 500;

fn poll_interval_ms(polls: u32) -> u64 {
    if polls < FAST_POLL_COUNT {
        FAST_POLL_MS
    } else {
        SLOW_POLL_MS
    }
}

/// `GET /api/health`.
#[tauri::command]
pub async fn check_health(state: State<'_, SharedApi>) -> Result<bool, String> {
    let port = state.port.lock().await.unwrap_or(8765);
    Ok(http_health(port).await)
}

/// `GET /api/tasks` — recent runs.
#[tauri::command]
pub async fn list_tasks(state: State<'_, SharedApi>) -> Result<Value, String> {
    let port = state.port.lock().await.unwrap_or(8765);
    let url = format!("{}/api/tasks", api_base(port));
    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("list_tasks request failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("list_tasks: HTTP {}", resp.status()));
    }
    resp.json::<Value>()
        .await
        .map_err(|e| format!("list_tasks parse failed: {e}"))
}

/// `DELETE /api/tasks` — clear the finished-task history (#3737).
///
/// Why: The "Recent tasks" panel's "Clear" affordance needs a command that
/// wipes the finished-task list WITHOUT the `clear-context` side effect of
/// aborting in-flight work — the backend `DELETE /api/tasks` handler
/// (`crates/trusty-agents/src/api/server/handlers.rs::clear_recent_tasks`)
/// removes only terminal tasks and keeps any running one. This just forwards
/// to it and returns the `{cleared, removed, retained_running}` body so the
/// frontend can refresh its list.
/// What: Issues `DELETE {api_base}/api/tasks`; a non-2xx status is a genuine
/// `Err` (unlike `cancel_task`, there's no expected-race status to fold in).
/// Test: Run `trusty-agents --api`, submit + finish a task, call this command,
/// assert `removed >= 1` and `GET /api/tasks` no longer lists it.
#[tauri::command]
pub async fn clear_recent_tasks(state: State<'_, SharedApi>) -> Result<Value, String> {
    let port = state.port.lock().await.unwrap_or(8765);
    let url = format!("{}/api/tasks", api_base(port));
    let client = reqwest::Client::new();
    let resp = client
        .delete(&url)
        .send()
        .await
        .map_err(|e| format!("clear_recent_tasks request failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("clear_recent_tasks: HTTP {}", resp.status()));
    }
    resp.json::<Value>()
        .await
        .map_err(|e| format!("clear_recent_tasks parse failed: {e}"))
}

#[derive(Debug, Serialize, Clone)]
struct ProgressEvent<'a> {
    task_id: &'a str,
    message: &'a str,
}

#[derive(Debug, Serialize, Clone)]
struct ErrorEvent<'a> {
    task_id: &'a str,
    error: &'a str,
}

#[derive(Debug, Deserialize)]
struct SubmitResponse {
    id: String,
    #[allow(dead_code)]
    status: String,
}

/// `POST /api/task` then poll `/api/task/:id` until terminal.
///
/// Why: The whole point of this command is to give the frontend one awaitable
/// call that also produces streaming `task-progress` events while the
/// workflow runs. ChatView updates its bubble off those events; InputArea
/// also observes the final return value as a belt-and-suspenders for the
/// browser fallback path.
/// What: Submits the task, emits `task-progress` every 1.5s with a short
/// "running…" tick, then emits `task-complete` (with the full PmResponse
/// JSON) or `task-error` on failure. Returns the final narrative string.
/// #3223 (Trusty Agents agent roster, epic #3052): `agent`, when set,
/// is forwarded as the request body's `agent` field — the same field the
/// `--direct <agent>` subprocess dispatch path already used — so the GUI's
/// roster selection determines which persona answers.
/// #3245 (model/provider picker, epic #3052): `model_id`/`provider_id`,
/// when set, are forwarded verbatim as the request body's `model_id`/
/// `provider_id` fields — the wire names `TaskRequest` (server-side)
/// deserializes — so the GUI's model picker selection pins a model/
/// credential-routing path for this turn. Tauri's `invoke()` maps
/// camelCase JS arg names (`modelId`, `providerId`) to these snake_case
/// Rust params automatically.
/// Test: Run `trusty-agents --api` manually, call this command with `content=
/// "echo hi"`, assert a sequence of progress events followed by
/// `task-complete` and a non-empty narrative return value.
// Why: Every parameter is a flat scalar Tauri maps 1:1 from the frontend's
// `invoke()` args to this request body's JSON fields (mirrors the
// `build_deployment_footer` allow in `trusty-agents/src/ctrl/pm_task/helpers.rs`
// — bundling them into a struct just to satisfy the 7-argument heuristic
// would obscure the `#[tauri::command]` call site without buying real
// abstraction). Allow locally.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn send_message(
    app: AppHandle,
    state: State<'_, SharedApi>,
    content: String,
    project_path: Option<String>,
    workflow: Option<String>,
    agent: Option<String>,
    model_id: Option<String>,
    provider_id: Option<String>,
) -> Result<String, String> {
    let port = state.port.lock().await.unwrap_or(8765);
    let client = reqwest::Client::new();

    let mut body = serde_json::Map::new();
    body.insert("task".into(), Value::String(content));
    body.insert(
        "workflow".into(),
        Value::String(workflow.unwrap_or_else(|| "prescriptive".into())),
    );
    if let Some(p) = project_path.as_ref().filter(|s| !s.is_empty()) {
        body.insert("project_path".into(), Value::String(p.clone()));
    }
    if let Some(a) = agent.as_ref().filter(|s| !s.is_empty()) {
        body.insert("agent".into(), Value::String(a.clone()));
    }
    if let Some(m) = model_id.as_ref().filter(|s| !s.is_empty()) {
        body.insert("model_id".into(), Value::String(m.clone()));
    }
    if let Some(p) = provider_id.as_ref().filter(|s| !s.is_empty()) {
        body.insert("provider_id".into(), Value::String(p.clone()));
    }

    let submit_url = format!("{}/api/task", api_base(port));
    let resp = client
        .post(&submit_url)
        .json(&Value::Object(body))
        .send()
        .await
        .map_err(|e| format!("submit failed: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("submit HTTP {status}: {text}"));
    }
    let submitted: SubmitResponse = resp
        .json()
        .await
        .map_err(|e| format!("submit parse failed: {e}"))?;

    let task_id = submitted.id.clone();
    let _ = app.emit(
        "task-progress",
        ProgressEvent {
            task_id: &task_id,
            message: "submitted to trusty-agents…",
        },
    );

    // Poll until terminal.
    let poll_url = format!("{}/api/task/{}", api_base(port), task_id);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10 * 60);
    // #3741 (perceived-latency fix): a flat 1500ms poll added up to +1.5s of
    // dead time on top of the LLM call — a trivial turn measured ~2.1s at the
    // backend but felt ~3.5s. Poll every 250ms for the first ~20 polls (the
    // first ~5s, which covers the vast majority of turns) so a fast result is
    // surfaced almost immediately, then back off to 500ms so a genuinely long
    // turn doesn't hammer the sidecar for minutes.
    let mut polls: u32 = 0;
    loop {
        if std::time::Instant::now() > deadline {
            let err = "task timed out after 10 minutes";
            let _ = app.emit(
                "task-error",
                ErrorEvent {
                    task_id: &task_id,
                    error: err,
                },
            );
            return Err(err.into());
        }
        let interval_ms = poll_interval_ms(polls);
        polls += 1;
        tokio::time::sleep(std::time::Duration::from_millis(interval_ms)).await;

        let poll = client
            .get(&poll_url)
            .send()
            .await
            .map_err(|e| format!("poll failed: {e}"))?;
        if !poll.status().is_success() {
            // 404 right after submit is transient; keep polling.
            continue;
        }
        let response: Value = poll
            .json()
            .await
            .map_err(|e| format!("poll parse failed: {e}"))?;
        let status = response
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("running");

        if status == "running" {
            let _ = app.emit(
                "task-progress",
                ProgressEvent {
                    task_id: &task_id,
                    message: "running…",
                },
            );
            continue;
        }

        // Terminal state. Emit complete (even on error-status responses so
        // ChatView can display the failure narrative).
        //
        // #3063: `PmResponse::cancelled()` (crates/trusty-agents/src/api/
        // types.rs) always sets narrative to the fixed string "Task
        // cancelled by client request", so a cancelled task's narrative is
        // never empty under the current backend contract — no
        // status-specific special-casing needed here.
        let narrative = response
            .get("narrative")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let _ = app.emit("task-complete", &response);
        return Ok(narrative);
    }
}

/// `DELETE /api/task/:id` — abort an in-flight task (#3063).
///
/// Why: The Stop button and the confirm-then-retask flow in InputArea both
/// need a single awaitable call that maps cleanly onto the backend's
/// `cancel_task` contract (`crates/trusty-agents/src/api/server/cancel.rs`):
/// 200 on success, 404 for an unknown id, 409 when the task already reached
/// a terminal state. Unlike `send_message`, this command must NOT turn a
/// 404/409 into a Rust `Err` — those are expected races the frontend handles
/// gracefully (no error toast), so folding the HTTP status into the `Ok`
/// payload lets the JS side branch on `http_status` instead of parsing error
/// strings.
/// What: Issues `DELETE {api_base}/api/task/{task_id}`, and for any response
/// in `{200, 404, 409}` returns the JSON body with an added `http_status`
/// field. A transport failure or any other status code is a genuine `Err`.
/// Test: Run `trusty-agents --api`, submit a long-running task, call this
/// command with its id — assert `http_status: 200` and `GET /api/task/:id`
/// subsequently reports `status: "cancelled"`. Call again with the same id —
/// assert `http_status: 409`.
#[tauri::command]
pub async fn cancel_task(state: State<'_, SharedApi>, task_id: String) -> Result<Value, String> {
    let port = state.port.lock().await.unwrap_or(8765);
    let url = format!("{}/api/task/{}", api_base(port), task_id);
    let client = reqwest::Client::new();
    let resp = client
        .delete(&url)
        .send()
        .await
        .map_err(|e| format!("cancel_task request failed: {e}"))?;
    let status_code = resp.status();
    let ok_status = status_code.is_success()
        || status_code == reqwest::StatusCode::NOT_FOUND
        || status_code == reqwest::StatusCode::CONFLICT;
    if !ok_status {
        return Err(format!("cancel_task: HTTP {status_code}"));
    }
    let body: Value = resp.json().await.unwrap_or_else(|_| serde_json::json!({}));
    let mut obj = body.as_object().cloned().unwrap_or_default();
    obj.insert(
        "http_status".into(),
        Value::Number(status_code.as_u16().into()),
    );
    Ok(Value::Object(obj))
}

#[cfg(test)]
mod tests {
    use super::{poll_interval_ms, FAST_POLL_MS, SLOW_POLL_MS};

    // #3745 (critic MEDIUM-3): pin the 250/500/20 backoff contract,
    // including the 19/20 boundary, without a live sidecar.
    #[test]
    fn poll_interval_ms_backoff_contract() {
        let cases = [
            (0u32, FAST_POLL_MS),
            (1, FAST_POLL_MS),
            (19, FAST_POLL_MS), // last fast poll
            (20, SLOW_POLL_MS), // first slow poll (boundary)
            (21, SLOW_POLL_MS),
            (1000, SLOW_POLL_MS),
        ];
        for (polls, expected) in cases {
            assert_eq!(
                poll_interval_ms(polls),
                expected,
                "poll #{polls} should use {expected}ms"
            );
        }
    }
}
