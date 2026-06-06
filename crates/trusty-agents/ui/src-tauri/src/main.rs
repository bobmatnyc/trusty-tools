//! trusty-agents desktop chat (Tauri 2).
//!
//! Why: Gives users a native chat UI for talking to the CTRL controller and
//! project-scoped PMs without hand-running `trusty-agents --task '…'` at the
//! command line. The Rust side here only does three things: (1) spawn the
//! `trusty-agents --api` sidecar on startup so the REST server is reachable,
//! (2) translate frontend `invoke(...)` calls into REST calls against that
//! sidecar, and (3) emit `task-progress` / `task-complete` / `task-error`
//! Tauri events so ChatView can stream a running task into its bubble.
//! What: Four Tauri commands (`ensure_api_server`, `send_message`,
//! `list_tasks`, `check_health`) plus a lightweight spawned-process registry
//! to avoid double-starting the API server.
//! Test: `cargo check` in `ui/src-tauri/` passes; launching the app and
//! sending a message produces a chat bubble that grows while polling the
//! task id.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::process::Child;
use tokio::sync::Mutex;

/// Shared handle to the spawned `trusty-agents --api` sidecar.
///
/// Why: We must not spawn the API sidecar twice; a `Mutex<Option<Child>>`
/// lets us check-and-insert atomically and lets the window-destroy hook
/// kill the child cleanly.
/// What: `None` until `ensure_api_server` spawns the subprocess. `Some(child)`
/// thereafter.
/// Test: Call `ensure_api_server(7654)` twice, assert only one child is
/// spawned (second call short-circuits on `is_some()`).
#[derive(Default)]
struct ApiServerState {
    child: Mutex<Option<Child>>,
    port: Mutex<Option<u16>>,
}

type SharedApi = Arc<ApiServerState>;

fn api_base(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

/// Spawn `trusty-agents --api --port <port>` if not already running.
///
/// Why: Lets the frontend operate as soon as the window is visible without
/// asking the user to start a server first. Silently succeeds when the
/// sidecar is already running so repeated calls from `App.svelte::onMount`
/// are safe.
/// What: Checks `/api/health` first — if it responds OK, does nothing. Else
/// spawns `trusty-agents --api --port <port>` (resolved relative to $PATH first,
/// then a workspace-relative debug/release target) and records the `Child`.
/// Test: Call this twice — second call returns early; kill the child and
/// call again — it re-spawns.
#[tauri::command]
async fn ensure_api_server(port: u16, state: State<'_, SharedApi>) -> Result<(), String> {
    // Fast path: if the server already answers, there is nothing to do.
    if http_health(port).await {
        let mut p = state.port.lock().await;
        *p = Some(port);
        return Ok(());
    }

    let mut guard = state.child.lock().await;
    if let Some(ref mut existing) = *guard {
        // Check whether the previously-spawned child is still alive.
        // `try_wait` returns Ok(None) if running, Ok(Some(status)) if exited.
        match existing.try_wait() {
            Ok(Some(_status)) => {
                // Child exited — clear the slot so we can respawn below.
                tracing::warn!(port, "trusty-agents sidecar exited; respawning");
                *guard = None;
            }
            Ok(None) => {
                // Still running but not yet healthy; let caller poll health.
                return Ok(());
            }
            Err(e) => {
                tracing::warn!(?e, "try_wait on sidecar failed; clearing and respawning");
                *guard = None;
            }
        }
    }

    let binary = resolve_tagent_binary();

    // #api-sidecar-cwd: When launched from the macOS .app bundle, the Tauri
    // process's cwd is `/` (sealed read-only APFS volume). The sidecar's
    // self-project detection falls back to cwd when no marker is found, which
    // would result in attempts to create `/.trusty-agents/state` (EROFS). Pass the
    // compile-time-known trusty-agents project root via TAGENT_PROJECT_DIR so the
    // sidecar resolves state dirs and `.env.local` against the correct path.
    //
    // Why: Fix for "API server did not become healthy within 20s" — the
    // sidecar was crashing on `create_dir_all("/.trusty-agents/state")` before
    // binding the HTTP listener.
    // What: Set TAGENT_PROJECT_DIR to the trusty-agents repo root derived from
    // CARGO_MANIFEST_DIR (ui/src-tauri → ../.. → repo root).
    // Test: Launch the bundled .app, observe sidecar reaches /api/health
    // within the 20s polling window.
    let project_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("."));

    tracing::info!(
        ?binary,
        port,
        ?project_root,
        "spawning trusty-agents --api sidecar"
    );

    let child = tokio::process::Command::new(&binary)
        .arg("--api")
        .arg("--port")
        .arg(port.to_string())
        .env("TAGENT_PROJECT_DIR", &project_root)
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .map_err(|e| format!("failed to spawn {}: {e}", binary.display()))?;

    *guard = Some(child);
    let mut p = state.port.lock().await;
    *p = Some(port);
    Ok(())
}

