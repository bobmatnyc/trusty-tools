//! The axum side of the webview bridge — one handler, two auth routes (#6637).
//!
//! Why: the webview speaks HTTP and Server-Sent Events and nothing else, while
//! the `tcode` daemon now speaks framed JSON-RPC over a Unix socket. This module
//! is the whole of the translation, so the webview needs no fork and the daemon
//! needs no TCP listener.
//!
//! What: [`build_router`] assembles the guard stack the daemon's own listener
//! carried — [`trusty_common::server::with_guarded_middleware_same_origin_cors`]
//! outside, `require_bearer` inside — around a wildcard handler that maps the
//! request through [`crate::bridge::map::map_request`], dials the socket, and
//! answers either a JSON body or an open `text/event-stream`.
//!
//! ## Why the credential survived the move
//!
//! The daemon's bearer token existed to authenticate a TCP peer that could be
//! anyone; a UDS peer is authenticated by `SO_PEERCRED` instead, which is why
//! #6637 retires the daemon's copy. This listener is TCP again — it has to be,
//! a webview cannot dial a socket — so the same reasoning that put a credential
//! on the daemon's port puts one on this one. What changed is where the token
//! comes from: [`crate::bridge::serve`] mints a fresh one per launch and hands
//! it to the webview over Tauri IPC, so there is no `0600` file for anything on
//! this machine to read.
//!
//! ## The SSE bridge, frame for frame
//!
//! [`trusty_common::uds::sse`] owns the `data:` encoding, the 20-second
//! keep-alive comment, and the terminal error event that keeps a dropped tail
//! from reading as a finished one. What stays here is what this daemon owns:
//! which paths stream, and when the response head is written — see
//! [`stream_response`], which deliberately writes it before reading anything.
//!
//! Test: `tests` below, plus `tests/bridge_uds.rs`, which drives the whole
//! router against a stub daemon socket.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path as AxumPath, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{any, post};
use serde_json::{Value, json};
use tracing::{debug, warn};
use trusty_common::server::{DaemonAuth, SelfOrigins, require_bearer};
use trusty_common::uds::sse::sse_response;

use super::map::{Call, map_request};
use super::{
    BridgeError, CALL_TIMEOUT, SOCKET_APP_NAME, STREAM_OPEN_TIMEOUT, call, json_response,
    open_stream, transcript_md,
};

/// Route that exchanges the bridge credential for a single-use SSE ticket.
///
/// The same path the daemon served and the same one `daemon-auth.ts` posts to,
/// so the webview's ticket flow is unchanged by the move.
pub const SSE_TICKET_PATH: &str = "/auth/sse-ticket";

/// Largest request body this listener accepts, in bytes.
///
/// Why this figure: the body becomes one request FRAME on the socket, and the
/// daemon's listener refuses a frame over `trusty_common::uds::MAX_FRAME_BYTES`.
/// Matching it here means an oversized body is refused before 8 MiB of it is
/// copied, rather than after the daemon has already rejected the frame.
const MAX_BODY_BYTES: usize = trusty_common::uds::MAX_FRAME_BYTES as usize;

/// What the handler needs: which socket to dial and which credential to check.
///
/// Why the socket is carried rather than resolved per request: the integration
/// tests bind their own stub socket and need the router to dial it, and the
/// alternative — `TRUSTY_DATA_DIR_OVERRIDE` — is process-global in a test binary
/// whose cases run as threads.
#[derive(Clone)]
pub struct BridgeState {
    /// The daemon socket this bridge dials, or `None` to resolve it.
    socket: Option<Arc<PathBuf>>,
    /// The per-launch credential and its ticket table.
    auth: DaemonAuth,
}

impl BridgeState {
    /// Build state around `auth`, resolving the daemon socket at request time.
    #[must_use]
    pub fn new(auth: DaemonAuth) -> Self {
        Self { socket: None, auth }
    }

    /// Dial `socket` instead of the resolved path.
    ///
    /// Why: see the `socket` field doc — the alternative is a process-global env
    /// var this crate's test binary cannot use safely.
    /// Test: `tests/bridge_uds.rs` sets it on every case.
    #[must_use]
    pub fn with_socket(mut self, socket: PathBuf) -> Self {
        self.socket = Some(Arc::new(socket));
        self
    }

    /// The socket this bridge dials, or why it could not be resolved.
    fn socket_path(&self) -> Result<PathBuf, String> {
        match &self.socket {
            Some(p) => Ok(p.as_ref().clone()),
            None => super::socket_path(),
        }
    }
}

