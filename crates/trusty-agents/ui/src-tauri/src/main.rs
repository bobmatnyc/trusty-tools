//! trusty-agents desktop chat (Tauri 2).
//!
//! Why: Gives users a native chat UI for talking to the CTRL controller and
//! project-scoped PMs without hand-running `trusty-agents --task '…'` at the
//! command line. The Rust side here only does three things: (1) spawn the
//! `trusty-agents --api` sidecar on startup so the REST server is reachable,
//! (2) translate frontend `invoke(...)` calls into REST calls against that
//! sidecar, and (3) emit `task-progress` / `task-complete` / `task-error`
//! Tauri events so ChatView can stream a running task into its bubble.
//! What: Seven Tauri commands (`ensure_api_server`, `send_message`,
//! `cancel_task`, `list_tasks`, `check_health`, `read_personalization_overlay`,
//! `write_personalization_overlay`) plus a lightweight spawned-process
//! registry to avoid double-starting the API server. As of #3059 the window
//! runs in persistent/tray mode: closing the window only hides it (the
//! sidecar stays alive so in-flight tasks keep running); a tray icon with
//! Show/Quit lets the user bring the window back or fully quit. The sidecar
//! is only reaped on a real quit (tray "Quit", Cmd+Q, or app-menu Quit — all
//! surface as `RunEvent::ExitRequested`). #3061 adds the
//! `read_personalization_overlay` / `write_personalization_overlay` pair
//! (implementation lives in [`personalization`], split out in #3270 to keep
//! this file under the workspace's 500-SLOC production-file cap) so the
//! Personality panel can edit the user's `~/.trusty-agents/agents/*.md`
//! overlay directly — both operate ONLY under that fixed `$HOME` directory,
//! gated by a strict `[a-zA-Z0-9_-]+` slug check on `name` (no path
//! separators, no `..` traversal). #3063 adds `cancel_task`, proxying
//! `DELETE /api/task/:id` so the frontend can Stop/Retask a running task; the
//! existing `send_message` poll loop already detects the resulting
//! `status: "cancelled"` as terminal, so no separate cancellation event path
//! is needed on the Tauri side.
//! Test: `cargo check` in `ui/src-tauri/` passes; launching the app and
//! sending a message produces a chat bubble that grows while polling the
//! task id. Tray hide/show/quit behavior is smoke-tested manually (see PR
//! description) — Tauri's window-manager event loop isn't unit-testable
//! from this crate. Personalization overlay commands are covered by unit
//! tests in [`personalization`].

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod personalization;

use std::sync::Arc;

