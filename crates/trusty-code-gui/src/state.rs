//! Shared application state for the trusty-code (tcode) Tauri shell.
//!
//! Why: the Rust side holds the two facts only it knows — where this launch's
//! HTTP bridge listens and what opens it. Before #6637 both were guesses about
//! ANOTHER process: a hardcoded `127.0.0.1:7882` mirroring the daemon's default
//! port, and a `0600` token file the daemon wrote and this shell read. Both
//! needed cross-crate pinning tests because nothing tied the two literals
//! together at compile time. Now the shell mints the credential and binds the
//! port itself, so there is one source for each and nothing to pin.
//!
//! What: [`GuiState`] carries the bridge base URL and its per-launch token,
//! registered once via `tauri::Manager::manage`.
//! Test: `state::tests::state_echoes_what_the_bridge_bound`.

/// Managed Tauri state — the bridge's address and credential.
///
/// Why both live here rather than being re-derived per IPC call: the webview
/// must get exactly the strings [`crate::bridge::serve::start`] returned, and a
/// second derivation of either is how the two ends stop agreeing.
pub struct GuiState {
    /// Base URL of the in-process HTTP bridge (no trailing slash).
    pub bridge_url: String,
    /// Credential the webview attaches to every bridge request.
    ///
    /// Held in memory for the app's life and never written to disk: nothing
    /// outside this process needs to discover it, so there is no file for
    /// anything else on this machine to read.
    pub bridge_token: String,
}

impl GuiState {
    /// Build state around a bound bridge.
    ///
    /// Why it takes the two strings rather than the [`crate::bridge::serve::Bridge`]
    /// itself: this struct is what the IPC commands read, and keeping it to two
    /// plain fields lets a test construct one without binding a port.
    #[must_use]
    pub fn new(bridge_url: String, bridge_token: String) -> Self {
        Self {
            bridge_url: bridge_url.trim_end_matches('/').to_string(),
            bridge_token,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the webview's whole access is these two strings; a trimmed or
    /// rewritten value would produce a base URL that no longer matches the
    /// listener's origin, which `daemon-auth.ts` compares before attaching the
    /// credential at all.
    /// Test: this is the test.
    #[test]
    fn state_echoes_what_the_bridge_bound() {
        let state = GuiState::new("http://127.0.0.1:54321/".to_string(), "t".repeat(64));
        assert_eq!(state.bridge_url, "http://127.0.0.1:54321");
        assert_eq!(state.bridge_token, "t".repeat(64));
    }
}
