//! Tauri IPC commands for the trusty-code (tcode) desktop shell.
//!
//! Why: Per DOC-39 §2.1 the daemon owns ALL logic and the GUI does no
//! client-side computation — that includes NOT proxying daemon data through
//! Rust. The frontend fetches session/health data directly from the daemon's
//! HTTP API (`fetch()`), the same way whether it runs inside Tauri or a
//! plain browser tab. The only thing the Rust side needs to expose is which
//! base URL the frontend should use, since only the native process can read
//! `TRUSTY_CODE_URL` — a web page cannot.
//! What: `get_daemon_url`, a single command echoing `GuiState::daemon_url`.
//! Test: `commands::tests::get_daemon_url_echoes_state`.

use tauri::State;

use crate::state::GuiState;

/// Return the configured daemon base URL.
///
/// Why: The frontend's transport layer needs to know where to `fetch()`
/// against; in Tauri mode it must ask the Rust side (which can read
/// `TRUSTY_CODE_URL`) rather than assume a hardcoded default.
/// What: Echoes `GuiState::daemon_url`.
/// Test: Invoke with `TRUSTY_CODE_URL` unset → returns the default URL.
#[tauri::command]
pub fn get_daemon_url(state: State<'_, GuiState>) -> String {
    state.daemon_url.clone()
}