/// Find the `trusty-agents` binary. Prefers `$PATH`, then the sibling Cargo
/// workspace's debug/release target so `cargo run` in this repo's root
/// doesn't require a global install.
fn resolve_tagent_binary() -> std::path::PathBuf {
    // 1. $PATH (works in dev / CLI contexts)
    if let Ok(path) = which("trusty-agents") {
        return path;
    }
    // 2. Explicit well-known install locations (macOS .app bundles get a
    //    minimal PATH like `/usr/bin:/bin:/usr/sbin:/sbin` so $HOME/.cargo/bin
    //    and $HOME/.local/bin are invisible above). #364
    if let Ok(home) = std::env::var("HOME") {
        let home = std::path::Path::new(&home);
        for candidate in [
            home.join(".cargo/bin/trusty-agents"),
            home.join(".local/bin/trusty-agents"),
        ] {
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    // 3. Sibling Cargo workspace target dir (ui/src-tauri → trusty-agents root).
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    if let Some(root) = manifest.ancestors().nth(2) {
        for profile in ["release", "debug"] {
            let candidate = root.join("target").join(profile).join("trusty-agents");
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    // 4. Fallback: trust $PATH resolution at spawn time.
    std::path::PathBuf::from("trusty-agents")
}

/// Minimal `which` that tolerates missing `which` crate dep.
fn which(name: &str) -> Result<std::path::PathBuf, ()> {
    let path = std::env::var_os("PATH").ok_or(())?;
    for entry in std::env::split_paths(&path) {
        let candidate = entry.join(name);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(())
}

async fn http_health(port: u16) -> bool {
    let url = format!("{}/api/health", api_base(port));
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(500))
        .build();
    let Ok(client) = client else { return false };
    match client.get(&url).send().await {
        Ok(r) => r.status().is_success(),
        Err(_) => false,
    }
}

/// `GET /api/health`.
#[tauri::command]
async fn check_health(state: State<'_, SharedApi>) -> Result<bool, String> {
    let port = state.port.lock().await.unwrap_or(8765);
    Ok(http_health(port).await)
}

/// `GET /api/tasks` — recent runs.
#[tauri::command]
async fn list_tasks(state: State<'_, SharedApi>) -> Result<Value, String> {
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
/// Test: Run `trusty-agents --api` manually, call this command with `content=
/// "echo hi"`, assert a sequence of progress events followed by
/// `task-complete` and a non-empty narrative return value.
#[tauri::command]
async fn send_message(
    app: AppHandle,
    state: State<'_, SharedApi>,
    content: String,
    project_path: Option<String>,
    workflow: Option<String>,
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
        let narrative = response
            .get("narrative")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let _ = app.emit("task-complete", &response);
        return Ok(narrative);
    }
}

fn main() {
    // Best-effort tracing init; errors (e.g. subscriber already set in tests)
    // are safe to ignore.
    let _ = tracing_subscriber_try_init();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage::<SharedApi>(Arc::new(ApiServerState::default()))
        .invoke_handler(tauri::generate_handler![
            ensure_api_server,
            send_message,
            list_tasks,
            check_health,
        ])
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::Destroyed = event {
                // Reap the spawned trusty-agents sidecar on window close so we
                // don't leak a listener port between app restarts.
                if let Some(api) = window.app_handle().try_state::<SharedApi>() {
                    let api = api.inner().clone();
                    tauri::async_runtime::spawn(async move {
                        let mut guard = api.child.lock().await;
                        if let Some(mut child) = guard.take() {
                            let _ = child.start_kill();
                            let _ = child.wait().await;
                        }
                    });
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// Wrapper so we can ignore the Result without pulling in `tracing-subscriber`
/// at the top level — keeps the Cargo.toml lean.
fn tracing_subscriber_try_init() -> Result<(), String> {
    // No-op: we just inherit stderr from the Rust side, which is enough for
    // dev. Hook in `tracing-subscriber` here when we want filtering.
    Ok(())
}
