//! Shared application state for the trusty-code (tcode) Tauri shell.
//!
//! Why: DOC-39 §2.1's thin-client rule means the Rust side must not compute
//! or cache daemon data itself — the ONLY thing it needs to hold is which
//! daemon base URL the frontend should talk to. Keeping that in one managed
//! struct avoids re-reading the env var on every IPC call.
//! What: `GuiState` carries the resolved tcode daemon base URL; it is
//! registered once via `tauri::Manager::manage`.
//! Test: Construct `GuiState::new()` with `TRUSTY_CODE_URL` unset and assert
//! `daemon_url` equals `DEFAULT_DAEMON_URL`; set the env var and assert it is
//! honored (see `state::tests`).

/// Default daemon REST endpoint when `TRUSTY_CODE_URL` is not set.
///
/// Why: `trusty-code serve --http` binds `serve::DEFAULT_HTTP_PORT` (7882,
/// `crates/trusty-code/src/serve/mod.rs`) when `--port` is omitted; the GUI
/// mirrors that default so a freshly started daemon and a freshly launched
/// GUI agree without any configuration. Previously `7881`, which collided
/// with `trusty-mpm`'s supervisor metrics listener — see
/// `crate::serve::DEFAULT_HTTP_PORT`'s docs and
/// `docs/architecture/port-assignments.md` (#3364). This literal and
/// `trusty-code::serve::DEFAULT_HTTP_PORT` must move in lockstep; there is no
/// shared crate between the GUI and the daemon binary to enforce it at
/// compile time, so `default_daemon_url_matches_tcode_default_http_port`
/// pins both values in the same assertion.
pub const DEFAULT_DAEMON_URL: &str = "http://127.0.0.1:7882";

/// Managed Tauri state — just the daemon base URL.
///
/// Why: every actual data fetch happens from the frontend via `fetch()`
/// against the daemon's HTTP API (thin client, DOC-39 §2.1) — the Rust side
/// never proxies session/event data, so there is no HTTP client to hold here.
pub struct GuiState {
    /// Base URL of the trusty-code daemon HTTP API (no trailing slash).
    pub daemon_url: String,
}

impl GuiState {
    /// Build state from the environment.
    ///
    /// Why: Lets ops point the desktop app at a non-local daemon without a
    /// rebuild by exporting `TRUSTY_CODE_URL`.
    /// What: Reads `TRUSTY_CODE_URL` (falling back to the default), trims any
    /// trailing slash.
    /// Test: Unset the env var → `daemon_url == DEFAULT_DAEMON_URL`.
    pub fn new() -> Self {
        let daemon_url = std::env::var("TRUSTY_CODE_URL")
            .unwrap_or_else(|_| DEFAULT_DAEMON_URL.to_string())
            .trim_end_matches('/')
            .to_string();
        Self { daemon_url }
    }
}

impl Default for GuiState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: pins the documented default so a future daemon port change is a
    /// deliberate, visible edit here rather than a silent drift.
    /// What: with `TRUSTY_CODE_URL` unset, `GuiState::new().daemon_url` must
    /// equal `DEFAULT_DAEMON_URL`.
    #[test]
    fn default_url_when_env_unset() {
        // Safety: test runs single-threaded per-process env mutation is fine
        // for this crate's tiny test suite.
        unsafe {
            std::env::remove_var("TRUSTY_CODE_URL");
        }
        assert_eq!(GuiState::new().daemon_url, DEFAULT_DAEMON_URL);
    }

    /// Why: confirms the override path ops rely on to point the desktop app
    /// at a non-local daemon.
    /// What: setting `TRUSTY_CODE_URL` with a trailing slash yields a
    /// trimmed `daemon_url`.
    #[test]
    fn env_override_is_trimmed() {
        unsafe {
            std::env::set_var("TRUSTY_CODE_URL", "http://example.test:9999/");
        }
        assert_eq!(GuiState::new().daemon_url, "http://example.test:9999");
        unsafe {
            std::env::remove_var("TRUSTY_CODE_URL");
        }
    }

    /// Cross-crate default-port pinning contract (#3364).
    ///
    /// Why: `DEFAULT_DAEMON_URL` is a hardcoded literal — nothing at compile
    /// time ties it to `trusty_code::serve::DEFAULT_HTTP_PORT`, the value it
    /// must mirror. The two were picked independently once already (both
    /// landed on `7881`, which then collided with `trusty-mpm`'s supervisor
    /// metrics listener); a future edit to one without the other would
    /// silently reintroduce a "DAEMON UNREACHABLE" GUI regression. This test
    /// parses the port back out of `DEFAULT_DAEMON_URL` and asserts it
    /// equals the daemon crate's own constant, so drift fails CI instead of
    /// shipping.
    /// What: asserts `DEFAULT_DAEMON_URL` embeds
    /// `trusty_code::serve::DEFAULT_HTTP_PORT` exactly.
    /// Test: this is the test.
    #[test]
    fn default_daemon_url_matches_tcode_default_http_port() {
        let expected = format!("http://127.0.0.1:{}", trusty_code::serve::DEFAULT_HTTP_PORT);
        assert_eq!(
            DEFAULT_DAEMON_URL, expected,
            "trusty-code-gui::DEFAULT_DAEMON_URL must mirror \
             trusty_code::serve::DEFAULT_HTTP_PORT exactly"
        );
    }
}
