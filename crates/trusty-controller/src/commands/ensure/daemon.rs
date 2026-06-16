//! Daemon address resolution + a thin HTTP client for the ensure stages.
//!
//! Why: the project-setup stages (index-register, palace-create) and the
//! `--wait` readiness poll all talk to the trusty-search and trusty-memory HTTP
//! daemons over loopback. Centralising "where is the daemon?" (the `http_addr`
//! file written by each daemon) and "issue a short-timeout request" here keeps
//! `project_setup.rs` and `readiness.rs` focused on their logic, and gives one
//! place to configure timeouts so a dead daemon never blocks `ensure`.
//!
//! What: [`resolve_base_url`] reads the daemon's recorded `host:port` via
//! `trusty_common::read_daemon_addr` and returns `http://<addr>`;
//! [`build_client`] makes a `reqwest::Client` with tight timeouts;
//! [`health_ok`] probes `GET {base}/health`.
//!
//! Test: `resolve_base_url` is exercised against a stubbed data dir in
//! `tests`; `build_client` is asserted to build; `health_ok` is covered by the
//! readiness-module integration test that stands up a stub server.

use std::time::Duration;

use anyhow::{Context, Result};

/// trusty-search daemon app name (the data-dir / `http_addr` key).
///
/// Why: `read_daemon_addr` is keyed by the daemon's app name; naming the two
/// daemons as constants keeps the strings from drifting across modules.
/// What: `"trusty-search"`.
/// Test: used by [`resolve_base_url`] callers; trivial.
pub const SEARCH_APP: &str = "trusty-search";

/// trusty-memory daemon app name (the data-dir / `http_addr` key).
///
/// Why: see [`SEARCH_APP`].
/// What: `"trusty-memory"`.
/// Test: used by [`resolve_base_url`] callers; trivial.
pub const MEMORY_APP: &str = "trusty-memory";

/// Per-request timeout for ensure's daemon calls.
///
/// Why: a generous-but-bounded request timeout — index registration may do real
/// work server-side, but a hung daemon must not block `ensure` indefinitely.
/// What: 10 seconds.
/// Test: indirectly via the stub-server integration tests.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Resolve a daemon's base URL from its recorded `http_addr` file.
///
/// Why: each daemon writes its bound `host:port` to its data dir on startup;
/// `ensure` must read that to know where to send requests, and distinguish
/// "daemon never started" (no file) from a real I/O error.
/// What: calls `trusty_common::read_daemon_addr(app)`; returns
/// `Ok(Some("http://<addr>"))` when the file exists and is non-empty,
/// `Ok(None)` when the daemon has no recorded address (not running), and `Err`
/// on an underlying I/O error.
/// Test: `tests::resolve_base_url_*` under a stubbed data dir.
pub fn resolve_base_url(app: &str) -> Result<Option<String>> {
    let addr = trusty_common::read_daemon_addr(app)
        .with_context(|| format!("read {app} daemon address"))?;
    Ok(addr
        .map(|a| a.trim().to_owned())
        .filter(|a| !a.is_empty())
        .map(|a| format!("http://{a}")))
}

/// Build the `reqwest::Client` used for ensure's daemon calls.
///
/// Why: one audited place for the timeout config so every ensure request fails
/// fast against a dead daemon instead of hanging on the OS TCP timeout.
/// What: a client with [`REQUEST_TIMEOUT`] applied to the whole request.
/// Test: `tests::build_client_succeeds`.
pub fn build_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .context("build ensure HTTP client")
}

/// Probe `GET {base_url}/health` and report whether it returns 2xx.
///
/// Why: the readiness poll needs a liveness check that returns quickly whether
/// the daemon is up; reusing one helper keeps the probe identical for both
/// daemons.
/// What: issues the GET with `client` and returns `true` only on a 2xx response;
/// any transport error or non-2xx status returns `false`.
/// Test: covered by the readiness-module stub-server test.
pub async fn health_ok(client: &reqwest::Client, base_url: &str) -> bool {
    let url = format!("{base_url}/health");
    matches!(client.get(&url).send().await, Ok(r) if r.status().is_success())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::ensure::ENV_TEST_LOCK as ENV_LOCK;
    use trusty_common::DATA_DIR_OVERRIDE_ENV;

    /// Why: a daemon with a recorded address must resolve to its `http://` base.
    /// What: writes an `http_addr` under a stubbed data dir and asserts the URL.
    /// Test: This is the test.
    #[test]
    fn resolve_base_url_present() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = std::env::temp_dir().join(format!(
            "tctl-ensure-daemon-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        unsafe {
            std::env::set_var(DATA_DIR_OVERRIDE_ENV, &tmp);
        }
        let app = format!("tctl-ensure-app-{}", std::process::id());
        trusty_common::write_daemon_addr(&app, "127.0.0.1:54321").unwrap();
        let got = resolve_base_url(&app);
        unsafe {
            std::env::remove_var(DATA_DIR_OVERRIDE_ENV);
        }
        let _ = std::fs::remove_dir_all(&tmp);
        assert_eq!(got.unwrap().as_deref(), Some("http://127.0.0.1:54321"));
    }

    /// Why: a daemon that never started has no `http_addr`; the resolver must
    /// return `None` (not an error) so the caller can report "daemon down".
    /// What: a fresh data dir with no addr file → `Ok(None)`.
    /// Test: This is the test.
    #[test]
    fn resolve_base_url_absent_is_none() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = std::env::temp_dir().join(format!(
            "tctl-ensure-daemon-absent-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        unsafe {
            std::env::set_var(DATA_DIR_OVERRIDE_ENV, &tmp);
        }
        let app = format!("tctl-ensure-absent-{}", std::process::id());
        let got = resolve_base_url(&app);
        unsafe {
            std::env::remove_var(DATA_DIR_OVERRIDE_ENV);
        }
        let _ = std::fs::remove_dir_all(&tmp);
        assert!(got.unwrap().is_none());
    }

    /// Why: the client builder must succeed in a normal environment.
    /// What: builds the client and asserts `Ok`.
    /// Test: This is the test.
    #[test]
    fn build_client_succeeds() {
        assert!(build_client().is_ok());
    }
}
