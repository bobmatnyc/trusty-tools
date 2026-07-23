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
use sidecar::{ensure_api_server, kill_sidecar, ApiServerState, SharedApi};
use task_commands::{cancel_task, check_health, clear_recent_tasks, list_tasks, send_message};

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
            clear_recent_tasks,
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
                        // Just trigger the real exit. The sidecar reap is
                        // centralized in the synchronous `RunEvent::ExitRequested`
                        // handler below (which this `exit` raises), so every quit
                        // path — tray, Cmd+Q, app-menu — reaps through one place.
                        app.exit(0);
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
        // Reap the `tagent --api` sidecar on the way out. We match BOTH
        // `ExitRequested` and `Exit` deliberately.
        //
        // #3734 root cause: PR #3728 handled ONLY `RunEvent::ExitRequested`, and
        // deferred the reap onto an async task (`prevent_exit()` +
        // `tauri::async_runtime::spawn` + `handle.exit(0)`). In this app's TRAY
        // configuration the window is never destroyed on Cmd+Q (CloseRequested
        // only hides it), and tao 0.35.2's macOS backend maps a Cmd+Q with no
        // window teardown to `applicationWillTerminate:` — i.e. it emits
        // `RunEvent::Exit`, NOT `ExitRequested`. So #3728's `ExitRequested`-only
        // handler NEVER FIRED on Cmd+Q; the process exited in ~3ms with zero
        // Rust log lines and the sidecar was orphaned (reparented to launchd,
        // still holding port 8765). Two independent bugs stacked: the wrong
        // event was matched, and even the matched path deferred work that AppKit
        // never let run.
        //
        // The fix: handle `Exit` too, and reap SYNCHRONOUSLY — block this
        // handler on `kill_sidecar` for a short bounded window (`SIDECAR_GRACE`,
        // 500ms) so SIGTERM→(bounded wait)→SIGKILL completes BEFORE control
        // returns to AppKit. `block_on` is safe here: this closure runs on the
        // main event-loop thread, not a Tokio worker. `kill_sidecar` `take()`s
        // the child, so if both events fire the second is a no-op. The sidecar's
        // own parent-death watchdog (#3734) backstops anything this still misses
        // (a crash, SIGKILL, or lock contention here).
        //
        // The eprintln is intentional and load-bearing for verification: a
        // packaged-app Cmd+Q must show this line in the console, proving the
        // reap branch actually ENTERS — a released port alone is insufficient
        // evidence, because the watchdog would free the port even if this
        // handler never ran (see the PR's manual-verification note).
        let should_reap = matches!(event, RunEvent::ExitRequested { .. } | RunEvent::Exit);
        if should_reap {
            if let Some(state) = app_handle.try_state::<SharedApi>() {
                eprintln!("[trusty-agents-ui] quit event: reaping tagent sidecar (bounded)");
                let state = state.inner().clone();
                tauri::async_runtime::block_on(async move {
                    kill_sidecar(&state).await;
                });
                eprintln!("[trusty-agents-ui] quit event: sidecar reap complete");
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
