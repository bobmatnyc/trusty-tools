//! The one client trusty-search's own CLI uses to reach its daemon (#6285).
//!
//! Why: ADR-0032 retires the daemon's loopback HTTP listener. Every route a
//! consumer needs already has a JSON-RPC twin on the daemon's Unix socket
//! (`service::socket::METHODS`), so the CLI moves onto those names before the
//! retire slice deletes the axum surface. One client, rather than a socket
//! dial per subcommand, is what keeps the socket path, the frame budget and the
//! refusal vocabulary identical across ~25 subcommands.
//!
//! What: [`DaemonClient`] resolves the socket, sends one frame per call through
//! `trusty_common::uds`, and reports every failure as a [`DaemonCallError`] —
//! `Unreachable` (nothing is serving the socket), `Transport` (the exchange
//! broke), or `Refused` (the daemon answered with its own JSON-RPC code).
//!
//! **It never falls back to TCP.** A missing or dead socket is reported as
//! `Unreachable`, naming the socket path, and the call fails. A silent fallback
//! onto `http://127.0.0.1:7878` would keep a half-migrated caller working until
//! the retire slice deleted the listener under it, and would reach a DIFFERENT
//! daemon whenever an isolated `TRUSTY_DATA_DIR` instance is running.
//!
//! Test: `daemon_client_tests.rs`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use trusty_common::uds::server::{
    RpcResponse, CODE_INTERNAL_ERROR, CODE_INVALID_PARAMS, CODE_METHOD_NOT_FOUND,
};
use trusty_common::uds::{
    send_framed_request_capped, send_framed_stream_request_capped, FramedStream, UdsRpcError,
};

use crate::service::rpc::error::{
    CODE_CONFLICT, CODE_DEADLINE_EXCEEDED, CODE_FORBIDDEN, CODE_NOT_FOUND, CODE_TOO_MANY_REQUESTS,
    CODE_UNAVAILABLE, CODE_UNAVAILABLE_PERMANENT,
};
use crate::service::socket::{self, MAX_FRAME_BYTES};

// #9168: `pub(crate)` so the MCP bridge's tests share this mock daemon.
#[cfg(test)]
#[path = "daemon_client_tests.rs"]
pub(crate) mod tests;

#[cfg(test)]
#[path = "daemon_client_sweep_tests.rs"]
mod sweep_tests;

/// Environment variable that names the daemon's socket explicitly.
///
/// The same variable `trusty_common::search_rpc` honours, so a test rig or an
/// operator pointing every trusty-* client at one socket sets it once.
pub const TRUSTY_SEARCH_SOCKET_ENV: &str = trusty_common::search_rpc::TRUSTY_SEARCH_SOCKET_ENV;

/// Budget for one ordinary call, dial to decoded response.
///
/// Why 60 s: the daemon bounds a query at 30 s (`query_timeout`) after it has
/// been admitted, and admission can queue. A budget below that would turn a
/// query the daemon answers into a client-side timeout.
pub const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(60);

/// Budget for a liveness probe.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Per-frame budget for a stream.
///
/// Why a day: the reindex stream is idle for as long as the embedder stalls
/// between batches, and the CLI already runs its own stall detector over it.
/// A tighter per-frame budget here would end a healthy slow reindex.
pub const STREAM_FRAME_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

/// A handle on the daemon's socket.
///
/// Why a value rather than free functions: a test hands a subcommand a client
/// pointed at a scratch socket, so no test ever resolves — or dials — the live
/// daemon's socket.
/// Test: `a_missing_socket_fails_closed_naming_the_path`.
#[derive(Debug, Clone)]
pub struct DaemonClient {
    socket: PathBuf,
    timeout: Duration,
}

/// Why a call to the daemon failed.
///
/// Why three kinds: each asks the operator for something different. Nothing
/// serving the socket means "start the daemon"; a broken exchange means "look
/// at the daemon"; a refusal carries the daemon's own reason, which is the
/// message to show.
#[derive(Debug, thiserror::Error)]
pub enum DaemonCallError {
    /// Nothing is serving the socket — the daemon is not running, or runs as a
    /// different instance.
    #[error(
        "the trusty-search daemon is not reachable at socket {} ({source}); \
         start it with `trusty-search start`",
        socket.display()
    )]
    Unreachable {
        /// The socket that could not be dialled.
        socket: PathBuf,
        /// The dial failure, verbatim.
        #[source]
        source: UdsRpcError,
    },
    /// The socket answered, but the exchange did not complete.
    #[error("{method} over socket {}: {source}", socket.display())]
    Transport {
        /// The method that was called.
        method: String,
        /// The socket the call was sent to.
        socket: PathBuf,
        /// The transport failure, verbatim.
        #[source]
        source: UdsRpcError,
    },
    /// The daemon answered with a JSON-RPC error.
    #[error("{method} refused ({}): {message}", code_label(*code))]
    Refused {
        /// The method that was called.
        method: String,
        /// The daemon's own code — see `service::rpc::error`.
        code: i64,
        /// The daemon's own message.
        message: String,
        /// The JSON-RPC error's `data` member, when the daemon sent one —
        /// an index-unavailable refusal's whole body (#6285).
        data: Option<Value>,
    },
    /// The daemon answered a frame carrying neither a result nor an error.
    #[error("{method} over socket {} answered with neither a result nor an error", socket.display())]
    Malformed {
        /// The method that was called.
        method: String,
        /// The socket the call was sent to.
        socket: PathBuf,
    },
}

