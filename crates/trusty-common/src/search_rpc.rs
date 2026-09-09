//! The one way anything outside trusty-search calls the running daemon (#6285,
//! #7237).
//!
//! Why: this client lived in `trusty_mpm::daemon::search_rpc`, where only
//! `tm doctor` and the session manager could reach it. `search_index`'s
//! find-or-create sits BELOW trusty-mpm in the dependency graph, so it could
//! not use it and went on resolving `~/.trusty-search/http_addr` and POSTing
//! `http://127.0.0.1:7878/indexes` — a listener ADR-0032 retired, which is why
//! every fresh project logged `error sending request for url` and then
//! `NotConfirmed` at session launch (#7237). Moving the client down to
//! trusty-common is what makes one implementation serve both: trusty-mpm's
//! module is now a re-export of this one.
//!
//! What: [`search_socket`] derives the path both ends compute, [`call_at`]
//! writes one frame and reads one back, and [`call_blocking`] is that call from
//! a synchronous caller — it runs on a dedicated OS thread with its own
//! current-thread runtime, because `search_index`'s entry points are called
//! from inside a tokio runtime as often as not. [`SearchRpcError`] carries the
//! daemon's own code, so a caller can tell "no such index" — the whole point of
//! the pinned-index probe (#5045) — and "that root already belongs to another
//! index" (#6864) from a transport failure.
//!
//! **The method names are literals here, and deliberately so.** trusty-common
//! has no Cargo edge on trusty-search (the edge runs the other way), so the
//! names cannot be imported; they are pinned by
//! `trusty_search::service::socket::METHODS`, which the daemon's own
//! `rpc_router_registers_every_documented_method` compares its router against.
//! A name that drifted answers `method_not_found`, which every caller here
//! reports as an unhealthy daemon.
//!
//! Test: `search_socket_honours_the_env_override`,
//! `call_at_reports_a_dead_socket_rather_than_hanging`,
//! `call_blocking_reports_a_dead_socket_rather_than_hanging`,
//! `call_blocking_round_trips_against_a_listening_daemon`,
//! `call_blocking_carries_the_daemons_own_error_code`,
//! `call_blocking_reports_a_panicking_handler_rather_than_hanging`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use serde_json::Value;

use crate::uds::send_framed_request;
use crate::uds::server::RpcResponse;

#[cfg(test)]
#[path = "search_rpc_tests.rs"]
mod tests;

/// Environment variable that pins the daemon's socket path explicitly.
///
/// Why: it replaces `TRUSTY_SEARCH_ADDR`, which named a listener there no
/// longer is one. The affordance it provided is still wanted — a test rig
/// points a probe at a daemon it started on a temp path — and the alternative,
/// `TRUSTY_DATA_DIR_OVERRIDE`, is process-global and would redirect every other
/// trusty-* client in the same process along with this one. Same shape as
/// [`crate::memory_rpc::TRUSTY_MEMORY_SOCKET_ENV`] and trusty-audit's
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

/// One index's stages, capabilities and footprint. Answers [`CODE_NOT_FOUND`]
/// for an id the daemon does not hold.
pub const METHOD_INDEX_STATUS: &str = "search.index.status";

/// `POST /indexes`' socket twin — register a new index, or re-join one that
/// already exists (#7237).
///
/// Params are a BARE `CreateIndexRequest`: `{id, root_path,
/// allow_sensitive_path, skip_vector}`, byte-for-byte the JSON the HTTP route
/// took. It is a registry-level write, so it does NOT take the
/// `{index_id, body}` envelope the index-scoped writes use — sending one would
/// be refused as `invalid_params`.
pub const METHOD_INDEX_CREATE: &str = "search.index.create";

/// `POST /indexes/{id}/reindex`' socket twin — queue a full reindex. The
/// trigger only; the walk itself is the daemon's.
///
/// Params are `{index_id}`, with an optional `body` the callers here never
/// send.
pub const METHOD_INDEX_REINDEX: &str = "search.index.reindex";

