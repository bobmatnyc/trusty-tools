//! trusty-code (tcode) desktop shell (Tauri 2).
//!
//! Why: Provides a native window over the `tcode serve --http` daemon
//! (issue #2983, DOC-39 `docs/specs/trusty-code-harness-ui.md`). This is the
//! MINIMAL scaffold slice only — a compiling, runnable shell that the
//! subsequent UI slices build on as the REST gateway (#2983) lands. Mirrors
//! `crates/trusty-mpm-gui`'s structure (Tauri 2 + Svelte 5 + Vite) so the two
//! desktop shells share one set of conventions.
//! What: Registers `GuiState` (the daemon base URL) and the single
//! `get_daemon_url` IPC command, then runs the Tauri event loop. Per DOC-39
//! §2.1 (thin client) all actual daemon data — `GET /health` today, session
//! resources once the REST gateway lands — is fetched by the frontend
//! directly over HTTP, never proxied through Rust.
//! Test: `cargo check -p trusty-code-gui` passes once the crate is
//! registered in the workspace; launching the app shows the health status
//! fetched from the daemon.

mod commands;
mod state;

use state::GuiState;

/// Build and run the Tauri application.
///
/// Why: Single entry point shared by `main.rs` (and any future test harness)
/// so the builder configuration lives in one place.
/// What: Manages `GuiState` and registers `get_daemon_url`, then runs the
/// generated Tauri context.
/// Test: Invoked from `main`; app window opens and the frontend's health
/// panel shows connected/disconnected against a live `tcode serve --http`.
pub fn run() {
    tauri::Builder::default()
        .manage(GuiState::new())
        .invoke_handler(tauri::generate_handler![commands::get_daemon_url])
        .run(tauri::generate_context!())
        .expect("error while running trusty-code-gui");
}
