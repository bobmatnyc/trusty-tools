//! `/api/analyze/{*path}` — the HTTP face of trusty-analyze's socket (#6155).
//!
//! Why: the console-served dashboard at `/tools/analyze/` is plain browser
//! JavaScript. It speaks HTTP and nothing else, while trusty-analyze has spoken
//! framed JSON-RPC over a Unix socket since #6287. This module is the whole of
//! the translation, so the SPA needs no fork and trusty-analyze needs no HTTP.
//!
//! What: one handler. It maps the request through [`super::map::map_request`],
//! dials the socket, and answers the daemon's JSON body.
//!
//! What is NOT here: an SSE arm. `crate::uds_sse` exists for exactly that and
//! both other bridges use it, but trusty-analyze registers no streaming method
//! (`map`'s module docs record why), so there is nothing to hand it.
//!
//! Test: `tests/analyze_uds_bridge.rs` drives the whole router against a stub
//! daemon socket.

use std::path::PathBuf;

use axum::body::Bytes;
use axum::extract::{Path as AxumPath, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use tracing::{debug, warn};

use super::map::{Call, map_request};
use super::{ANALYZE_SERVICE, AnalyzeRpcError, CALL_TIMEOUT, MAX_FRAME_BYTES, call, json_response};
use crate::server::AppState;

/// `ANY /api/analyze/{*path}` — reach trusty-analyze over its socket.
///
/// Why a dedicated handler rather than a row in `proxy::routes::full_id`: that
/// proxy forwards bytes to a base URL resolved from an `http_addr` discovery
/// file, and trusty-analyze stopped writing one in #6287 — which is exactly why
/// its proxy row was deleted then. A stale file left by a pre-migration daemon
/// names a port that whatever now holds 7879 will answer, so restoring the proxy
/// row is not an option; this is.
///
/// What: read the body, map the request, resolve the socket, then one unary
/// exchange rendered as JSON. An unmapped path is `501` naming itself; an
/// unresolvable or unreachable socket is `502`; a refusal is the status the same
/// refusal carried over HTTP.
///
/// Test: `an_unmapped_path_is_not_implemented_and_names_itself` and
/// `a_dead_socket_is_a_bad_gateway_not_an_empty_success`, plus every other case
/// in `tests/analyze_uds_bridge.rs`.
pub async fn analyze_api_handler(
    State(state): State<AppState>,
    AxumPath(path): AxumPath<String>,
    req: Request,
) -> Response {
    let (parts, body) = req.into_parts();
    let query = parts.uri.query().map(str::to_owned);

    // #6155: the body becomes one request FRAME, so the frame budget is the body
    // limit — one constant, not a second literal that can drift from it.
    let body_limit = usize::try_from(MAX_FRAME_BYTES).unwrap_or(usize::MAX);
    let body_bytes: Bytes = match axum::body::to_bytes(body, body_limit).await {
        Ok(b) => b,
        Err(e) => {
            warn!("analyze_uds: could not read the request body: {e}");
            return (
                StatusCode::PAYLOAD_TOO_LARGE,
                format!("request body exceeds the {MAX_FRAME_BYTES}-byte frame budget"),
            )
                .into_response();
        }
    };

    let mapped = match map_request(&parts.method, &path, query.as_deref(), &body_bytes) {
        Ok(call) => call,
        Err(reason) => {
            warn!(
                "analyze_uds: {} /{path} is not mapped: {reason}",
                parts.method
            );
            return (
                StatusCode::NOT_IMPLEMENTED,
                axum::Json(json!({ "error": reason, "service": ANALYZE_SERVICE })),
            )
                .into_response();
        }
    };

    let socket = match state.analyze_socket_path() {
        Ok(p) => p,
        Err(reason) => return AnalyzeRpcError::Unresolved(reason).into_response(),
    };

    let Call::Unary { method, params } = mapped;
    debug!("analyze_uds: {} /{path} → {method}", parts.method);
    match call(&socket, method, params, CALL_TIMEOUT).await {
        Ok(result) => json_response(&result),
        Err(e) => e.into_response(),
    }
}

/// `ANY /proxy/analyze/{*path}` — the deprecated alias, kept working (#1849).
///
/// Why kept: this SPA's own `base.js` documented `/proxy/analyze/` as a
/// supported mount for as long as the console reverse-proxied this daemon, so
/// callers exist that predate the `/api/` rename. They would otherwise get
/// `400 unknown daemon`, since #6287 removed the analyze row from
/// `proxy::routes::full_id`.
/// Test: `deprecated_alias_reaches_the_same_handler` in
/// `tests/analyze_uds_bridge.rs`.
pub async fn deprecated_analyze_api_handler(
    state: State<AppState>,
    path: AxumPath<String>,
    req: Request,
) -> Response {
    tracing::trace!(
        "analyze_uds: DEPRECATED /proxy/analyze/… — use /api/analyze/… instead (#1849)"
    );
    analyze_api_handler(state, path, req).await
}

/// Resolve trusty-analyze's socket for this bridge.
///
/// Why on `AppState` rather than a free call: the integration tests bind their
/// own stub socket and need the router to dial it, and the alternative —
/// `TRUSTY_DATA_DIR_OVERRIDE` — is process-global in a test binary that runs six
/// connectors in parallel. The same argument `search_uds` and `memory_uds`
/// record for their own overrides.
/// Test: `tests/analyze_uds_bridge.rs` drives every case through it.
impl AppState {
    /// The socket this console dials for trusty-analyze's `analyze.*` surface.
    ///
    /// # Errors
    ///
    /// When the data directory cannot be resolved or created.
    pub(crate) fn analyze_socket_path(&self) -> Result<PathBuf, String> {
        match &self.analyze_socket {
            Some(p) => Ok(p.as_ref().clone()),
            None => super::socket_path(),
        }
    }

    /// Dial `socket` for trusty-analyze instead of the resolved path.
    ///
    /// Why: see the `AppState::analyze_socket` field doc — the alternative is a
    /// process-global env var this crate's test binary cannot use safely.
    /// Test: `tests/analyze_uds_bridge.rs` sets it on every case.
    #[must_use]
    pub fn with_analyze_socket(mut self, socket: PathBuf) -> Self {
        self.analyze_socket = Some(std::sync::Arc::new(socket));
        self
    }
}
