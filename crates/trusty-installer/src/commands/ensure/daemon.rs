//! Daemon address resolution + a thin HTTP client for the ensure stages.
//!
//! Why: the project-setup stages (index-register, palace-create) and the
//! `--wait` readiness poll talk to trusty-search and trusty-memory.
//! Centralising "where is the daemon?" and "issue a short-timeout request" here
//! keeps `project_setup.rs` and `readiness.rs` focused on their logic, and
//! gives one place to configure timeouts so a dead daemon never blocks
//! `ensure`.
//!
//! **The two daemons no longer share a transport (#6286).** trusty-search still
//! serves loopback HTTP and is still found through its `http_addr` file.
//! ADR-0032 moved trusty-memory onto a Unix socket, so `resolve_base_url`
//! answered `None` for it on every machine forever — and both callers read that
//! as "the daemon is not running": the palace stage no-opped and `--wait` never
//! reported ready. The memory half goes through [`memory_socket`] and
//! [`memory_serving`] instead, which derive the same path the daemon binds.
//!
//! What: [`resolve_base_url`] reads trusty-search's recorded `host:port` via
//! `trusty_common::read_daemon_addr` and returns `http://<addr>`;
//! [`build_client`] makes a `reqwest::Client` with tight timeouts;
//! [`health_ok`] probes `GET {base}/health`; [`memory_socket`] derives
//! trusty-memory's socket and [`memory_serving`] says whether anything answers
//! on it.
//!
//! Test: `resolve_base_url` is exercised against a stubbed data dir in
//! `tests`; `build_client` is asserted to build; `health_ok` is covered by the
//! readiness-module integration test that stands up a stub server;
//! `memory_serving_is_false_for_an_absent_socket`.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};

/// trusty-search daemon app name (the data-dir / `http_addr` key).
///
/// Why: `read_daemon_addr` is keyed by the daemon's app name; naming the two
/// daemons as constants keeps the strings from drifting across modules.
/// What: `"trusty-search"`.
/// Test: used by [`resolve_base_url`] callers; trivial.
pub const SEARCH_APP: &str = "trusty-search";

/// trusty-memory daemon app name (the data-dir key its socket is derived under).
///
/// Why: see [`SEARCH_APP`]. Since #6286 this keys the SOCKET path rather than an
/// `http_addr` file — `memory_rpc` derives it the same way the daemon does.
/// What: `"trusty-memory"`.
/// Test: used by [`memory_socket`] callers; trivial.
pub const MEMORY_APP: &str = "trusty-memory";

/// How long to wait for trusty-memory's socket to prove it is being served.
///
/// A local dial either connects or is refused immediately; the budget only
/// covers a loaded machine. Far below [`REQUEST_TIMEOUT`], because this is the
/// liveness question rather than a call that does work.
const MEMORY_PROBE_TIMEOUT: Duration = Duration::from_millis(750);

/// Per-call budget for a trusty-memory method that does real work.
///
/// Matches [`REQUEST_TIMEOUT`]: creating a palace opens redb and may embed, and
/// the reason for a ceiling is the same one — a hung daemon must not block
/// `ensure` indefinitely.
pub const MEMORY_CALL_TIMEOUT: Duration = REQUEST_TIMEOUT;

/// The socket trusty-memory binds, as both it and its consumers derive it.
///
/// Why: `resolve_base_url(MEMORY_APP)` is permanently `None` since ADR-0032 —
/// there is no listener to record an address and nothing writes the file — so
/// every caller that kept asking it read a running daemon as absent.
///
/// # Errors
///
/// When the data directory cannot be resolved or created. That is an
/// operator-fixable condition (permissions, a `TRUSTY_DATA_DIR_OVERRIDE`
/// pointing somewhere unusable), distinct from "the daemon is not running",
/// which this cannot and does not report — [`memory_serving`] answers that.
///
/// Test: `memory_serving_is_false_for_an_absent_socket` drives it.
pub fn memory_socket() -> Result<PathBuf> {
    trusty_common::memory_rpc::resolve_memory_socket()
        .context("resolve the trusty-memory socket path")
}

/// Is anything serving trusty-memory's socket?
///
/// Why: the UDS counterpart of [`health_ok`]. A bare connect rather than a
/// `memory.health` call, for the same reason `memory_daemon_is_serving` gives:
/// the question is whether the endpoint is live, and a daemon that is up but
/// degraded must not be reported absent.
/// Test: `memory_serving_is_false_for_an_absent_socket`.
pub async fn memory_serving(socket: &std::path::Path) -> bool {
    trusty_common::memory_rpc::memory_daemon_is_serving(socket, MEMORY_PROBE_TIMEOUT).await
}

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
/// fast against a dead daemon instead of hanging on the OS TCP timeout. Every
/// target is a loopback daemon, so it also has to be proxy-free: reqwest routes
/// `127.0.0.1` through an exported `HTTP_PROXY` and `tctl ensure --wait` then
/// waits out its whole budget against a daemon that is up (#4392).
/// What: the shared `trusty_common::http_client` loopback builder with
/// [`REQUEST_TIMEOUT`] applied to the whole request.
/// Test: `tests::build_client_succeeds`; the proxy immunity is proven in
/// `trusty_common::http_client::tests` and in
/// `probe_http_tests::probe_ignores_http_proxy_env`.
pub fn build_client() -> Result<reqwest::Client> {
    trusty_common::http_client::loopback_client_builder()
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
            // SAFETY: serialised by ENV_TEST_LOCK; no concurrent env access in this crate's tests.
            std::env::set_var(DATA_DIR_OVERRIDE_ENV, &tmp);
        }
        let app = format!("tctl-ensure-app-{}", std::process::id());
        trusty_common::write_daemon_addr(&app, "127.0.0.1:54321").unwrap();
        let got = resolve_base_url(&app);
        unsafe {
            // SAFETY: serialised by ENV_TEST_LOCK; no concurrent env access in this crate's tests.
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
            // SAFETY: serialised by ENV_TEST_LOCK; no concurrent env access in this crate's tests.
            std::env::set_var(DATA_DIR_OVERRIDE_ENV, &tmp);
        }
        let app = format!("tctl-ensure-absent-{}", std::process::id());
        let got = resolve_base_url(&app);
        unsafe {
            // SAFETY: serialised by ENV_TEST_LOCK; no concurrent env access in this crate's tests.
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

    /// Why: "the daemon is not running" is the branch both callers take to
    /// no-op, and it has to come from the socket rather than from a discovery
    /// file nothing writes. A probe that answered `true` for an absent path
    /// would make the palace stage attempt a call it cannot complete.
    /// What: probes a path nothing has ever bound and asserts `false`, promptly.
    /// Test: This is the test.
    #[tokio::test]
    async fn memory_serving_is_false_for_an_absent_socket() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let started = std::time::Instant::now();
        assert!(!memory_serving(&tmp.path().join("absent.sock")).await);
        assert!(
            started.elapsed() < MEMORY_PROBE_TIMEOUT,
            "a refused dial must not wait out the budget: {:?}",
            started.elapsed()
        );
    }
}
