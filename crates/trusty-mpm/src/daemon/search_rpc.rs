//! The one way this crate calls the running trusty-search daemon (#6285).
//!
//! Why: `tm doctor`'s two search probes each resolved an address — the
//! `~/.trusty-search/http_addr` discovery file, else a compiled-in
//! `127.0.0.1:7878` — and issued a `reqwest` GET against it. ADR-0032 retires
//! that listener: trusty-search binds one hardened Unix socket at the path
//! [`trusty_common::daemon_socket_path`] derives and speaks the framed
//! JSON-RPC envelope [`trusty_common::uds`] defines. There is no address to
//! discover, no port to fall back to, and nothing for a stale `http_addr` left
//! by an older daemon to disagree with.
//!
//! What: [`search_socket`] derives the path both ends compute, and [`call_at`]
//! writes one frame and reads one back. [`SearchRpcError`] carries the daemon's
//! own code, so a caller can tell "no such index" — the whole point of the
//! pinned-index probe (#5045) — from a transport failure.
//!
//! **The method names are literals here, and deliberately so.** trusty-mpm has
//! no Cargo edge on trusty-search, so the names cannot be imported; they are
//! pinned by `trusty_search::service::socket::METHODS`, which the daemon's own
//! `rpc_router_registers_every_documented_method` compares its router against.
//! A name that drifted answers `method_not_found`, which every probe below
//! reports as an unhealthy daemon.
//!
//! Test: `search_socket_honours_the_env_override`,
//! `call_at_reports_a_dead_socket_rather_than_hanging`, and end-to-end through
//! `doctor_tests::search_*` / `doctor_search_pin_tests::pinned_but_missing_index_is_fail`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use serde_json::Value;

use trusty_common::uds::send_framed_request;
use trusty_common::uds::server::RpcResponse;

/// Environment variable that pins the daemon's socket path explicitly.
///
/// Why: it replaces `TRUSTY_SEARCH_ADDR`, which named a listener there no
/// longer is one. The affordance it provided is still wanted — a test rig
/// points a probe at a daemon it started on a temp path — and the alternative,
/// `TRUSTY_DATA_DIR_OVERRIDE`, is process-global and would redirect every other
/// trusty-* client in the same process along with this one. Same shape as
/// `trusty_common::memory_rpc::TRUSTY_MEMORY_SOCKET_ENV` and trusty-audit's
/// `TRUSTY_ANALYZE_SOCKET`.
pub const TRUSTY_SEARCH_SOCKET_ENV: &str = "TRUSTY_SEARCH_SOCKET";

/// The app name trusty-search derives its socket path under.
///
/// Matches the daemon's own `daemon_socket_path("trusty-search")` call
/// (`trusty_search::service::socket::socket_path`), which is why caller and
/// daemon compute the same path with nothing published between them.
const SEARCH_APP_NAME: &str = "trusty-search";

/// Liveness, index count and the daemon's version.
pub const METHOD_HEALTH: &str = "search.health";

/// Every registered index — the `details` flag widens each entry.
pub const METHOD_INDEXES_LIST: &str = "search.indexes.list";

/// One index's stages, capabilities and footprint. Answers
/// [`crate::daemon::error::CODE_NOT_FOUND`] for an id the daemon does not hold.
pub const METHOD_INDEX_STATUS: &str = "search.index.status";

/// The daemon answered, and what it answered was an error.
///
/// Why a typed error rather than a formatted string: the pinned-index probe has
/// to tell "the daemon has no such index" from "the call failed" — a 404 is a
/// definite, actionable verdict and a transport failure is an absence of
/// information (#5045). Carrying the code in the error keeps [`call_at`]'s
/// `Result<Value>` signature and makes only the caller that cares pay for it,
/// via `anyhow::Error::downcast_ref`.
#[derive(Debug, thiserror::Error)]
#[error("{method} failed: {message} ({code})")]
pub struct SearchRpcError {
    /// The method that was called.
    pub method: String,
    /// The daemon's own JSON-RPC error code.
    pub code: i64,
    /// The daemon's own message.
    pub message: String,
}

impl SearchRpcError {
    /// Did the daemon say the thing asked for does not exist?
    pub fn is_not_found(&self) -> bool {
        self.code == crate::daemon::error::CODE_NOT_FOUND
    }
}

/// Resolve the socket the trusty-search daemon binds.
///
/// # Errors
///
/// When the data directory cannot be resolved or created — an operator-fixable
/// condition, distinct from "the daemon is not running", which this function
/// cannot and does not report.
///
/// Test: `search_socket_honours_the_env_override`.
pub fn search_socket() -> Result<PathBuf> {
    if let Ok(raw) = std::env::var(TRUSTY_SEARCH_SOCKET_ENV) {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return Ok(PathBuf::from(trimmed));
        }
    }
    trusty_common::daemon_socket_path(SEARCH_APP_NAME)
}

/// Call one method on the daemon at `socket` and return its `result`.
///
/// # Errors
///
/// When the socket cannot be dialled — which is what "the daemon is not
/// running" looks like — or when the daemon answers with a JSON-RPC error,
/// whose code and message are carried through in a [`SearchRpcError`] so the
/// caller reports the reason it was given rather than a generic failure.
///
/// Test: `call_at_reports_a_dead_socket_rather_than_hanging`.
pub async fn call_at(
    socket: &Path,
    method: &str,
    params: Value,
    timeout: Duration,
) -> Result<Value> {
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    });

    let response: RpcResponse = send_framed_request(socket, &request, timeout)
        .await
        .with_context(|| {
            format!(
                "call {method} on the trusty-search daemon at {}",
                socket.display()
            )
        })?;

    match (response.result, response.error) {
        (Some(result), _) => Ok(result),
        (None, Some(e)) => Err(anyhow::Error::new(SearchRpcError {
            method: method.to_string(),
            code: e.code,
            message: e.message,
        })),
        // The daemon's own contract is that exactly one of the two is present.
        (None, None) => Err(anyhow!(
            "{method} answered with neither a result nor an error"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the override is the only way a probe can be pointed at a daemon a
    /// test started without redirecting every other trusty-* client in the
    /// process through `TRUSTY_DATA_DIR_OVERRIDE`.
    /// Test: itself.
    #[test]
    #[serial_test::serial]
    fn search_socket_honours_the_env_override() {
        unsafe {
            std::env::set_var(TRUSTY_SEARCH_SOCKET_ENV, "/tmp/example-search.sock");
        }
        let resolved = search_socket();
        unsafe {
            std::env::remove_var(TRUSTY_SEARCH_SOCKET_ENV);
        }
        assert_eq!(
            resolved.expect("an override always resolves"),
            PathBuf::from("/tmp/example-search.sock")
        );
    }

    /// Why: every probe's failure arm depends on a dead socket FAILING rather
    /// than consuming the budget — `tm doctor` must not hang on an absent
    /// daemon.
    /// Test: itself.
    #[tokio::test]
    async fn call_at_reports_a_dead_socket_rather_than_hanging() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("absent.sock");
        let err = call_at(
            &socket,
            METHOD_HEALTH,
            serde_json::json!({}),
            Duration::from_secs(5),
        )
        .await
        .expect_err("nothing is listening");
        assert!(
            err.downcast_ref::<SearchRpcError>().is_none(),
            "a dial failure is a transport error, never a daemon refusal: {err:#}"
        );
    }
}