impl DaemonCallError {
    /// The daemon's JSON-RPC code, when it answered with one.
    pub fn code(&self) -> Option<i64> {
        match self {
            Self::Refused { code, .. } => Some(*code),
            _ => None,
        }
    }

    /// The daemon's own message, when it answered with one.
    pub fn message(&self) -> Option<&str> {
        match self {
            Self::Refused { message, .. } => Some(message),
            _ => None,
        }
    }

    /// The daemon's structured refusal detail, when it sent one (#6285).
    ///
    /// Why: the MCP bridge's `INDEX_UNAVAILABLE` contract relays the daemon's
    /// 503 body — `index_id`, `retryable`, `restore_via`, `reason`,
    /// `transient`, `stages` — and over the socket that body arrives here.
    /// What: the refusal's `data` member verbatim; `None` for every other
    /// failure kind and for a refusal that carried none.
    /// Test: `a_refusal_carries_the_daemons_error_data`.
    pub fn data(&self) -> Option<&Value> {
        match self {
            Self::Refused { data, .. } => data.as_ref(),
            _ => None,
        }
    }

    /// Nothing is serving the socket.
    pub fn is_unreachable(&self) -> bool {
        matches!(self, Self::Unreachable { .. })
    }

    /// The daemon has no such index (HTTP 404's twin).
    pub fn is_not_found(&self) -> bool {
        self.code() == Some(CODE_NOT_FOUND)
    }

    /// The write collides with an existing registration (HTTP 409's twin).
    pub fn is_conflict(&self) -> bool {
        self.code() == Some(CODE_CONFLICT)
    }

    /// The caller's params were refused (HTTP 400's twin).
    pub fn is_invalid_params(&self) -> bool {
        self.code() == Some(CODE_INVALID_PARAMS)
    }

    /// The daemon cannot serve this now or ever (HTTP 503's twin, either class).
    pub fn is_unavailable(&self) -> bool {
        matches!(
            self.code(),
            Some(CODE_UNAVAILABLE) | Some(CODE_UNAVAILABLE_PERMANENT)
        )
    }

    /// The 503 twin that retrying will never clear.
    pub fn is_permanently_unavailable(&self) -> bool {
        self.code() == Some(CODE_UNAVAILABLE_PERMANENT)
    }
}

/// The HTTP refusal class a socket code stands for, for operator-facing text.
///
/// Why: `service::rpc::error::code_for` projects each HTTP status onto one
/// code. Printing the class back beside the daemon's message keeps an
/// operator's error line as specific as the `HTTP 404` it used to carry.
/// What: the inverse of `code_for`, plus the three transport-level codes.
/// Test: `every_daemon_code_has_a_label`.
pub fn code_label(code: i64) -> &'static str {
    match code {
        CODE_INVALID_PARAMS => "invalid params",
        CODE_FORBIDDEN => "forbidden",
        CODE_NOT_FOUND => "not found",
        CODE_DEADLINE_EXCEEDED => "deadline exceeded",
        CODE_CONFLICT => "conflict",
        CODE_TOO_MANY_REQUESTS => "too many requests",
        CODE_UNAVAILABLE => "unavailable",
        CODE_UNAVAILABLE_PERMANENT => "permanently unavailable",
        CODE_METHOD_NOT_FOUND => "method not found",
        CODE_INTERNAL_ERROR => "internal error",
        _ => "error",
    }
}

impl DaemonClient {
    /// A client for the socket this process's daemon binds.
    ///
    /// Why: the daemon derives its socket from `TRUSTY_DATA_DIR`
    /// ([`socket::socket_path`]); a client that resolved it any other way would
    /// reach a different instance. `TRUSTY_SEARCH_SOCKET` wins when set, the
    /// same override `trusty_common::search_rpc` honours.
    /// What: the env override when non-empty, else [`socket::socket_path`].
    ///
    /// # Errors
    ///
    /// When the data directory cannot be resolved or created.
    ///
    /// Test: `resolve_honours_the_socket_env_override`.
    pub fn resolve() -> anyhow::Result<Self> {
        Ok(Self::at(resolve_socket(
            std::env::var_os(TRUSTY_SEARCH_SOCKET_ENV).as_deref(),
        )?))
    }

