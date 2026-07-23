//! trusty-agents desktop chat (Tauri 2).
//!
//! Why: Gives users a native chat UI for talking to the CTRL controller,
//! project-scoped PMs, and — as of #3223/#3224 (Trusty Agents agent
//! roster, epic #3052) — named persona agents, without hand-running
//! `trusty-agents --task '…'` at the command line. The Rust side here only
//! does three things: (1) spawn the `trusty-agents --api` sidecar on startup
//! so the REST server is reachable (`sidecar` module), (2) translate
//! frontend `invoke(...)` calls into REST calls against that sidecar
//! (`task_commands` module) or direct `$HOME` filesystem access
//! (`overlay` module), and (3) emit `task-progress` / `task-complete` /
//! `task-error` Tauri events so ChatView can stream a running task into its
//! bubble. This file itself is just the entry point + tray/window wiring —
//! the command implementations live in their own modules (split in the
//! #3220 header-consolidation wave to stay under the workspace's 500-SLOC
//! production-file cap).
//! What: Ten Tauri commands across three modules — `sidecar::ensure_api_server`,
//! `task_commands::{send_message, cancel_task, list_tasks, check_health}`,
//! `overlay::{read_personalization_overlay, write_personalization_overlay,
//! list_personalization_overlays, delete_personalization_overlay}` — plus
//! this file's tray icon / persistent-window lifecycle. As of #3059 the
//! window runs in persistent/tray mode: closing the window only hides it
//! (the sidecar stays alive so in-flight tasks keep running); a tray icon
//! with Show/Quit lets the user bring the window back or fully quit. The
//! sidecar is only reaped on a real quit (tray "Quit", Cmd+Q, or app-menu
//! Quit — all surface as `RunEvent::ExitRequested`).
//! Test: `cargo check` in `ui/src-tauri/` passes; launching the app and
//! sending a message produces a chat bubble that grows while polling the
//! task id. Tray hide/show/quit behavior is smoke-tested manually (see PR
//! description) — Tauri's window-manager event loop isn't unit-testable
//! from this crate. Command-level unit tests live alongside their
//! implementations in `overlay.rs`.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod overlay;
mod sidecar;
mod task_commands;

use std::sync::Arc;

use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, RunEvent, WindowEvent};

use overlay::{
    delete_personalization_overlay, list_personalization_overlays, read_personalization_overlay,
    write_personalization_overlay,
};
use sidecar::{
    ensure_api_server, kill_sidecar, terminate_child, ApiServerState, SharedApi, SIDECAR_GRACE,
};
use task_commands::{cancel_task, check_health, list_tasks, send_message};

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
            list_personalization_overlays,
            delete_personalization_overlay,
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
            // Real quit path: Cmd+Q, the app-menu Quit item, or the tray
            // "Quit" item (which reaps the sidecar itself, then calls
            // `AppHandle::exit`, which re-raises `ExitRequested`).
            //
            // #3372: the previous fast path only called `child.start_kill()`
            // and returned *without awaiting the reap* — and did no graceful
            // shutdown — so the `tagent --api` sidecar could outlive the GUI
            // and keep holding its fixed port. Graceful-then-forced
            // termination is inherently async (SIGTERM, then wait up to a
            // grace window, then SIGKILL), so whenever there is a child to
            // reap we defer the real exit: call `prevent_exit()` synchronously
            // *before* this closure returns (the runtime checks for a pending
            // `prevent_exit` via a synchronous `try_recv()` immediately after
            // this closure returns — a bare fire-and-forget spawn would lose
            // that race and let the process exit first), reap the sidecar
            // asynchronously, then trigger the real exit ourselves.
            //
            // Crucially we only defer when a child is actually present. Our own
            // `handle.exit(0)` below re-raises `ExitRequested`; by then the
            // child slot is empty (`take()`), so this branch does nothing and
            // the process exits — which is what breaks the re-entrant loop.
            if let Some(state) = app_handle.try_state::<SharedApi>() {
                match state.child.try_lock() {
                    Ok(mut guard) => {
                        if let Some(child) = guard.take() {
                            // Release the lock before awaiting the (up to
                            // `SIDECAR_GRACE`) termination.
                            drop(guard);
                            api.prevent_exit();
                            let handle = app_handle.clone();
                            tauri::async_runtime::spawn(async move {
                                terminate_child(child, SIDECAR_GRACE).await;
                                handle.exit(0);
                            });
                        }
                        // else: no child (already reaped by the tray "Quit"
                        // path, or the re-entrant exit above) — let the exit
                        // proceed.
                    }
                    Err(_) => {
                        // Lock contended by a concurrent
                        // `ensure_api_server`/`kill_sidecar`. Defer and reap
                        // via the shared path, which re-takes the lock itself.
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
