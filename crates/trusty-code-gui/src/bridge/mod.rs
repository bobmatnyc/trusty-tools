//! The webview's HTTP face onto the `tcode` daemon's Unix socket (#6637).
//!
//! Why: the owner ruling on #6637 is that trusty-code speaks UDS natively and a
//! separate UI crate answers the HTTP a webview needs. Before this module the
//! webview `fetch()`ed the daemon's own TCP listener directly, which is the
//! listener PR 2c deletes. A webview cannot dial a Unix socket, so something in
//! this process has to; this module is that something, and it lives here rather
//! than in trusty-console because ADR-0032 scopes "console is the only HTTP
//! surface" to the trusty-* DAEMONS — a desktop shell serving its own webview is
//! not a second daemon surface.
//!
//! What: the method names this crate dials, the socket resolution every caller
//! shares, one unary exchange, one stream open, and the mapping from a JSON-RPC
//! refusal back to the HTTP status the daemon's own REST layer sent for it.
//! [`crate::bridge::map`] owns which path becomes which method,
//! [`crate::bridge::routes`] owns the axum side, and
//! [`crate::bridge::serve`] binds the listener.
//!
//! **There is no HTTP fallback to the daemon.** The daemon's TCP listener is
//! transient (PR 1 kept it only until this bridge existed), and a fallback that
//! resolves `127.0.0.1:7882` is not a safety net — it is a way to report
//! whatever now holds that port as a healthy `tcode`. A daemon with no socket
//! reads as unreachable, which is what it is.
//!
//! Test: `status_matches_the_daemons_own_rest_mapping`,
//! `socket_app_name_matches_the_daemon_resolver`,
//! `call_reports_a_dead_socket_as_unreachable`,
//! `call_reports_a_jsonrpc_error_with_the_http_status_it_came_from`,
//! `call_reports_an_empty_answer_as_malformed`, plus `tests/bridge_uds.rs`,
//! which drives the whole router against a stub daemon socket.

pub mod map;
pub mod routes;
pub mod serve;
pub mod transcript_md;

use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::body::Body;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use trusty_common::uds::server::RpcResponse;
use trusty_common::uds::stream_client::FramedStream;

/// The daemon this bridge dials, as `trusty_common::daemon_addr` names it.
///
/// A literal rather than an import: `trusty-code` is a dev-dependency here (it
/// would otherwise link the whole daemon into a desktop shell), so the equality
/// is a test rather than a compile-time fact.
/// Test: `socket_app_name_matches_the_daemon_resolver`.
pub const SOCKET_APP_NAME: &str = "trusty-code";

/// Live session events — `GET /sessions/{id}/events`, as a stream.
///
/// Test: `stream_method_names_match_the_daemons`.
pub const METHOD_SESSION_EVENTS: &str = "session.events";

/// Live workstream events — `GET /workstreams/{id}/events`, as a stream.
///
/// Test: `stream_method_names_match_the_daemons`.
pub const METHOD_WORKSTREAM_EVENTS: &str = "workstream.events";

/// How long one unary exchange may take, end to end.
///
/// Why longer than `tcode tui`'s 15 s: this bridge carries `session.create` and
/// `task.run`, which resolve a project binding and spawn a runner before they
/// answer, and `session.get_transcript`, which serialises a whole run. The TUI
/// dials none of those on this budget.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// How long opening a stream may take before the bridge gives up on it.
///
/// Why separate from [`STREAM_FRAME_TIMEOUT`]: that budget is a day, and one
/// figure applied to the dial, the write and the first read alike would leave
/// the webview waiting a day for a response head from a listener whose backlog
/// is full — such a listener accepts the connection and then reads nothing. An
/// ABSENT socket is not this case; it fails immediately with `ENOENT`.
pub const STREAM_OPEN_TIMEOUT: Duration = Duration::from_secs(60);

