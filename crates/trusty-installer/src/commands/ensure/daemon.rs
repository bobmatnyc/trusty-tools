//! Daemon socket resolution + liveness probes for the ensure stages.
//!
//! Why: the project-setup stages (index-register, palace-create) and the
//! `--wait` readiness poll talk to trusty-search and trusty-memory.
//! Centralising "where is the daemon?" and "is it serving?" here keeps
//! `project_setup.rs` and `readiness.rs` focused on their logic, and gives one
//! place to configure timeouts so a dead daemon never blocks `ensure`.
//!
//! **Both daemons are reached over their Unix sockets.** ADR-0032 moved
//! trusty-memory first (#6286); #9214 moves trusty-search too, ahead of the
//! final slice that drops its `:7878` bind. A stale `http_addr` file is never
//! read here.
//!
//! What: [`search_socket`] / [`memory_socket`] derive the path each daemon
//! binds; [`search_serving`] / [`memory_serving`] say whether anything answers
//! on it; [`search_healthy`] calls trusty-search's health method.
//!
//! Test: `memory_serving_is_false_for_an_absent_socket`,
//! `search_probes_are_false_for_an_absent_socket`; the stage and readiness
//! tests drive the rest against stub sockets.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};

/// trusty-search daemon app name (the data-dir key).
///
/// Why: naming the two daemons as constants keeps the strings from drifting
/// across modules. Since #9214 nothing here reads the `http_addr` file kept
/// under this key; the tests plant one to prove it stays unread.
/// What: `"trusty-search"`.
/// Test: `register_index_dials_the_socket_not_http_addr`.
pub const SEARCH_APP: &str = "trusty-search";

/// trusty-memory daemon app name (the data-dir key its socket is derived under).
///
/// Why: see [`SEARCH_APP`]. Since #6286 this keys the SOCKET path rather than an
/// `http_addr` file — `memory_rpc` derives it the same way the daemon does.
/// What: `"trusty-memory"`.
/// Test: used by [`memory_socket`] callers; trivial.
pub const MEMORY_APP: &str = "trusty-memory";

/// How long to wait for a daemon's socket to prove it is being served.
///
/// A local dial either connects or is refused immediately; the budget only
/// covers a loaded machine. Far below [`REQUEST_TIMEOUT`], because this is the
/// liveness question rather than a call that does work.
const SOCKET_PROBE_TIMEOUT: Duration = Duration::from_millis(750);

/// Per-call budget for a trusty-memory method that does real work.
///
/// Matches [`REQUEST_TIMEOUT`]: creating a palace opens redb and may embed, and
/// the reason for a ceiling is the same one — a hung daemon must not block
/// `ensure` indefinitely.
pub const MEMORY_CALL_TIMEOUT: Duration = REQUEST_TIMEOUT;

/// Per-call budget for a trusty-search method (#9214).
///
/// Matches [`REQUEST_TIMEOUT`], the budget the HTTP client it replaces had:
/// index registration may do real work, but a hung daemon must not block
/// `ensure` indefinitely.
pub const SEARCH_CALL_TIMEOUT: Duration = REQUEST_TIMEOUT;

/// The socket trusty-search binds, as both it and its consumers derive it.
///
/// Why (#9214): the `http_addr` file names a TCP listener that is going away.
/// Delegating to `trusty_common::search_rpc::search_socket` keeps one path
/// derivation, honouring `TRUSTY_SEARCH_SOCKET` and any later data-dir rule.
///
/// # Errors
///
/// When the data directory cannot be resolved or created — an
/// operator-fixable condition, distinct from "the daemon is not running",
/// which [`search_serving`] answers.
///
/// Test: `register_index_dials_the_socket_not_http_addr`.
pub fn search_socket() -> Result<PathBuf> {
    trusty_common::search_rpc::search_socket().context("resolve the trusty-search socket path")
}

/// Is anything serving trusty-search's socket?
///
/// Why: "the daemon is not running" must be a bare-connect verdict, so a
/// daemon that is up but slow to answer a method is never reported absent.
/// Test: `search_probes_are_false_for_an_absent_socket`,
/// `register_index_dead_or_missing_socket_is_skipped`.
pub async fn search_serving(socket: &std::path::Path) -> bool {
    trusty_common::uds::socket_is_serving(socket, SOCKET_PROBE_TIMEOUT).await
}

/// Does trusty-search answer its health method on `socket`?
///
/// Why (#9214): the socket counterpart of the retired `GET /health` probe.
/// The daemon serves both from the same `health_report`, so a result is the
/// same verdict a 2xx was.
/// What: `search.health` with `{}`; `true` on any result, `false` on a dial
/// failure, timeout or JSON-RPC error.
/// Test: `probe_ready_is_true_with_only_the_search_and_memory_sockets`,
/// `search_probes_are_false_for_an_absent_socket`.
pub async fn search_healthy(socket: &std::path::Path) -> bool {
    trusty_common::search_rpc::call_at(
        socket,
        trusty_common::search_rpc::METHOD_HEALTH,
        serde_json::json!({}),
        SEARCH_CALL_TIMEOUT,
    )
    .await
    .is_ok()
}

/// The socket trusty-memory binds, as both it and its consumers derive it.
///
/// Why: the retired `resolve_base_url(MEMORY_APP)` was `None` since ADR-0032 —
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
/// Why: the memory twin of [`search_serving`]. A bare connect rather than a
/// `memory.health` call, for the same reason `memory_daemon_is_serving` gives:
/// the question is whether the endpoint is live, and a daemon that is up but
/// degraded must not be reported absent.
/// Test: `memory_serving_is_false_for_an_absent_socket`.
pub async fn memory_serving(socket: &std::path::Path) -> bool {
    trusty_common::memory_rpc::memory_daemon_is_serving(socket, SOCKET_PROBE_TIMEOUT).await
}

/// Per-request timeout for ensure's daemon calls.
///
/// Why: a generous-but-bounded request timeout — index registration may do real
/// work server-side, but a hung daemon must not block `ensure` indefinitely.
/// What: 10 seconds.
/// Test: indirectly via the stub-server integration tests.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

#[cfg(test)]
mod tests {
    use super::*;

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
            started.elapsed() < SOCKET_PROBE_TIMEOUT,
            "a refused dial must not wait out the budget: {:?}",
            started.elapsed()
        );
    }

    /// Why (#9214): the index stage's skip and `--wait`'s not-ready both rest on
    /// these two answering `false` for a socket nothing serves — promptly.
    /// What: probes a path nothing has bound with both; asserts `false` inside
    /// one call budget.
    /// Test: This is the test.
    #[tokio::test]
    async fn search_probes_are_false_for_an_absent_socket() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let absent = tmp.path().join("absent.sock");
        let started = std::time::Instant::now();
        assert!(!search_serving(&absent).await);
        assert!(!search_healthy(&absent).await);
        assert!(
            started.elapsed() < SEARCH_CALL_TIMEOUT,
            "a refused dial must not wait out the budget: {:?}",
            started.elapsed()
        );
    }
}