use personalization::{read_personalization_overlay, write_personalization_overlay};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, RunEvent, State, WindowEvent};
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
async fn cancel_task(state: State<'_, SharedApi>, task_id: String) -> Result<Value, String> {
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

/// Reap the spawned `trusty-agents --api` sidecar, exactly once.
///
/// Why: The tray "Quit" item and the real-quit `RunEvent::ExitRequested`
/// path (Cmd+Q, app-menu Quit) both need to kill the sidecar before the app
/// goes away, and they can race each other (the Quit item itself calls
/// `AppHandle::exit`, which raises a fresh `ExitRequested`). `guard.take()`
/// empties the slot on first use, so a second caller finds `None` and is a
/// safe no-op — no double-kill, no error.
/// What: Locks `state.child`, takes the `Child` if present, sends it a kill
/// signal and awaits reaping.
/// Test: Manually — quit via the tray item, confirm the sidecar process
/// exits (`ps` shows no `trusty-agents --api`); quit via Cmd+Q, same check.
async fn kill_sidecar(state: &SharedApi) {
    let mut guard = state.child.lock().await;
    if let Some(mut child) = guard.take() {
        let _ = child.start_kill();
        let _ = child.wait().await;
    }
}

/// Show and focus the main window (tray "Show" item / tray icon click).
fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn main() {
    // Best-effort tracing init; errors (e.g. subscriber already set in tests)
    // are safe to ignore.
    let _ = tracing_subscriber_try_init();

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage::<SharedApi>(Arc::new(ApiServerState::default()))
        .invoke_handler(tauri::generate_handler![
            ensure_api_server,
            send_message,
            cancel_task,
            list_tasks,
            check_health,
            read_personalization_overlay,
            write_personalization_overlay,
        ])
        .setup(|app| {
            // Tray icon + menu (#3059): lets the user bring the hidden main
            // window back or fully quit the app (which is otherwise no
            // longer reachable once the window is hidden, since there's no
            // dock-icon-click affordance guarantee across platforms).
            let show_item = MenuItem::with_id(app, "show", "Show", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let tray_menu = Menu::with_items(app, &[&show_item, &quit_item])?;

            let tray_icon =
                tauri::image::Image::from_bytes(include_bytes!("../icons/tray-icon.png"))?;

            TrayIconBuilder::new()
                .icon(tray_icon)
                .icon_as_template(true)
                .tooltip("trusty-agents")
                .menu(&tray_menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "show" => show_main_window(app),
                    "quit" => {
                        // Reap the sidecar fully before exiting so we don't
                        // race the process teardown, then trigger the real
                        // exit (which also fires `RunEvent::ExitRequested`,
                        // but `kill_sidecar` is idempotent by then).
                        let handle = app.clone();
                        tauri::async_runtime::spawn(async move {
                            if let Some(state) = handle.try_state::<SharedApi>() {
                                kill_sidecar(&state).await;
                            }
                            handle.exit(0);
                        });
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        show_main_window(tray.app_handle());
                    }
                })
                .build(app)?;

            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                // This app currently has exactly one window ("main", per
                // tauri.conf.json). Guard on the label explicitly so a
                // future secondary window (settings/about/etc.) does not
                // silently inherit hide-on-close — only "main" participates
                // in persistent/tray mode.
                if window.label() != "main" {
                    return;
                }
                // Persistent/tray mode (#3059): closing the window must NOT
                // kill the sidecar or the app — hide it instead. The tray
                // "Show" item / tray-icon click brings it back without
                // re-spawning the API server (ensure_api_server's health
                // check short-circuits since it's still running).
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(|app_handle, event| {
        if let RunEvent::ExitRequested { api, .. } = event {
            // Real quit path: Cmd+Q or the app-menu Quit item (the tray
            // "Quit" item already reaped the sidecar itself before calling
            // `exit`, so this is a no-op in that case thanks to `take()`).
            // We must not block this callback on the async mutex — the
            // runtime checks for a pending `prevent_exit()` via a
            // synchronous `try_recv()` immediately after this closure
            // returns, so a bare fire-and-forget spawn here can lose the
            // race and let the process exit before `kill_sidecar` runs.
            // Try a non-blocking lock first (the common case); if it's
            // contended, call `prevent_exit()` synchronously *before*
            // returning, finish the kill asynchronously, then trigger the
            // real exit ourselves once it's done — mirroring the tray
            // "Quit" pattern above.
            if let Some(state) = app_handle.try_state::<SharedApi>() {
                match state.child.try_lock() {
                    Ok(mut guard) => {
                        if let Some(mut child) = guard.take() {
                            let _ = child.start_kill();
                        }
                    }
                    Err(_) => {
                        api.prevent_exit();
                        let state = state.inner().clone();
                        let handle = app_handle.clone();
                        tauri::async_runtime::spawn(async move {
                            kill_sidecar(&state).await;
                            handle.exit(0);
                        });
                    }
                }
            }
        }
    });
}

/// Wrapper so we can ignore the Result without pulling in `tracing-subscriber`
/// at the top level — keeps the Cargo.toml lean.
fn tracing_subscriber_try_init() -> Result<(), String> {
    // No-op: we just inherit stderr from the Rust side, which is enough for
    // dev. Hook in `tracing-subscriber` here when we want filtering.
    Ok(())
}