/// The per-frame budget on an open stream — effectively none.
///
/// Why so large: this bounds each FRAME read, and both event streams are tails.
/// A workstream whose session is waiting on a model emits nothing for as long
/// as that takes, and the webview's `openDaemonEventStream` treats a closed
/// stream as a drop to reconnect through — so a budget short enough to cut an
/// idle tail would churn a fresh ticket and a fresh subscription for no reason.
/// What: a day. Not `Duration::MAX`, which overflows tokio's timer when it is
/// added to `Instant::now()`.
pub const STREAM_FRAME_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

// ─── the codes the daemon's refusals carry ───────────────────────────────────
//
// `crates/trusty-code/src/serve/rest/mod.rs`'s `rpc_error_to_status` is the
// mapping the webview has been reading since #2983; [`BridgeError::status`]
// below is that table, copied. It is a copy rather than an import for the same
// reason `SOCKET_APP_NAME` is, and `status_matches_the_daemons_own_rest_mapping`
// pins the two together for as long as both exist — PR 2c deletes the daemon's.

/// HTTP 404 — `RpcError::not_found`.
const CODE_NOT_FOUND: i64 = -32002;
/// HTTP 404 — `RpcError::session_not_found`.
const CODE_SESSION_NOT_FOUND: i64 = -32007;
/// HTTP 403 — `RpcError::permission_denied`.
const CODE_PERMISSION_DENIED: i64 = -32001;
/// HTTP 400 — `RpcError::invalid_argument`.
const CODE_INVALID_ARGUMENT: i64 = -32003;
/// HTTP 409 — `RpcError::active_conflict` (DOC-48 §5.2).
const CODE_ACTIVE_CONFLICT: i64 = -32008;
/// HTTP 409 — `RpcError::already_exists`.
const CODE_ALREADY_EXISTS: i64 = -32009;
/// HTTP 400 — JSON-RPC `Invalid params`.
const CODE_INVALID_PARAMS: i64 = -32602;

/// Why one exchange with the daemon did not produce an answer.
///
/// Why the four arms are separate: the webview must not render "the daemon is
/// not running" and "the daemon said no" the same way, and neither may render
/// as an empty success — the fail-open branch this whole module exists to
/// close.
/// Test: `status_matches_the_daemons_own_rest_mapping`, and the `call_reports_*`
/// tests below.
#[derive(Debug)]
pub enum BridgeError {
    /// The socket path itself could not be resolved — an unusable data
    /// directory, not a daemon that is down.
    Unresolved(String),
    /// Nothing answered on the socket, or the exchange failed in transport.
    Unreachable(String),
    /// The daemon answered with a JSON-RPC `error` frame.
    Refused {
        /// The daemon's own JSON-RPC error code.
        code: i64,
        /// The daemon's own words.
        message: String,
    },
    /// The daemon answered, but with neither a result nor an error.
    Malformed(String),
}

impl BridgeError {
    /// The HTTP status this failure surfaces as.
    ///
    /// Why: the webview branches on status — `WorkstreamActivity`'s poller
    /// treats a `404` as "this session is gone" and anything else as an error
    /// line — so a refusal has to arrive as the status the daemon's REST route
    /// sent for the same condition.
    /// What: the arms of `trusty_code::serve::rest::rpc_error_to_status`, plus
    /// `502` for the three arms that are this bridge's own observation. An
    /// unmapped code is `500`, never a `200` with an error body.
    /// Test: `status_matches_the_daemons_own_rest_mapping`.
    #[must_use]
    pub fn status(&self) -> StatusCode {
        match self {
            Self::Unresolved(_) | Self::Unreachable(_) | Self::Malformed(_) => {
                StatusCode::BAD_GATEWAY
            }
            Self::Refused { code, .. } => match *code {
                CODE_NOT_FOUND | CODE_SESSION_NOT_FOUND => StatusCode::NOT_FOUND,
                CODE_PERMISSION_DENIED => StatusCode::FORBIDDEN,
                CODE_INVALID_ARGUMENT | CODE_INVALID_PARAMS => StatusCode::BAD_REQUEST,
                CODE_ACTIVE_CONFLICT | CODE_ALREADY_EXISTS => StatusCode::CONFLICT,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            },
        }
    }

