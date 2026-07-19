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
/// #3223 (Trusty Assistant agent roster, epic #3052): `agent`, when set,
/// is forwarded as the request body's `agent` field — the same field the
/// `--direct <agent>` subprocess dispatch path already used — so the GUI's
/// roster selection determines which persona answers.
/// Test: Run `trusty-agents --api` manually, call this command with `content=
/// "echo hi"`, assert a sequence of progress events followed by
/// `task-complete` and a non-empty narrative return value.
#[tauri::command]
pub async fn send_message(
    app: AppHandle,
    state: State<'_, SharedApi>,
    content: String,
    project_path: Option<String>,
    workflow: Option<String>,
    agent: Option<String>,
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
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;

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