/// Assemble the bridge router, guards included.
///
/// Why the layer order is copied rather than reinvented: the daemon applied
/// `require_bearer` with `.layer()` on the FULLY-MERGED router (never
/// `route_layer`, which covers only routes registered before it in the same
/// chain) and put the origin/CORS stack OUTSIDE it, so a CORS preflight is
/// answered without a credential and a foreign-origin write is `403`ed rather
/// than `401`ed. Reordering either half changes which failure a browser sees.
/// What: `POST /auth/sse-ticket` answered here, everything else through
/// [`bridge_handler`], then the two guard layers.
/// Test: `tests/bridge_uds.rs`'s `a_request_without_the_token_is_unauthorized`
/// and `a_cross_origin_write_is_rejected`.
pub fn build_router(state: BridgeState) -> axum::Router {
    let auth = state.auth.clone();
    let app = axum::Router::new()
        .route(SSE_TICKET_PATH, post(sse_ticket_handler))
        .route("/{*path}", any(bridge_handler))
        .with_state(state);
    let authed = app.layer(axum::middleware::from_fn_with_state(auth, require_bearer));
    // The bridge binds loopback only (`crate::bridge::serve`), so the guard
    // needs no bind-derived allowlist — `SelfOrigins::default()` trusts loopback
    // and the three Tauri webview origins, and nothing else.
    trusty_common::server::with_guarded_middleware_same_origin_cors(authed, SelfOrigins::default())
}

/// `ANY /{*path}` — reach the `tcode` daemon over its socket.
///
/// What: map the request, resolve the socket, then either one unary exchange
/// rendered as JSON, one stream bridged to Server-Sent Events, or the Markdown
/// transcript. An unmapped path is `501` naming itself; an unresolvable or
/// unreachable socket is `502`; a refusal is the status the same refusal carried
/// over the daemon's REST route.
/// Test: `an_unmapped_path_is_not_implemented_and_names_itself` and
/// `a_dead_socket_is_a_bad_gateway_not_an_empty_success` in
/// `tests/bridge_uds.rs`.
async fn bridge_handler(
    State(state): State<BridgeState>,
    AxumPath(path): AxumPath<String>,
    req: Request,
) -> Response {
    let (parts, body) = req.into_parts();
    let query = parts.uri.query().map(str::to_owned);

    let body_bytes: Bytes = match axum::body::to_bytes(body, MAX_BODY_BYTES).await {
        Ok(b) => b,
        Err(e) => {
            warn!("bridge: could not read the request body: {e}");
            return (
                StatusCode::PAYLOAD_TOO_LARGE,
                format!("request body exceeds the {MAX_BODY_BYTES}-byte frame budget"),
            )
                .into_response();
        }
    };

    let mapped = match map_request(&parts.method, &path, query.as_deref(), &body_bytes) {
        Ok(call) => call,
        Err(reason) => {
            warn!("bridge: {} /{path} is not mapped: {reason}", parts.method);
            return (
                StatusCode::NOT_IMPLEMENTED,
                axum::Json(json!({ "error": reason, "service": SOCKET_APP_NAME })),
            )
                .into_response();
        }
    };

    let socket = match state.socket_path() {
        Ok(p) => p,
        Err(reason) => return BridgeError::Unresolved(reason).into_response(),
    };

    match mapped {
        Call::Unary {
            method,
            params,
            status,
        } => {
            debug!("bridge: {} /{path} → {method}", parts.method);
            match call(&socket, method, params, CALL_TIMEOUT).await {
                Ok(result) => json_response(status, &result),
                Err(e) => e.into_response(),
            }
        }
        Call::Stream { method, params } => {
            debug!("bridge: {} /{path} → {method} (stream)", parts.method);
            stream_response(&socket, method, params, STREAM_OPEN_TIMEOUT).await
        }
        Call::TranscriptMarkdown { session_id } => {
            debug!("bridge: GET /{path} → transcript markdown");
            transcript_md::respond(&socket, &session_id).await
        }
    }
}

/// `POST /auth/sse-ticket` — exchange the bridge credential for a ticket.
///
/// Why the bridge answers this itself rather than proxying it: the ticket is
/// redeemable against THIS listener's [`DaemonAuth`], and the daemon has no idea
/// this listener exists. `EventSource` cannot send a header, which is the whole
/// reason a ticket exists at all.
/// What: refuses a path that is not one of the two event streams, then mints a
/// ticket bound to that exact path. The route is not public, so an anonymous
/// `POST` is `401`ed by the guard before this function runs — a ticket is never
/// a way IN, only a way to carry an existing right onto a URL.
/// Test: `a_ticket_opens_one_stream_then_is_spent` and
/// `a_ticket_cannot_be_minted_for_a_non_stream_path` in `tests/bridge_uds.rs`.
async fn sse_ticket_handler(
    State(state): State<BridgeState>,
    axum::extract::Query(params): axum::extract::Query<SseTicketRequest>,
) -> Response {
    if !path_is_ticketable(&params.path) {
        // No echo of the requested path — this response reaches a browser.
        return (
            StatusCode::BAD_REQUEST,
            axum::Json(json!({ "error": "not a ticketable stream" })),
        )
            .into_response();
    }
    axum::Json(json!({ "ticket": state.auth.issue_ticket(&params.path) })).into_response()
}