    /// The daemon's own words, or this bridge's account of why it heard none.
    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::Unresolved(m) | Self::Unreachable(m) | Self::Malformed(m) => m,
            Self::Refused { message, .. } => message,
        }
    }
}

impl IntoResponse for BridgeError {
    /// Render the failure as the JSON body the webview reads.
    ///
    /// The daemon's REST layer answered a `Response::err` envelope; the webview
    /// reads a non-2xx body as text and puts it in the error line either way,
    /// so this carries the daemon's wording under an `error` key rather than
    /// re-deriving a JSON-RPC envelope with no id to echo.
    /// Test: `a_dead_socket_is_a_bad_gateway_not_an_empty_success` in
    /// `tests/bridge_uds.rs`.
    fn into_response(self) -> Response {
        let status = self.status();
        let body = json!({ "error": self.message(), "service": SOCKET_APP_NAME });
        (status, axum::Json(body)).into_response()
    }
}

/// Where the daemon's socket is, or why the bridge could not work it out.
///
/// Why the error is carried rather than discarded: an unresolvable data
/// directory is operator-fixable (permissions, a `TRUSTY_DATA_DIR_OVERRIDE`
/// pointing somewhere unusable) and is indistinguishable in the webview from a
/// daemon that is simply not running.
/// What: `trusty_common::daemon_addr::daemon_socket_path`, the ONE resolver the
/// daemon itself calls, so there is no second answer to where the socket is.
///
/// # Errors
///
/// When the data directory cannot be resolved or created.
///
/// Test: `socket_app_name_matches_the_daemon_resolver`.
pub fn socket_path() -> Result<PathBuf, String> {
    trusty_common::daemon_addr::daemon_socket_path(SOCKET_APP_NAME)
        .map_err(|e| format!("could not resolve the {SOCKET_APP_NAME} socket path: {e:#}"))
}

/// One unary JSON-RPC exchange with the daemon.
///
/// Why here rather than at each call site: the envelope, the framing, the
/// timeout and the two ways an exchange fails before a handler runs are
/// identical for every method, and a second copy of them is how one caller
/// starts reading an `error` frame as a success while another does not.
///
/// What: one framed request on the shared 8 MiB frame budget — the same budget
/// `tcode tui` dials with and the same one the daemon's listener reads on, so
/// there is one figure in play rather than three. A response carrying `error`
/// is [`BridgeError::Refused`] with the daemon's code and message; a response
/// carrying neither half is [`BridgeError::Malformed`], never an empty success.
///
/// # Errors
///
/// Every arm of [`BridgeError`] except [`BridgeError::Unresolved`], which is
/// the caller's to raise when it cannot name a socket at all.
///
/// Test: `call_reports_a_dead_socket_as_unreachable`,
/// `call_reports_a_jsonrpc_error_with_the_http_status_it_came_from`,
/// `call_reports_an_empty_answer_as_malformed`.
pub async fn call(
    socket: &Path,
    method: &str,
    params: Value,
    timeout: Duration,
) -> Result<Value, BridgeError> {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    });

    let response: RpcResponse = trusty_common::uds::send_framed_request(socket, &request, timeout)
        .await
        .map_err(|e| {
            BridgeError::Unreachable(format!("{SOCKET_APP_NAME} did not answer {method}: {e}"))
        })?;

    if let Some(error) = response.error {
        return Err(BridgeError::Refused {
            code: error.code,
            message: error.message,
        });
    }

    response.result.ok_or_else(|| {
        BridgeError::Malformed(format!(
            "{SOCKET_APP_NAME} answered {method} with neither a result nor an error"
        ))
    })
}

