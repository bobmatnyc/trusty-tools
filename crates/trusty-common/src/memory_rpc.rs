//! The one way anything outside trusty-memory calls the running daemon (#6286).
//!
//! Why: this module used to resolve an address — `TRUSTY_MEMORY_URL`, else the
//! `http_addr` discovery file, else a guaranteed-dead placeholder — and POST a
//! JSON-RPC envelope to `{base}/rpc`. ADR-0032 retired that listener: since
//! #6286 trusty-memory binds one hardened Unix socket at the path
//! [`crate::daemon_socket_path`] derives, writes no discovery file, and speaks
//! the framed JSON-RPC envelope [`crate::uds`] defines. There is no address to
//! discover, no port to walk, and nothing for a stale file to disagree with.
//!
//! What: [`call_memory_tool`] derives the socket, writes one frame, reads one
//! back, and returns the envelope's `result`. [`call_memory_tool_at`] takes the
//! socket explicitly, for a caller that resolved it once and threads it through
//! (catch-up's `CatchupOptions::memory_socket`) or a test pointing at a daemon
//! it started itself.
//!
//! **This module is now what the monitor TUI's client and trusty-agents'
//! `TrustyMemoryClient` call too.** Both were independent REST clients against
//! `/api/v1/*` routes, and the doc comment here used to say so and disclaim
//! reusing them. #6286 folded both onto this function: the routes they targeted
//! no longer exist, and re-deriving a second socket client for each would be
//! the drift the workspace's common-entry-point rule exists to prevent.
//!
//! **This is the request/response half only.** `memory.chat` answers in many
//! frames; a caller that wants it uses
//! [`crate::uds::send_framed_stream_request_capped`] directly, because a stream
//! is not something these signatures can return.
//!
//! Test: `call_memory_tool_at_reports_a_dead_socket_rather_than_hanging`,
//! `resolve_memory_socket_honours_the_env_override`,
//! `resolve_memory_socket_or_unreachable_falls_back`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};

use crate::uds::send_framed_request_capped;
use crate::uds::server::RpcResponse;

/// Environment variable that pins the daemon's socket path explicitly.
///
/// Why: it replaces `TRUSTY_MEMORY_URL`, which named a base URL there is no
/// longer a listener for. The affordance it provided is still wanted — a test
/// rig or a CI job points a client at a daemon it started on a temp path — and
/// the alternative, `TRUSTY_DATA_DIR_OVERRIDE`, is process-global and would
/// redirect every other trusty-* client in the same process along with this one.
///
/// What: the literal env var name `TRUSTY_MEMORY_SOCKET`, read as a path.
/// Test: `resolve_memory_socket_honours_the_env_override`.
pub const TRUSTY_MEMORY_SOCKET_ENV: &str = "TRUSTY_MEMORY_SOCKET";

/// The app name trusty-memory derives its socket path under.
///
/// Matches the daemon's own `daemon_socket_path("trusty-memory")` call, which
/// is why caller and daemon compute the same path with nothing published
/// between them.
const MEMORY_APP_NAME: &str = "trusty-memory";

/// Largest frame this client reads or writes, in bytes.
///
/// Why not [`crate::uds::MAX_FRAME_BYTES`] (8 MiB): the daemon's own budget is
/// 32 MiB (`trusty_memory::transport::uds::MAX_FRAME_BYTES`), sized for whole-
/// palace KG dumps and 500-row activity pages, and the budget is symmetric —
/// a client that kept the shared default would refuse frames the daemon
/// considers legal, which only moves which end reports the failure.
///
/// **This is a second copy of the daemon's figure, and deliberately so.**
/// `trusty-common` is below `trusty-memory` in the dependency graph, so it
/// cannot import the constant. `memory_rpc_frame_budget_matches_the_daemon` in
/// `trusty-memory/tests/uds_consumer_contract.rs` is what keeps them equal.
pub const MAX_FRAME_BYTES: u64 = 32 * 1024 * 1024;