/// The `?path=` a ticket request names.
#[derive(serde::Deserialize)]
struct SseTicketRequest {
    /// The stream path the caller means to open.
    path: String,
}

/// Is `path` one of the two event streams a ticket may open?
///
/// Why: a ticket is only as narrow as the set of paths it can be minted for. A
/// handler that minted for whatever it was handed would give back an arbitrary
/// authenticated request, which is what the binding exists to remove.
/// What: exact shape match on `/sessions/{id}/events` and
/// `/workstreams/{id}/events` — three segments, the first and last fixed, the id
/// non-empty and free of `/`. Anything else is refused.
/// Test: `a_ticket_cannot_be_minted_for_a_non_stream_path`.
fn path_is_ticketable(path: &str) -> bool {
    let segments: Vec<&str> = path.split('/').collect();
    // A leading `/` yields an empty first segment: ["", group, id, "events"].
    matches!(segments.as_slice(),
        ["", group, id, "events"]
            if (*group == "sessions" || *group == "workstreams") && !id.is_empty()
    )
}

/// Open one stream and answer it as Server-Sent Events.
///
/// What: dial, then `200 text/event-stream` immediately. Nothing is read before
/// the head is written.
///
/// **Why no peek, and what that costs.** trusty-console's bridge reads the first
/// frame before choosing a status, so a stream that refuses AT OPEN — the socket
/// contract sends that refusal as the stream's first `"stream":"error"` frame,
/// not as an ordinary response — becomes a real HTTP status rather than an empty
/// `200`. Copying that here breaks the pane it was meant to serve.
/// `workstream.events` is a pure live tail with no replay: a healthy stream on a
/// quiet workstream sends nothing at all, so peeking withholds the response head
/// until the first event happens, or until the open budget runs out. Measured on
/// a real `tcode serve`: `curl` on a freshly created workstream received zero
/// bytes and no status line, and would have waited out
/// [`super::STREAM_OPEN_TIMEOUT`]'s minute. The webview's `EventSource` reads
/// that as a connection that never opened and reconnect-loops.
///
/// A short peek window is not the fix. `FramedStream::next_frame` is not
/// cancel-safe — a cancelled read discards the bytes of a line already taken off
/// the socket — so a peek that timed out while a frame was arriving would drop
/// that event silently, which is a worse failure than the one it fixes.
///
/// What the refusal costs instead: it arrives as
/// [`trusty_common::uds::sse::sse_tail`]'s terminal `{"type":"error"}` event on a
/// `200`, plus its `warn!`. That is not a downgrade for THIS consumer —
/// `EventSource` cannot read a non-2xx body, so `openDaemonEventStream` fires
/// `onerror` and re-mints on its backoff whichever way the refusal is spelled.
/// The event and the log line are strictly more than the discarded `404` body
/// was.
///
/// `open_timeout` bounds the dial and the request write. The parameter exists so
/// a test need not wait the production minute.
/// Test: `a_healthy_but_silent_stream_gets_its_head_immediately` and
/// `a_stream_refusal_at_open_becomes_a_terminal_error_event` in
/// `tests/bridge_uds.rs`, plus
/// `a_socket_that_never_answers_is_a_prompt_bad_gateway` below.
pub(crate) async fn stream_response(
    socket: &Path,
    method: &'static str,
    params: Value,
    open_timeout: std::time::Duration,
) -> Response {
    let stream = match open_stream(socket, method, params, open_timeout).await {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };

    // `first` is `None`: nothing is read before the head. See the fn docs.
    sse_response(None, stream, method)
}

#[cfg(test)]
mod tests {
    use super::path_is_ticketable;

    /// Why: a ticket minted for an arbitrary path would be the arbitrary
    /// authenticated request the path binding exists to prevent.
    /// Test: this is the test.
    #[test]
    fn only_the_two_event_streams_are_ticketable() {
        assert!(path_is_ticketable("/sessions/s1/events"));
        assert!(path_is_ticketable("/workstreams/w1/events"));
        for refused in [
            "/rpc",
            "/sessions",
            "/sessions//events",
            "/sessions/s1/transcript",
            "/agents/a/events",
            "sessions/s1/events",
        ] {
            assert!(
                !path_is_ticketable(refused),
                "{refused} must not be ticketable"
            );
        }
    }
}