    /// A client for an explicit socket path.
    pub fn at(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: socket.into(),
            timeout: DEFAULT_CALL_TIMEOUT,
        }
    }

    /// The same client with a different per-call budget.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The socket this client dials.
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// Call `method` with `params` and return its `result`.
    ///
    /// Why the 64 MiB frame budget: the daemon's listener accepts
    /// [`MAX_FRAME_BYTES`], and a `full_content` query can answer more than the
    /// shared 8 MiB default. A smaller client budget would refuse a response the
    /// daemon served.
    ///
    /// # Errors
    ///
    /// [`DaemonCallError::Unreachable`] when the socket cannot be dialled,
    /// [`DaemonCallError::Refused`] when the daemon answers an error, and
    /// [`DaemonCallError::Transport`] for every other failure.
    ///
    /// Test: `a_refusal_carries_the_daemons_code_and_message`,
    /// `a_missing_socket_fails_closed_naming_the_path`.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, DaemonCallError> {
        self.call_with_timeout(method, params, self.timeout).await
    }

    /// [`Self::call`] with an explicit budget for this one call.
    ///
    /// # Errors
    ///
    /// The same set as [`Self::call`].
    pub async fn call_with_timeout(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, DaemonCallError> {
        let request = request_frame(method, params, false);
        let response: RpcResponse =
            send_framed_request_capped(&self.socket, &request, timeout, MAX_FRAME_BYTES)
                .await
                .map_err(|e| self.transport_error(method, e))?;
        match (response.result, response.error) {
            (Some(result), _) => Ok(result),
            (None, Some(e)) => Err(DaemonCallError::Refused {
                method: method.to_string(),
                code: e.code,
                message: e.message,
                data: e.data,
            }),
            (None, None) => Err(DaemonCallError::Malformed {
                method: method.to_string(),
                socket: self.socket.clone(),
            }),
        }
    }

    /// Open a streaming method and return its frames.
    ///
    /// Why: `search.index.reindex.stream` replaces the SSE body the reindex
    /// driver read; one item is the JSON document one SSE `data:` line carried.
    ///
    /// # Errors
    ///
    /// [`DaemonCallError::Unreachable`] or [`DaemonCallError::Transport`] while
    /// opening. A refusal arrives through the stream as its one error item.
    ///
    /// Test: `a_stream_yields_items_then_ends`.
    pub async fn stream(
        &self,
        method: &str,
        params: Value,
    ) -> Result<FramedStream<Value>, DaemonCallError> {
        let request = request_frame(method, params, true);
        send_framed_stream_request_capped(
            &self.socket,
            &request,
            STREAM_FRAME_TIMEOUT,
            MAX_FRAME_BYTES,
        )
        .await
        .map_err(|e| self.transport_error(method, e))
    }

    /// `search.health`, with the short probe budget.
    ///
    /// # Errors
    ///
    /// The same set as [`Self::call`].
    pub async fn health(&self) -> Result<Value, DaemonCallError> {
        self.call_with_timeout(
            socket::METHOD_HEALTH,
            Value::Object(Default::default()),
            PROBE_TIMEOUT,
        )
        .await
    }

    /// Whether the daemon answers `search.health` right now.
    pub async fn is_up(&self) -> bool {
        self.health().await.is_ok()
    }

    /// Sort a transport failure into unreachable versus broken.
    fn transport_error(&self, method: &str, source: UdsRpcError) -> DaemonCallError {
        if source.is_dial_failure() {
            DaemonCallError::Unreachable {
                socket: self.socket.clone(),
                source,
            }
        } else {
            DaemonCallError::Transport {
                method: method.to_string(),
                socket: self.socket.clone(),
                source,
            }
        }
    }
}

/// Resolve the socket from the env override, else the daemon's own derivation.
///
/// Why a parameter: a test compares the two arms without mutating the
/// process-global environment.
///
/// # Errors
///
/// When the data directory cannot be resolved or created.
///
/// Test: `resolve_honours_the_socket_env_override`.
pub fn resolve_socket(env_override: Option<&std::ffi::OsStr>) -> anyhow::Result<PathBuf> {
    if let Some(raw) = env_override {
        let trimmed = raw.to_string_lossy();
        let trimmed = trimmed.trim();
        if !trimmed.is_empty() {
            return Ok(PathBuf::from(trimmed));
        }
    }
    socket::socket_path()
}

/// The JSON-RPC envelope for one call.
fn request_frame(method: &str, params: Value, stream: bool) -> Value {
    let mut frame = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    });
    if stream {
        frame["stream"] = Value::Bool(true);
    }
    frame
}