/// Default budget for one call.
///
/// Why 5 seconds: it is the timeout the retired `reqwest` client carried, kept
/// so this migration changes the transport and not what a slow daemon looks
/// like to a caller. A caller with different needs passes its own through
/// [`call_memory_tool_at_with_timeout`] — the monitor TUI polls on a 3-second
/// tick, and a bulk import wants far longer than either.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// A path nothing can be serving, for a caller that must never see an error.
///
/// Why a path under a directory that cannot exist rather than an empty one: a
/// dial against it is refused by the kernel immediately, which is what makes
/// the fail-open callers below fail fast instead of waiting out a budget.
const UNREACHABLE_PLACEHOLDER: &str = "/nonexistent/trusty-memory/trusty-memory.sock";

/// Resolve the socket the trusty-memory daemon binds.
///
/// # Errors
///
/// When the data directory cannot be resolved or created — an operator-fixable
/// condition (permissions, a `TRUSTY_DATA_DIR_OVERRIDE` pointing somewhere
/// unusable), distinct from "the daemon is not running", which this function
/// cannot and does not report.
///
/// Test: `resolve_memory_socket_honours_the_env_override`.
pub fn resolve_memory_socket() -> Result<PathBuf> {
    if let Ok(raw) = std::env::var(TRUSTY_MEMORY_SOCKET_ENV) {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return Ok(PathBuf::from(trimmed));
        }
    }
    crate::daemon_addr::daemon_socket_path(MEMORY_APP_NAME)
}

/// Fail-open variant of [`resolve_memory_socket`].
///
/// Why: catch-up, identity seeding, and the TUI health poller all degrade
/// gracefully when trusty-memory is unreachable rather than aborting their
/// caller. Keeping the "give me *a* path, even a dead one" policy here means it
/// is one decision rather than an `unwrap_or_else` at every call site.
///
/// What: delegates to [`resolve_memory_socket`]; on `Err`, warns on stderr and
/// returns [`UNREACHABLE_PLACEHOLDER`].
///
/// Test: `resolve_memory_socket_or_unreachable_falls_back`.
pub fn resolve_memory_socket_or_unreachable() -> PathBuf {
    resolve_memory_socket().unwrap_or_else(|e| {
        eprintln!("trusty-memory: {e}");
        PathBuf::from(UNREACHABLE_PLACEHOLDER)
    })
}

/// Call one method on the running daemon and return its `result`.
///
/// # Errors
///
/// When the socket cannot be resolved or dialled — which is what "the daemon is
/// not running" looks like — or when the daemon answers with a JSON-RPC error,
/// whose message and code are carried through so the caller reports the reason
/// it was given rather than a generic failure.
pub async fn call_memory_tool(method: &str, params: Value) -> Result<Value> {
    let socket = resolve_memory_socket()?;
    call_memory_tool_at(&socket, method, params).await
}

/// [`call_memory_tool`] against an already-resolved socket.
///
/// Why: catch-up resolves once into `CatchupOptions::memory_socket` and threads
/// it through, and a test drives a daemon on a temp path. Re-resolving per call
/// would make both impossible without mutating process-global state.
///
/// # Errors
///
/// As [`call_memory_tool`].
///
/// Test: `call_memory_tool_at_reports_a_dead_socket_rather_than_hanging`.
pub async fn call_memory_tool_at(socket: &Path, method: &str, params: Value) -> Result<Value> {
    call_memory_tool_at_with_timeout(socket, method, params, DEFAULT_TIMEOUT).await
}

