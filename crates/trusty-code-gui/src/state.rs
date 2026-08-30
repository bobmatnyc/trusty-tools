//! Shared application state for the trusty-code (tcode) Tauri shell.
//!
//! Why: DOC-39 §2.1's thin-client rule means the Rust side must not compute
//! or cache daemon data itself — the ONLY thing it needs to hold is which
//! daemon base URL the frontend should talk to. Keeping that in one managed
//! struct avoids re-reading the env var on every IPC call.
//! What: `GuiState` carries the resolved tcode daemon base URL; it is
//! registered once via `tauri::Manager::manage`.
//! `GuiState::new` reads `TRUSTY_CODE_URL` once and delegates to
//! `GuiState::from_url_override`, which resolves the value without touching
//! the process env — the seam that keeps the tests off shared global state
//! (#6310).
//! Test: `state::tests::{default_url_when_env_unset, env_override_is_trimmed,
//! default_daemon_url_matches_tcode_default_http_port}`.

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
    /// What: Reads `TRUSTY_CODE_URL` and hands the raw value to
    /// [`GuiState::from_url_override`], which owns the fallback and trimming.
    /// Test: `from_url_override`'s tests cover both branches; this function
    /// is the one-line env read above them.
    pub fn new() -> Self {
        // #6310: this is the ONLY read of the process env in this module, and
        // it is deliberately the whole of `new`. Tests drive
        // `from_url_override` directly rather than mutating `TRUSTY_CODE_URL`,
        // which raced across parallel test threads in one process.
        Self::from_url_override(std::env::var("TRUSTY_CODE_URL").ok())
    }

    /// Resolve the daemon base URL from an already-read `TRUSTY_CODE_URL`.
    ///
    /// Why: `cargo test` runs a target's tests as threads in ONE process, so
    /// a test that sets `TRUSTY_CODE_URL` and a sibling that removes it race
    /// each other — that race failed the required `trusty-code-gui clippy`
    /// check on unrelated PRs (#6310). Taking the value as an argument moves
    /// the env read to the single caller above and leaves the resolution
    /// logic pure, mirroring the `daemon_url_from` seam in
    /// `crate::commands`.
    /// What: `None` (or an unset var) yields [`DEFAULT_DAEMON_URL`]; any
    /// value has its trailing slashes trimmed.
    /// Test: `default_url_when_env_unset`, `env_override_is_trimmed`.
    fn from_url_override(raw: Option<String>) -> Self {
        let daemon_url = raw
            .unwrap_or_else(|| DEFAULT_DAEMON_URL.to_string())
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
    /// What: an unset `TRUSTY_CODE_URL` (`None`) must yield
    /// `DEFAULT_DAEMON_URL`.
    #[test]
    fn default_url_when_env_unset() {
        // #6310: the unset case is passed as `None` rather than produced by
        // `remove_var`, so this test cannot be perturbed by — or perturb — a
        // parallel sibling sharing the process env.
        assert_eq!(
            GuiState::from_url_override(None).daemon_url,
            DEFAULT_DAEMON_URL
        );
    }

    /// Why: confirms the override path ops rely on to point the desktop app
    /// at a non-local daemon.
    /// What: a `TRUSTY_CODE_URL` value with a trailing slash yields a trimmed
    /// `daemon_url`.
    #[test]
    fn env_override_is_trimmed() {
        // #6310: the override is passed as an argument, never written to the
        // process env, so `default_url_when_env_unset` can no longer clear it
        // between this call and its assertion.
        let state = GuiState::from_url_override(Some("http://example.test:9999/".to_string()));
        assert_eq!(state.daemon_url, "http://example.test:9999");
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
