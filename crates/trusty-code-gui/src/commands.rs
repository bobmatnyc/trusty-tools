//! Tauri IPC commands for the trusty-code (tcode) desktop shell.
//!
//! Why: Per DOC-39 §2.1 the daemon owns ALL logic and the GUI does no
//! client-side computation — that includes NOT proxying daemon data through
//! Rust. The frontend fetches session/health data directly from the daemon's
//! HTTP API (`fetch()`), the same way whether it runs inside Tauri or a
//! plain browser tab. The only thing the Rust side needs to expose is which
//! base URL the frontend should use, since only the native process can read
//! `TRUSTY_CODE_URL` — a web page cannot.
//! #5439 adds the second thing only the native process can supply: the
//! daemon's local-client credential. It lives in a `0600` file under the
//! daemon's data directory, which a webview cannot read and a plain browser
//! tab cannot reach at all — so the shell reads it and hands it over, exactly
//! as it already does for the URL.
//! What: `get_daemon_url` echoes `GuiState::daemon_url`; `get_daemon_token`
//! resolves the credential for that URL.
//! Test: `commands::tests::get_daemon_url_echoes_state`,
//! `commands::tests::daemon_token_is_withheld_from_a_non_loopback_url`.

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

/// Client-side override naming the credential directly, ahead of the token
/// file — the same variable `tcode tui` honours.
///
/// Why: the shell may run where it cannot read the daemon's data directory,
/// and an operator driving a forwarded daemon has no file to read at all.
pub const DAEMON_TOKEN_ENV: &str = "TCODE_DAEMON_TOKEN";

/// Return the daemon credential the frontend must attach to every request, or
/// an empty string when there is none to give.
///
/// Why (#5439): every daemon route but `/health` now requires
/// `Authorization: Bearer <token>`, and `fetch()` inside a webview has no way
/// to read a `0600` file. This command is the bridge. An empty string rather
/// than an error because "no credential available" is a state the frontend
/// must render (the daemon is not running, or it is not ours), not an
/// exception — and it keeps the IPC signature a plain `String`.
/// What: [`DAEMON_TOKEN_ENV`] when set and non-empty, else
/// `trusty_common::daemon_token::read_token`. The credential is withheld
/// entirely when `daemon_url` is not loopback, so pointing `TRUSTY_CODE_URL`
/// at a remote host cannot make the shell disclose the local machine's token.
/// Test: `commands::tests::daemon_token_is_withheld_from_a_non_loopback_url`,
/// `commands::tests::daemon_token_reads_the_env_override_for_loopback`.
#[tauri::command]
pub fn get_daemon_token(state: State<'_, GuiState>) -> String {
    daemon_token_for(&state.daemon_url)
}

/// The logic under [`get_daemon_token`], taking the URL directly so tests
/// need no `tauri::State` (which has no public constructor outside a running
/// `App`).
fn daemon_token_for(daemon_url: &str) -> String {
    // The classifier is trusty-common's, shared with the daemon's own origin
    // guard — a second "is this loopback" spelling here is exactly what would
    // drift and start disclosing the token off-host.
    if !trusty_common::server::origin_is_loopback(daemon_url) {
        return String::new();
    }
    if let Ok(raw) = std::env::var(DAEMON_TOKEN_ENV) {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    trusty_common::daemon_token::read_token(crate::state::TOKEN_APP_NAME).unwrap_or_default()
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

    /// #5439's credential-exfiltration guard, GUI side: pointing
    /// `TRUSTY_CODE_URL` at a remote host must not make the shell hand the
    /// local machine's token to the frontend, which would then send it there.
    ///
    /// This is the arm that fails open if the gate is dropped — it asserts an
    /// empty result even with an override set in the environment.
    #[test]
    #[serial_test::serial]
    fn daemon_token_is_withheld_from_a_non_loopback_url() {
        // SAFETY: test-only env mutation; serialized against the sibling test
        // that reads the same variable.
        unsafe {
            std::env::set_var(DAEMON_TOKEN_ENV, "a".repeat(64));
        }
        let remote = daemon_token_for("http://example.test:7882");
        let local = daemon_token_for("http://127.0.0.1:7882");
        unsafe {
            std::env::remove_var(DAEMON_TOKEN_ENV);
        }
        assert_eq!(remote, "", "a remote daemon must get no credential");
        assert_eq!(local, "a".repeat(64), "loopback must get the credential");
    }

    /// The env override must beat the token file, so a shell that cannot read
    /// the daemon's data directory can still be pointed at a credential.
    #[test]
    #[serial_test::serial]
    fn daemon_token_reads_the_env_override_for_loopback() {
        // SAFETY: test-only env mutation; serialized as above.
        unsafe {
            std::env::set_var(DAEMON_TOKEN_ENV, "b".repeat(64));
        }
        let resolved = daemon_token_for("http://localhost:7882");
        unsafe {
            std::env::remove_var(DAEMON_TOKEN_ENV);
        }
        assert_eq!(resolved, "b".repeat(64));
    }
}