/// Open one streaming JSON-RPC exchange with the daemon.
///
/// Why `"stream": true` is set here: the server negotiates on that field
/// (`trusty_common::uds::server`'s wire contract) and a streaming method called
/// without it answers `CODE_STREAM_REQUIRED`. Setting it at the one place that
/// opens a stream is what keeps a caller from having to know that.
///
/// What: dials and writes the request frame, then hands back the reader.
/// Nothing has been read yet — the first frame may still be the daemon's
/// refusal, which is why [`crate::bridge::routes`] reads it before choosing an
/// HTTP status.
///
/// Why `open_timeout` wraps the helper instead of being passed to it: the
/// helper takes ONE figure for the dial, the request write, and each frame
/// read, and the frame read needs [`STREAM_FRAME_TIMEOUT`]'s day. Wrapping
/// bounds only the open while the established stream keeps the long per-frame
/// budget.
///
/// # Errors
///
/// [`BridgeError::Unreachable`] for a dial or write failure, and for an open
/// that outlasts `open_timeout`.
///
/// Test: `open_stream_reports_a_dead_socket_as_unreachable`, and
/// `a_healthy_but_silent_stream_gets_its_head_immediately` in
/// `tests/bridge_uds.rs` for the open that succeeds.
pub async fn open_stream(
    socket: &Path,
    method: &str,
    params: Value,
    open_timeout: Duration,
) -> Result<FramedStream<Value>, BridgeError> {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
        "stream": true,
    });

    let opened = tokio::time::timeout(
        open_timeout,
        trusty_common::uds::stream_client::send_framed_stream_request(
            socket,
            &request,
            STREAM_FRAME_TIMEOUT,
        ),
    )
    .await
    .map_err(|_| {
        BridgeError::Unreachable(format!(
            "{SOCKET_APP_NAME} did not open {method} within {}s",
            open_timeout.as_secs_f32()
        ))
    })?;

    opened.map_err(|e| {
        BridgeError::Unreachable(format!("{SOCKET_APP_NAME} did not open {method}: {e}"))
    })
}

