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
/// What: Echoes `GuiState::daemon_url`. Delegates to `daemon_url_from` so the
/// logic is testable without constructing a `tauri::State` (its constructor
/// is private outside a running `App`).
/// Test: `commands::tests::get_daemon_url_echoes_state`.
#[tauri::command]
pub fn get_daemon_url(state: State<'_, GuiState>) -> String {
    daemon_url_from(&state)
}

/// Read `GuiState::daemon_url` — the logic under `get_daemon_url`, extracted
/// so it can be exercised directly in tests.
///
/// Why: `tauri::State` has no public constructor outside a running `App`, so
/// a unit test cannot build one to call `get_daemon_url` directly; this plain
/// function takes `&GuiState`, which the test constructs directly.
/// What: Clones and returns `state.daemon_url`.
/// Test: `commands::tests::get_daemon_url_echoes_state`.
fn daemon_url_from(state: &GuiState) -> String {
    state.daemon_url.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: pins the command's actual behavior — the frontend's health check
    /// and every future data fetch depend on getting the real configured URL
    /// back, not a stale or default one.
    /// What: constructs a `GuiState` with a known URL and asserts
    /// `daemon_url_from` echoes it unchanged.
    #[test]
    fn get_daemon_url_echoes_state() {
        let state = GuiState {
            daemon_url: "http://127.0.0.1:9999".to_string(),
        };
        assert_eq!(daemon_url_from(&state), "http://127.0.0.1:9999");
    }
}
