//! Shared application state for the trusty-code (tcode) Tauri shell.
//!
//! Why: the Rust side holds the facts only it knows — the base URL the webview
//! fetches, the credential that opens the bridge, and (since #10166) the
//! in-process router both transports dispatch into plus the live event
//! streams. Before #6637 the first two were guesses about ANOTHER process; now
//! the shell mints the credential and builds the router itself, so there is
//! one source for each and nothing to pin.
//!
//! What: [`GuiState`] carries the bridge base URL, its per-launch token, the
//! router, and the [`StreamRegistry`], registered once via
//! `tauri::Manager::manage`.
//! Test: `state::tests::state_echoes_what_the_bridge_built`.

use crate::bridge::channel::StreamRegistry;

/// Managed Tauri state — the bridge's address, credential, router and streams.
///
/// Why these live here rather than being re-derived per IPC call: the webview
/// must get exactly the strings [`crate::bridge::serve::start`] returned, and
/// the stream command must dispatch into the same router — and so the same
/// ticket table — the scheme handler uses.
pub struct GuiState {
    /// Base URL of the bridge (no trailing slash) — the scheme origin.
    pub bridge_url: String,
    /// Credential the webview attaches to every bridge request.
    ///
    /// Held in memory for the app's life and never written to disk: nothing
    /// outside this process needs to discover it.
    pub bridge_token: String,
    /// The guarded router the stream command opens streams through.
    pub router: axum::Router,
    /// The event streams the webview has open, by id.
    pub streams: StreamRegistry,
}

impl GuiState {
    /// Build state around a built bridge.
    ///
    /// Why it takes the parts rather than the [`crate::bridge::serve::Bridge`]
    /// itself: `crate::run` hands the router to the scheme handler too, and a
    /// test can construct state from plain values.
    #[must_use]
    pub fn new(bridge_url: String, bridge_token: String, router: axum::Router) -> Self {
        Self {
            bridge_url: bridge_url.trim_end_matches('/').to_string(),
            bridge_token,
            router,
            streams: StreamRegistry::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the webview's whole access is these two strings; a rewritten base
    /// would no longer match the scheme origin `daemon-auth.ts` compares before
    /// attaching the credential at all.
    /// Test: this is the test.
    #[test]
    fn state_echoes_what_the_bridge_built() {
        let state = GuiState::new(
            "tcode-bridge://localhost/".to_string(),
            "t".repeat(64),
            axum::Router::new(),
        );
        assert_eq!(state.bridge_url, "tcode-bridge://localhost");
        assert_eq!(state.bridge_token, "t".repeat(64));
        assert_eq!(state.streams.live(), 0);
    }
}
