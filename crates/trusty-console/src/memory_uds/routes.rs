//! `/api/memory/{*path}` — the HTTP face of trusty-memory's socket (#6155).
//!
//! Why: the console-served dashboard at `/tools/memory/` is plain browser
//! JavaScript. It speaks HTTP and Server-Sent Events and nothing else, while
//! trusty-memory has spoken framed JSON-RPC over a Unix socket since #6286. This
//! module is the whole of the translation, so the SPA needs no fork and
//! trusty-memory needs no HTTP.
//!
//! What: one handler. It maps the request through [`super::map::map_request`],
//! dials the socket, and answers either a JSON body or an open
//! `text/event-stream`.
//!
//! The SSE plumbing itself is [`crate::uds_sse`]'s, shared with the trusty-search
//! bridge. What stays here is what trusty-memory owns: which JSON-RPC code
//! becomes which HTTP status, and the peek below.
//!
//! A refusal that arrives BEFORE the first item becomes an HTTP status, not a
//! `200` carrying an error event — which is why the first frame is read before
//! the response head is built. `ActivityFeed.svelte` reads a closed stream as a
//! feed that ended, so a silent empty stream would report a refused subscription
//! as an empty one.
//!
//! Test: `tests/memory_uds_bridge.rs` drives the whole router against a stub
//! daemon socket.

use std::path::{Path, PathBuf};