/// Deregister one index, and — with `delete_data: true` — destroy its on-disk
/// data directory.
///
/// The only destructive method named here. In trusty-mpm it is reachable
/// exclusively through `session_manager::index_delete_guard::DestructiveIndexDelete`,
/// which holds that crate's one copy of the `delete_data` opt-in (#4743).
pub const METHOD_INDEX_DELETE: &str = "search.index.delete";

/// The daemon's code for "the thing you asked about does not exist" (HTTP 404).
///
/// A second copy of `trusty_search::service::rpc::error::CODE_NOT_FOUND`, and
/// of `trusty_mpm::daemon::error::CODE_NOT_FOUND`, because trusty-common is
/// below both in the dependency graph and cannot import either.
/// `search_rpc_codes_match_the_daemon_error_table` in trusty-mpm is what keeps
/// this equal to the copy its own daemon serves.
pub const CODE_NOT_FOUND: i64 = -32004;

/// The daemon's code for "this write collides with a registration that already
/// exists" (HTTP 409) — see [`CODE_NOT_FOUND`] for why the number is duplicated.
///
/// The recoverable refusal #6864 is about: the requested id names another tree,
/// or this tree already belongs to another id.
pub const CODE_CONFLICT: i64 = -32009;

/// The daemon answered, and what it answered was an error.
///
/// Why a typed error rather than a formatted string: the pinned-index probe has
/// to tell "the daemon has no such index" from "the call failed" — a 404 is a
/// definite, actionable verdict and a transport failure is an absence of
/// information (#5045) — and the find-or-create has to tell a recoverable `409`
/// from an outright refusal (#6864). Carrying the code in the error keeps
/// [`call_at`]'s `Result<Value>` signature and makes only the callers that care
/// pay for it, via `anyhow::Error::downcast_ref`.
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
        self.code == CODE_NOT_FOUND
    }

    /// Did the daemon refuse because the registry already holds this id, or
    /// this root, under other terms (#6864)?
    pub fn is_conflict(&self) -> bool {
        self.code == CODE_CONFLICT
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
    crate::daemon_socket_path(SEARCH_APP_NAME)
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

/// [`call_at`] from a synchronous caller, on a dedicated OS thread.
///
/// Why: `search_index`'s find-or-create is a blocking function called from
/// session launch and from tcode's task start, both of which are frequently
/// inside a tokio runtime — and `Runtime::block_on` inside a runtime worker
/// panics. Spawning the runtime on a freshly-created `std::thread` keeps it
/// entirely off the caller's async worker, which is the same arrangement the
/// `reqwest::blocking` client this replaced needed for the same reason.
/// What: builds a current-thread runtime on a joined thread and drives
/// [`call_at`] on it. A panicked worker is reported as an error rather than
/// re-raised, so a best-effort caller keeps its fail-closed shape.
///
/// A panic on the DAEMON's side takes the other route: the connection task dies
/// and this thread sees an unanswered call, which
/// `call_blocking_reports_a_panicking_handler_rather_than_hanging` pins. The
/// `join` arm below stays defensive — nothing this function drives panics on
/// its own thread — so it has no test of its own.
///
/// # Errors
///
/// Everything [`call_at`] reports, plus a failure to build the runtime and a
/// panicked worker thread.
///
/// Test: `call_blocking_round_trips_against_a_listening_daemon`,
/// `call_blocking_carries_the_daemons_own_error_code`,
/// `call_blocking_reports_a_dead_socket_rather_than_hanging`,
/// `call_blocking_reports_a_panicking_handler_rather_than_hanging`.
pub fn call_blocking(
    socket: &Path,
    method: &str,
    params: Value,
    timeout: Duration,
) -> Result<Value> {
    let socket = socket.to_path_buf();
    let owned_method = method.to_string();

    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .with_context(|| format!("build a runtime for {owned_method}"))?;
        runtime.block_on(call_at(&socket, &owned_method, params, timeout))
    })
    .join()
    .map_err(|_| anyhow!("the trusty-search {method} worker thread panicked"))?
}