/// [`call_memory_tool_at`] with an explicit budget.
///
/// # Errors
///
/// As [`call_memory_tool`].
pub async fn call_memory_tool_at_with_timeout(
    socket: &Path,
    method: &str,
    params: Value,
    timeout: Duration,
) -> Result<Value> {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    });

    let response: RpcResponse =
        send_framed_request_capped(socket, &request, timeout, MAX_FRAME_BYTES)
            .await
            .with_context(|| {
                format!(
                    "call {method} on the trusty-memory daemon at {}",
                    socket.display()
                )
            })?;

    match (response.result, response.error) {
        (Some(result), _) => Ok(result),
        (None, Some(e)) => Err(anyhow!("{method} failed: {} ({})", e.message, e.code)),
        // The daemon's own contract is that exactly one of the two is present.
        (None, None) => Err(anyhow!(
            "{method} answered with neither a result nor an error"
        )),
    }
}

/// Is anything serving the daemon's socket?
///
/// Why a bare connect rather than a `memory.health` call: the question is
/// whether the endpoint is live. A daemon that is up but degraded must not be
/// reported absent and then spawned on top of itself.
pub async fn memory_daemon_is_serving(socket: &Path, timeout: Duration) -> bool {
    crate::uds::socket_is_serving(socket, timeout).await
}

#[cfg(test)]
mod tests {
    use super::*;
    // Reuse the crate-wide env-mutation lock (`data_dir::ENV_LOCK`) rather than
    // a module-local one: several test modules mutate `TRUSTY_DATA_DIR_OVERRIDE`,
    // and cargo runs tests in the same process across files, so a separate
    // lock would not prevent the race.
    use crate::data_dir::ENV_LOCK;

    /// Why: the override is the only way a test rig or a CI job can point this
    /// client at a daemon it started itself without redirecting every other
    /// trusty-* client in the process through `TRUSTY_DATA_DIR_OVERRIDE`.
    /// Test: itself.
    #[test]
    fn resolve_memory_socket_honours_the_env_override() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::set_var(TRUSTY_MEMORY_SOCKET_ENV, "/tmp/example-memory.sock");
        }
        let resolved = resolve_memory_socket();
        unsafe {
            std::env::remove_var(TRUSTY_MEMORY_SOCKET_ENV);
        }
        assert_eq!(
            resolved.expect("an override always resolves"),
            PathBuf::from("/tmp/example-memory.sock")
        );
    }

    /// Why: `resolve_memory_socket` errs only when the data directory cannot be
    /// resolved or created, and the fail-open callers must get a dead path
    /// rather than an error they would have to handle.
    /// What: points `TRUSTY_DATA_DIR_OVERRIDE` at a path under a file, which no
    /// directory can be created beneath.
    /// Test: itself.
    #[test]
    fn resolve_memory_socket_or_unreachable_falls_back() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::NamedTempFile::new().expect("temp file");
        unsafe {
            std::env::remove_var(TRUSTY_MEMORY_SOCKET_ENV);
            std::env::set_var(
                crate::data_dir::DATA_DIR_OVERRIDE_ENV,
                tmp.path().join("under-a-file"),
            );
        }
        let resolved = resolve_memory_socket_or_unreachable();
        unsafe {
            std::env::remove_var(crate::data_dir::DATA_DIR_OVERRIDE_ENV);
        }
        assert_eq!(resolved, PathBuf::from(UNREACHABLE_PLACEHOLDER));
    }

    /// Why: every fail-open caller degrades on "the daemon is not running", and
    /// has to learn that promptly rather than by waiting out a timeout. A dial
    /// against an absent socket is refused by the kernel, so the error must
    /// arrive well inside the budget.
    /// Test: itself.
    #[tokio::test]
    async fn call_memory_tool_at_reports_a_dead_socket_rather_than_hanging() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let started = std::time::Instant::now();
        let result = call_memory_tool_at(
            &tmp.path().join("absent.sock"),
            "memory_list",
            json!({ "palace": "p" }),
        )
        .await;
        assert!(result.is_err(), "an absent socket cannot answer");
        assert!(
            started.elapsed() < DEFAULT_TIMEOUT,
            "a refused dial must not wait out the budget: {:?}",
            started.elapsed()
        );
    }
}