use axum::extract::{Path as AxumPath, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use tracing::debug;
use trusty_common::uds::UdsRpcError;

use super::map::{Call, map_request};
use super::{
    CALL_TIMEOUT, MEMORY_SERVICE, MemoryRpcError, STREAM_FIRST_FRAME_PEEK, STREAM_OPEN_TIMEOUT,
    call, json_response, open_stream,
};
use crate::server::AppState;
use crate::uds_sse::sse_response;

/// `ANY /api/memory/{*path}` — reach trusty-memory over its socket.
///
/// Why a dedicated handler rather than a row in `proxy::routes::full_id`: that
/// proxy forwards bytes to a base URL resolved from an `http_addr` discovery
/// file, and trusty-memory stopped writing one in #6286 — which is exactly why
/// its proxy row was deleted then. A stale file left by a pre-migration daemon
/// names a port that whatever now holds 7070 will answer, so restoring the proxy
/// row is not an option; this is.
///
/// What: map the request, resolve the socket, then either one unary exchange
/// rendered as JSON or one stream bridged to Server-Sent Events. An unmapped
/// path is `501` naming itself; an unresolvable or unreachable socket is `502`;
/// a refusal is the status the same refusal carried over HTTP.
///
/// Test: `an_unmapped_path_is_not_implemented_and_names_itself` and
/// `a_dead_socket_is_a_bad_gateway_not_an_empty_success`, plus every other case
/// in `tests/memory_uds_bridge.rs`.
pub async fn memory_api_handler(
    State(state): State<AppState>,
    AxumPath(path): AxumPath<String>,
    req: Request,
) -> Response {
    let (parts, _body) = req.into_parts();
    let query = parts.uri.query().map(str::to_owned);

    let mapped = match map_request(&parts.method, &path, query.as_deref()) {
        Ok(call) => call,
        Err(reason) => {
            tracing::warn!(
                "memory_uds: {} /{path} is not mapped: {reason}",
                parts.method
            );
            return (
                StatusCode::NOT_IMPLEMENTED,
                axum::Json(json!({ "error": reason, "service": MEMORY_SERVICE })),
            )
                .into_response();
        }
    };

    let socket = match state.memory_socket_path() {
        Ok(p) => p,
        Err(reason) => return MemoryRpcError::Unresolved(reason).into_response(),
    };

    match mapped {
        Call::Unary { method, params } => {
            debug!("memory_uds: {} /{path} → {method}", parts.method);
            match call(&socket, method, params, CALL_TIMEOUT).await {
                Ok(result) => json_response(&result),
                Err(e) => e.into_response(),
            }
        }
        Call::Stream { method, params } => {
            debug!("memory_uds: {} /{path} → {method} (stream)", parts.method);
            stream_response(
                &socket,
                method,
                params,
                STREAM_OPEN_TIMEOUT,
                STREAM_FIRST_FRAME_PEEK,
            )
            .await
        }
    }
}

/// `ANY /proxy/memory/{*path}` — the deprecated alias, kept working (#1849).
///
/// Why kept: this SPA's own `base.js` documents `/proxy/memory/` as a supported
/// mount, so callers exist that predate the `/api/` rename. They would otherwise
/// get `400 unknown daemon`, since #6286 removed the memory row from
/// `proxy::routes::full_id`.
/// Test: `deprecated_alias_reaches_the_same_handler` in
/// `tests/memory_uds_bridge.rs`.
pub async fn deprecated_memory_api_handler(
    state: State<AppState>,
    path: AxumPath<String>,
    req: Request,
) -> Response {
    tracing::trace!("memory_uds: DEPRECATED /proxy/memory/… — use /api/memory/… instead (#1849)");
    memory_api_handler(state, path, req).await
}

/// Open one stream and answer it as Server-Sent Events.
///
/// Why the first frame is peeked at all: a streaming method can refuse, and that
/// refusal arrives as the stream's FIRST frame, after the dial has already
/// succeeded. Committing to `200 text/event-stream` before reading it would turn
/// every refusal into an empty stream.
///
/// Why the peek has its own short budget: `memory.activity_stream` sends no
/// opener, so on an idle daemon the first frame is minutes away — see
/// [`super::STREAM_FIRST_FRAME_PEEK`]. A peek that expires is NOT a failure: the
/// head goes out with no leading item and the stream stays open, which is what
/// the retired `/sse` route did.
///
/// What: `open_timeout` bounds the dial and the request write; `peek` bounds the
/// wait for a refusal. Then either the refusal's own status or [`sse_response`],
/// with the peeked item leading the body when there was one.
/// Test: `tests/memory_uds_bridge.rs`'s
/// `a_stream_refusal_before_the_first_item_is_an_http_status`,
/// `a_stream_reaches_the_browser_frame_for_frame`, and
/// `an_idle_stream_still_gets_its_response_head`.
async fn stream_response(
    socket: &Path,
    method: &'static str,
    params: Value,
    open_timeout: std::time::Duration,
    peek: std::time::Duration,
) -> Response {
    let mut stream = match open_stream(socket, method, params, open_timeout).await {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };

    let first = match tokio::time::timeout(peek, stream.next_frame()).await {
        Ok(Some(Ok(item))) => Some(item),
        Ok(Some(Err(e))) => return stream_error(method, e).into_response(),
        // A stream that ended with no items at all is still a well-formed empty
        // answer; the browser gets an immediately-closed event stream.
        Ok(None) => None,
        // Nothing yet, which for an event feed is the normal state. Write the
        // head and let `sse_tail` carry whatever comes, including a late
        // refusal, which reaches the browser as an error event.
        Err(_) => None,
    };

    sse_response(first, stream, method)
}

/// Turn a stream failure into the console's verdict about it.
///
/// Why: `UdsRpcError::Stream` is the daemon's own terminal error frame and
/// carries its code, so it maps to the HTTP status that refusal had. Every other
/// variant is a transport problem the console observed, which is a `502`.
/// Test: `stream_error_carries_the_daemon_code`.
fn stream_error(method: &str, e: UdsRpcError) -> MemoryRpcError {
    match e {
        UdsRpcError::Stream { error, .. } => MemoryRpcError::Refused {
            code: error.code,
            message: error.message,
        },
        other => MemoryRpcError::Unreachable(format!(
            "{MEMORY_SERVICE} did not stream {method}: {other}"
        )),
    }
}

/// Resolve trusty-memory's socket for this bridge.
///
/// Why on `AppState` rather than a free call: the integration tests bind their
/// own stub socket and need the router to dial it, and the alternative —
/// `TRUSTY_DATA_DIR_OVERRIDE` — is process-global in a test binary that runs six
/// connectors in parallel. The same argument `search_uds` records for
/// `search_socket`.
/// Test: `tests/memory_uds_bridge.rs` drives every case through it.
impl AppState {
    /// The socket this console dials for trusty-memory's `memory.*` surface.
    ///
    /// # Errors
    ///
    /// When the data directory cannot be resolved or created.
    pub(crate) fn memory_socket_path(&self) -> Result<PathBuf, String> {
        match &self.memory_socket {
            Some(p) => Ok(p.as_ref().clone()),
            None => super::socket_path(),
        }
    }

    /// Dial `socket` for trusty-memory instead of the resolved path.
    ///
    /// Why: see the `AppState::memory_socket` field doc — the alternative is a
    /// process-global env var this crate's test binary cannot use safely.
    /// Test: `tests/memory_uds_bridge.rs` sets it on every case.
    #[must_use]
    pub fn with_memory_socket(mut self, socket: PathBuf) -> Self {
        self.memory_socket = Some(std::sync::Arc::new(socket));
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trusty_common::uds::server::RpcError;

    /// Why: the daemon's terminal error frame is a refusal with a code, and it
    /// must reach the browser as the status that code stands for — not as a
    /// generic gateway failure that hides which palace was missing.
    /// Test: this is the test.
    #[test]
    fn stream_error_carries_the_daemon_code() {
        let err = stream_error(
            "memory.activity_stream",
            UdsRpcError::Stream {
                path: PathBuf::from("/tmp/memory.sock"),
                error: RpcError::new(-32004, "no such palace".to_string()),
            },
        );
        assert_eq!(err.status(), StatusCode::NOT_FOUND);
        assert!(err.message().contains("no such palace"), "{err:?}");
    }

    /// Why: a transport failure is the console's own observation, never the
    /// daemon's refusal, so it must not borrow a daemon status code.
    /// Test: this is the test.
    #[test]
    fn a_transport_failure_mid_stream_is_a_bad_gateway() {
        let err = stream_error(
            "memory.activity_stream",
            UdsRpcError::NoResponse {
                path: PathBuf::from("/tmp/memory.sock"),
            },
        );
        assert_eq!(err.status(), StatusCode::BAD_GATEWAY);
    }
}