/// Render a `serde_json::Value` as the JSON body the daemon's REST route sent.
///
/// Why not `axum::Json`: the daemon's handlers already answer the exact
/// document its axum handler serialised, so re-encoding through a typed wrapper
/// would be a second chance to differ from it. `status` carries the `201` and
/// `202` the REST layer answered for creates and for `task.run`.
/// Test: `unary_route_returns_the_daemon_body_verbatim` and
/// `a_create_keeps_the_rest_layers_201` in `tests/bridge_uds.rs`.
#[must_use]
pub fn json_response(status: StatusCode, value: &Value) -> Response {
    match serde_json::to_vec(value) {
        Ok(bytes) => Response::builder()
            .status(status)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(bytes))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
        Err(e) => BridgeError::Malformed(format!(
            "{SOCKET_APP_NAME} answered a body the bridge could not re-encode: {e}"
        ))
        .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bind a socket that answers one framed request with `reply`.
    pub(crate) fn stub_daemon(dir: &Path, reply: impl Into<String>) -> PathBuf {
        let socket = dir.join("sockets").join("tcode.sock");
        let reply = reply.into();
        let listener = trusty_common::uds::bind_hardened(&socket).expect("bind");
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
            let Ok((mut conn, _)) = listener.accept().await else {
                return;
            };
            let mut sink = Vec::new();
            let _ = conn.read_to_end(&mut sink).await;
            let _ = conn.write_all(reply.as_bytes()).await;
            let _ = conn.write_all(b"\n").await;
            let _ = conn.flush().await;
        });
        socket
    }

    /// Why: this table is a COPY of the daemon's, and a copy that drifts sends
    /// the webview a `500` where it used to read a `404` — which its poller
    /// renders as an error line instead of "the session is gone". Pinning it
    /// against the original is what makes the copy safe for the window before
    /// PR 2c deletes the original.
    /// Test: this is the test.
    #[test]
    fn status_matches_the_daemons_own_rest_mapping() {
        use trusty_code::jsonrpc::RpcError;
        use trusty_code::serve::rest::rpc_error_to_status;

        let cases = [
            RpcError::not_found("x"),
            RpcError::session_not_found("s-1"),
            RpcError::permission_denied("x"),
            RpcError::invalid_argument("x"),
            RpcError::invalid_params("x"),
            RpcError::internal("x"),
            RpcError::active_conflict("ws-1"),
            RpcError::already_exists("dup"),
            RpcError::new(-1, "custom"),
        ];
        for err in cases {
            // The daemon's `RpcError` carries an `i32`; the socket envelope
            // widens it to `i64`. Widening here is what compares the same code
            // through both tables.
            let mine = BridgeError::Refused {
                code: i64::from(err.code),
                message: err.message.clone(),
            };
            assert_eq!(
                mine.status(),
                rpc_error_to_status(&err),
                "code {} must map the same way the daemon's REST layer maps it",
                err.code
            );
        }
    }

    /// Why: the bridge dials the path the daemon binds. A drift here is a
    /// permanent `502` with no hint why.
    /// Test: this is the test.
    #[test]
    fn socket_app_name_matches_the_daemon_resolver() {
        assert_eq!(SOCKET_APP_NAME, trusty_code::serve::uds::SOCKET_APP_NAME);
    }

    /// Why: the two stream names are literals here and constants there; a typo
    /// reaches the webview as an empty event stream, which its activity pane
    /// renders as a workstream with nothing happening.
    /// Test: this is the test.
    #[test]
    fn stream_method_names_match_the_daemons() {
        assert_eq!(
            METHOD_SESSION_EVENTS,
            trusty_code::session::events_stream::METHOD
        );
        assert_eq!(
            METHOD_WORKSTREAM_EVENTS,
            trusty_code::workstreams::events_stream::METHOD
        );
    }

    /// Why: a daemon that is not running must be distinguishable from a healthy
    /// one with nothing to show.
    /// Test: this is the test.
    #[tokio::test]
    async fn call_reports_a_dead_socket_as_unreachable() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let err = call(
            &tmp.path().join("absent.sock"),
            "session.list",
            json!({}),
            Duration::from_secs(2),
        )
        .await
        .expect_err("an absent socket must fail");
        assert!(matches!(err, BridgeError::Unreachable(_)), "{err:?}");
        assert_eq!(err.status(), StatusCode::BAD_GATEWAY);
    }

    /// Why: a refusal carries a code, and that code is the whole of what the
    /// webview branches on.
    /// Test: this is the test.
    #[tokio::test]
    async fn call_reports_a_jsonrpc_error_with_the_http_status_it_came_from() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let socket = stub_daemon(
            tmp.path(),
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "error": { "code": CODE_SESSION_NOT_FOUND, "message": "no session 's-9'" },
            })
            .to_string(),
        );
        let err = call(&socket, "session.status", json!({}), Duration::from_secs(5))
            .await
            .expect_err("the stub refuses");
        assert_eq!(err.status(), StatusCode::NOT_FOUND);
        assert!(err.message().contains("s-9"), "{}", err.message());
    }

    /// Why: an envelope with neither half is a daemon bug, and answering `200`
    /// with `null` would hide it inside a rendered pane.
    /// Test: this is the test.
    #[tokio::test]
    async fn call_reports_an_empty_answer_as_malformed() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let socket = stub_daemon(tmp.path(), json!({ "jsonrpc": "2.0", "id": 1 }).to_string());
        let err = call(&socket, "session.list", json!({}), Duration::from_secs(5))
            .await
            .expect_err("an empty envelope must fail");
        assert!(matches!(err, BridgeError::Malformed(_)), "{err:?}");
        assert_eq!(err.status(), StatusCode::BAD_GATEWAY);
    }

    /// Why: the stream open has its own failure path, and a dead socket must
    /// reach it as a gateway failure rather than as an empty `200` stream.
    /// Test: this is the test.
    #[tokio::test]
    async fn open_stream_reports_a_dead_socket_as_unreachable() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let err = open_stream(
            &tmp.path().join("absent.sock"),
            METHOD_SESSION_EVENTS,
            json!({ "session_id": "s-1" }),
            Duration::from_secs(2),
        )
        .await
        .expect_err("an absent socket must fail");
        assert_eq!(err.status(), StatusCode::BAD_GATEWAY);
    }
}
