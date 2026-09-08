//! Tauri IPC commands for the trusty-code (tcode) desktop shell.
//!
//! Why: the webview needs two facts it cannot work out for itself — where this
//! launch's HTTP bridge listens (an ephemeral port, so there is no default to
//! assume) and what credential opens it (minted in memory, so there is no file
//! to read). Only the native side knows either, and IPC is how it says so.
//!
//! Why the two command NAMES did not change: `get_daemon_url` and
//! `get_daemon_token` are what `api-config.ts` and `daemon-auth.ts` invoke, and
//! what they mean from the webview's side is unchanged — "the base URL to
//! `fetch()`" and "the credential to attach". What moved is which listener
//! answers, which is not the webview's business.
//!
//! What: [`get_daemon_url`] echoes `GuiState::bridge_url`; [`get_daemon_token`]
//! echoes `GuiState::bridge_token`.
//! Test: `commands::tests::the_commands_echo_the_bridge_the_shell_bound`.

use tauri::State;

use crate::state::GuiState;

/// Return the bridge base URL the webview must `fetch()` against.
///
/// Why the webview cannot compute it: the bridge binds `127.0.0.1:0`, so the
/// port is assigned per launch and nothing on the page can discover it.
/// What: echoes `GuiState::bridge_url`. Delegates to [`bridge_url_from`] so the
/// logic is testable without constructing a `tauri::State`, whose constructor is
/// private outside a running `App`.
/// Test: `commands::tests::the_commands_echo_the_bridge_the_shell_bound`.
#[tauri::command]
pub fn get_daemon_url(state: State<'_, GuiState>) -> String {
    bridge_url_from(&state)
}

/// Read `GuiState::bridge_url` — the logic under [`get_daemon_url`].
fn bridge_url_from(state: &GuiState) -> String {
    state.bridge_url.clone()
}

/// Return the credential the webview must attach to every bridge request.
///
/// Why (#6637): every bridge route requires `Authorization: Bearer <token>`,
/// and `fetch()` inside a webview has no way to obtain one on its own. The
/// credential is minted per launch and held only in this process's memory —
/// there is no `0600` file, so the exfiltration guard the previous version
/// carried (withhold the token when the configured URL is not loopback) has
/// nothing left to guard: the URL is not configurable, and
/// [`crate::bridge::serve::start`] binds loopback or fails.
/// What: echoes `GuiState::bridge_token`.
/// Test: `commands::tests::the_commands_echo_the_bridge_the_shell_bound`.
#[tauri::command]
pub fn get_daemon_token(state: State<'_, GuiState>) -> String {
    bridge_token_from(&state)
}

/// Read `GuiState::bridge_token` — the logic under [`get_daemon_token`].
fn bridge_token_from(state: &GuiState) -> String {
    state.bridge_token.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: these two strings are the whole of the webview's access. An echo
    /// that rewrote either would leave the page unable to reach a listener that
    /// is running perfectly well, with a `401` or a connection refusal and no
    /// hint which.
    /// Test: this is the test.
    #[test]
    fn the_commands_echo_the_bridge_the_shell_bound() {
        let state = GuiState::new("http://127.0.0.1:54321".to_string(), "a".repeat(64));
        assert_eq!(bridge_url_from(&state), "http://127.0.0.1:54321");
        assert_eq!(bridge_token_from(&state), "a".repeat(64));
    }
}
