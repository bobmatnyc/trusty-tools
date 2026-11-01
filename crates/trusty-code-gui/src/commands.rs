//! Tauri IPC commands for the trusty-code (tcode) desktop shell.
//!
//! Why: the webview needs facts it cannot work out for itself — the bridge's
//! base URL and the credential that opens it — and, since #10166, a way to open
//! an event stream, which a custom-scheme `fetch()` cannot carry. Only the
//! native side has any of these, and IPC is how it says so.
//!
//! Why `get_daemon_url` and `get_daemon_token` kept their NAMES: they are what
//! `api-config.ts` and `daemon-auth.ts` invoke, and from the webview's side they
//! still mean "the base URL to `fetch()`" and "the credential to attach".
//!
//! What: [`get_daemon_url`] and [`get_daemon_token`] echo [`GuiState`];
//! [`open_daemon_stream`] and [`close_daemon_stream`] drive its
//! [`crate::bridge::channel::StreamRegistry`].
//! Test: `commands::tests::the_commands_echo_the_bridge_the_shell_built`; the
//! stream commands are thin wrappers over `bridge::channel`, covered there and
//! in `tests/bridge_uds.rs`.

use tauri::State;
use tauri::ipc::Channel;

use crate::bridge::channel::StreamEvent;
use crate::state::GuiState;

/// Return the bridge base URL the webview must `fetch()` against.
///
/// Why the webview is told rather than hard-coding it: the scheme origin is the
/// Rust side's to name, and one source keeps both ends agreeing.
/// What: echoes `GuiState::bridge_url` — `tcode-bridge://localhost` (#10166).
/// Delegates to [`bridge_url_from`] so the logic is testable without a
/// `tauri::State`, whose constructor is private outside a running `App`.
/// Test: `commands::tests::the_commands_echo_the_bridge_the_shell_built`.
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
/// credential is minted per launch and held only in this process's memory.
/// What: echoes `GuiState::bridge_token`.
/// Test: `commands::tests::the_commands_echo_the_bridge_the_shell_built`.
#[tauri::command]
pub fn get_daemon_token(state: State<'_, GuiState>) -> String {
    bridge_token_from(&state)
}

/// Read `GuiState::bridge_token` — the logic under [`get_daemon_token`].
fn bridge_token_from(state: &GuiState) -> String {
    state.bridge_token.clone()
}

/// Open one event stream and feed it into `on_event`; return its id.
///
/// Why (#10166): a custom-scheme response cannot stream, so the two event
/// routes reach the webview through a Tauri `Channel` instead.
/// What: `GET {path}?ticket={ticket}` through the bridge router, then each SSE
/// event as a [`StreamEvent`]. A failed send — the webview was destroyed — stops
/// the pump; a page reload closes every stream through `StreamRegistry::close_all`
/// (#10166).
///
/// # Errors
///
/// The stream did not open: a malformed path or ticket, or the router's
/// refusal (`401` for a spent ticket, `502` for an absent daemon socket).
///
/// Test: `bridge::channel::tests::a_refused_open_is_an_error_not_an_empty_stream`.
#[tauri::command]
pub async fn open_daemon_stream(
    state: State<'_, GuiState>,
    path: String,
    ticket: String,
    on_event: Channel<StreamEvent>,
) -> Result<u64, String> {
    state
        .streams
        .open(state.router.clone(), &path, &ticket, move |event| {
            on_event.send(event).is_ok()
        })
        .await
}

/// Stop the stream `id`. `false` when it had already ended.
///
/// Test: `bridge::channel::tests::a_closed_stream_is_aborted_and_forgotten`.
#[tauri::command]
pub fn close_daemon_stream(state: State<'_, GuiState>, id: u64) -> bool {
    state.streams.close(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: these two strings are the whole of the webview's access. An echo
    /// that rewrote either would leave every `fetch()` failing with a `401` or
    /// an unknown scheme, and no hint which.
    /// Test: this is the test.
    #[test]
    fn the_commands_echo_the_bridge_the_shell_built() {
        let state = GuiState::new(
            "tcode-bridge://localhost".to_string(),
            "a".repeat(64),
            axum::Router::new(),
        );
        assert_eq!(bridge_url_from(&state), "tcode-bridge://localhost");
        assert_eq!(bridge_token_from(&state), "a".repeat(64));
    }
}
