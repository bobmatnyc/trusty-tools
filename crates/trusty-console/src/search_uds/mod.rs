//! trusty-console's one client to trusty-search, over the daemon's Unix socket
//! (#6285).
//!
//! Why: ADR-0032 leaves trusty-console as the workspace's only HTTP surface, and
//! #6285 deletes trusty-search's own. Until this module existed the console
//! reached trusty-search three ways — the reverse proxy resolved a base URL from
//! the `http_addr` discovery file, `detect::SearchConnector` read that same file
//! and probed the port, and `routes::deletes` dialled `DELETE /indexes/{id}` on
//! it. All three stop resolving the moment the daemon stops writing that file,
//! and a stale file left by a pre-migration daemon is worse than none: it names
//! a port that whatever now holds 7878 will answer. #6286 and #6287 deleted the
//! memory and analyze rows from the proxy for exactly that reason; this module
//! is what lets the search row go the same way without taking the SPA with it.
//!
//! What: the method names this crate dials, the socket-path resolution every
//! caller shares, one unary exchange, one stream open, and the mapping from a
//! JSON-RPC refusal back to the HTTP status the same refusal used to carry.
//!
//! **There is no HTTP fallback.** trusty-memory and trusty-analyze migrated with
//! none, and the reason is the stale discovery file above — a fallback that
//! resolves `127.0.0.1:7878` is not a safety net, it is a way to report an
//! unrelated process as a healthy trusty-search. A daemon with no socket reads
//! as unreachable, which is what it is.
//!
//! Test: `error_status_maps_every_documented_code`,
//! `call_reports_a_dead_socket_as_unreachable`,
//! `call_reports_a_jsonrpc_error_with_the_http_status_it_came_from`,
//! `call_reports_an_empty_answer_as_malformed`.

pub(crate) mod map;
pub(crate) mod routes;

use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::body::Body;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use trusty_common::uds::server::{
    CODE_INTERNAL_ERROR, CODE_INVALID_PARAMS, CODE_INVALID_REQUEST, CODE_METHOD_NOT_FOUND,
    CODE_PARSE_ERROR, RpcResponse,
};
use trusty_common::uds::stream_client::FramedStream;

/// The daemon this module dials, as `trusty_common::daemon_socket_path` names it.
pub(crate) const SEARCH_SERVICE: &str = "trusty-search";

// ─── the method names ────────────────────────────────────────────────────────
//
// Duplicated as literals rather than imported: trusty-console has no Cargo edge
// on trusty-search and adding one would pull a whole search engine into the
// console's build. `trusty_search::service::socket::METHODS` is the definition;
// these are the client's copy, and the contract test the retire slice adds to
// trusty-search is what keeps them equal — the same arrangement
// `detect::AnalyzeConnector` records for `analyze.health`.

/// Liveness, index count and the daemon's version.
pub(crate) const METHOD_HEALTH: &str = "search.health";
/// The index roster — `GET /indexes`.
pub(crate) const METHOD_INDEXES_LIST: &str = "search.indexes.list";
/// Register an index — `POST /indexes`.
pub(crate) const METHOD_INDEX_CREATE: &str = "search.index.create";
/// Deregister an index — `DELETE /indexes/{id}`.
pub(crate) const METHOD_INDEX_DELETE: &str = "search.index.delete";
/// One index's status — `GET /indexes/{id}/status`.
pub(crate) const METHOD_INDEX_STATUS: &str = "search.index.status";
/// One index's hygiene config — `GET /indexes/{id}/config`.
pub(crate) const METHOD_INDEX_CONFIG_GET: &str = "search.index.config.get";
/// Patch one index's hygiene config — `PATCH /indexes/{id}/config`.
pub(crate) const METHOD_INDEX_CONFIG_SET: &str = "search.index.config.set";
/// Per-index hybrid search — `POST /indexes/{id}/search`.
pub(crate) const METHOD_QUERY: &str = "search.query";
/// Cross-index fan-out search — `POST /search`.
pub(crate) const METHOD_QUERY_ALL: &str = "search.query.all";
/// Trigger a reindex — `POST /indexes/{id}/reindex`.
pub(crate) const METHOD_INDEX_REINDEX: &str = "search.index.reindex";
/// Daemon memory-limit config — `GET /config`.
pub(crate) const METHOD_CONFIG_GET: &str = "search.config.get";
/// Patch the daemon config — `PATCH /config`.
pub(crate) const METHOD_CONFIG_SET: &str = "search.config.set";
/// The in-memory log ring — `GET /logs/tail`.
pub(crate) const METHOD_LOGS_TAIL: &str = "search.logs.tail";
/// The stale-registration census — `GET /registry/orphans`.
pub(crate) const METHOD_REGISTRY_ORPHANS: &str = "search.registry.orphans";
/// Live daemon events — `GET /status/stream`, as a stream.
pub(crate) const METHOD_STATUS_STREAM: &str = "search.status.stream";
/// One index's reindex progress — `GET /indexes/{id}/reindex/stream`, as a stream.
pub(crate) const METHOD_INDEX_REINDEX_STREAM: &str = "search.index.reindex.stream";

/// How long one unary exchange may take, end to end.
///
/// Matches `routes::ACTION_TIMEOUT`: a delete waits for in-flight writers to
/// quiesce and a fan-out query walks every registered corpus, so this is bounded
/// by disk work rather than by a round trip.
pub(crate) const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the health probe may take.
///
/// Shorter than [`CALL_TIMEOUT`] and for a different reason: the console's
/// detection pass runs six connectors, and one wedged daemon must not stall the
/// dashboard. Matches the figure `detect::AnalyzeConnector` uses.
pub(crate) const HEALTH_TIMEOUT: Duration = Duration::from_secs(3);

/// The per-frame budget on an open stream — effectively none.
///
/// Why so large: `send_framed_stream_request` applies this to EACH frame read,
/// and `search.index.reindex.stream` emits per batch. A reindex whose embedder
/// sidecar stalls emits nothing for the length of the stall, and the SPA reads a
/// closed stream as "the reindex finished"
/// (`crates/trusty-console/ui-search/src/lib/views/Indexes.svelte`) — so a budget short
/// enough to cut a stall would report a still-running reindex as complete. That
/// is the same trade #6155 recorded when it gave the proxy's stream client a
/// silence bound rather than a total one.
/// What: a day. Not `Duration::MAX`, which overflows tokio's timer when it is
/// added to `Instant::now()`.
///
/// The same value also bounds the dial and the request write inside the shared
/// helper, which takes one figure for both. [`STREAM_OPEN_TIMEOUT`] is what
/// bounds the open, wrapped around the call rather than passed into it.
pub(crate) const STREAM_FRAME_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

/// How long opening a stream may take before the console gives up on it.
///
/// Why separate from [`STREAM_FRAME_TIMEOUT`]: that budget is a day, and
/// `send_framed_stream_request_capped` applies its ONE figure to the dial, the
/// request write, and every frame read alike. A socket whose backlog is full
/// accepts the connect and then never reads, so the dial and the write both
/// park — and with only the day-long figure in play the browser's reindex
/// request would hang for a day rather than being told the daemon is
/// unreachable. An ABSENT socket is not this case; it fails immediately with
/// `ENOENT`.
/// What: 60 s for the whole open — the dial, the write, and the first frame
/// read, which is the last step before a response head exists. `routes`'
/// `stream_response` takes ONE deadline before dialling and reuses it for the
/// first-frame read, so the figure here is the total rather than a per-step
/// budget two steps could each spend in full. Once the head is written the long
/// per-frame budget takes over, because a reindex legitimately emits nothing for
/// minutes.
/// Test: `a_socket_that_never_answers_is_a_prompt_bad_gateway` and
/// `a_slow_open_and_a_silent_first_frame_share_one_budget` in [`routes`].
pub(crate) const STREAM_OPEN_TIMEOUT: Duration = Duration::from_secs(60);

/// The frame budget this client applies — at least the listener's.
///
/// Why not the shared 8 MiB default: `trusty_search::service::socket`'s
/// `MAX_FRAME_BYTES` is 64 MiB — the figure `POST /indexes/{id}/graph` carries
/// as its `DefaultBodyLimit` — and slice 5.5 states that a consumer must raise
/// its own budget to match or a request the listener ACCEPTED comes back as
/// `FrameTooLarge`, because the default applies to the response read too. The
/// console proxies `search.query.all`, which can pass 8 MiB whenever a caller
/// asks for full content at a large `top_k`, so the plain helper would fail that
/// call after the daemon had already done the work.
///
/// The invariant is a FLOOR, not an equality: this figure must be at least the
/// listener's. Smaller breaks a response the daemon has already produced;
/// larger has no failure mode, because the listener refuses an oversized REQUEST
/// on its own terms and never sends a frame past its own budget.
///
/// The same figure bounds the HTTP request body [`routes`] will carry, so the
/// bridge refuses a body over the frame budget before copying 64 MiB of it.
/// Test: `a_response_over_the_shared_default_is_read` and
/// `the_frame_budget_is_at_least_the_listeners`.
pub(crate) const MAX_FRAME_BYTES: u64 = 64 * 1024 * 1024;

// ─── the codes trusty-search's refusals carry ────────────────────────────────
//
// `crates/trusty-search/src/service/rpc/error.rs` projects an HTTP status onto
// one of these; [`SearchRpcError::status`] projects it back, so a refusal the
// SPA used to read as `404 Not Found` still reads as one through this bridge.

/// HTTP 404 — `rpc::error::CODE_NOT_FOUND`.
const CODE_NOT_FOUND: i64 = -32004;
/// HTTP 503, retryable — `rpc::error::CODE_UNAVAILABLE`.
const CODE_UNAVAILABLE: i64 = -32002;
/// HTTP 503, permanent — `rpc::error::CODE_UNAVAILABLE_PERMANENT`.
const CODE_UNAVAILABLE_PERMANENT: i64 = -32012;
/// HTTP 408 — `rpc::error::CODE_DEADLINE_EXCEEDED`.
const CODE_DEADLINE_EXCEEDED: i64 = -32005;
/// HTTP 403 — `rpc::error::CODE_FORBIDDEN`.
const CODE_FORBIDDEN: i64 = -32003;
/// HTTP 409 — `rpc::error::CODE_CONFLICT`.
const CODE_CONFLICT: i64 = -32009;
/// HTTP 429 — `rpc::error::CODE_TOO_MANY_REQUESTS`.
const CODE_TOO_MANY_REQUESTS: i64 = -32013;

/// Why one exchange with trusty-search did not produce an answer.
///
/// Why the three arms are separate: the dashboard must not render "the daemon
/// is not running" and "the daemon said no" the same way, and neither may render
/// as an empty success. That is the fail-open branch this whole module exists to
/// close — see [`call`].
/// Test: `error_status_maps_every_documented_code`, and the `call_reports_*`
/// tests below.
#[derive(Debug)]
pub(crate) enum SearchRpcError {
    /// The socket path itself could not be resolved — an unusable data
    /// directory, not a daemon that is down.
    Unresolved(String),
    /// Nothing answered on the socket, or the exchange failed in transport.
    Unreachable(String),
    /// The daemon answered with a JSON-RPC `error` frame.
    Refused { code: i64, message: String },
    /// The daemon answered, but with neither a result nor an error.
    Malformed(String),
}

impl SearchRpcError {
    /// The HTTP status this failure surfaces as.
    ///
    /// Why: the SPA branches on status (`api.js`'s `ApiError`), so a refusal has
    /// to arrive as the status the HTTP route sent for the same condition. An
    /// unmapped code is `500` rather than `200` with an error body — no failure
    /// arm here may reach the browser as a success.
    /// What: the inverse of `trusty-search`'s `rpc_error_from_http` table, plus
    /// `502` for the two arms that are the console's own observation.
    /// Test: `error_status_maps_every_documented_code`.
    pub(crate) fn status(&self) -> StatusCode {
        match self {
            Self::Unresolved(_) | Self::Unreachable(_) | Self::Malformed(_) => {
                StatusCode::BAD_GATEWAY
            }
            Self::Refused { code, .. } => match *code {
                CODE_NOT_FOUND => StatusCode::NOT_FOUND,
                CODE_UNAVAILABLE | CODE_UNAVAILABLE_PERMANENT => StatusCode::SERVICE_UNAVAILABLE,
                CODE_DEADLINE_EXCEEDED => StatusCode::REQUEST_TIMEOUT,
                CODE_FORBIDDEN => StatusCode::FORBIDDEN,
                CODE_CONFLICT => StatusCode::CONFLICT,
                CODE_TOO_MANY_REQUESTS => StatusCode::TOO_MANY_REQUESTS,
                CODE_INVALID_PARAMS | CODE_INVALID_REQUEST => StatusCode::BAD_REQUEST,
                CODE_METHOD_NOT_FOUND => StatusCode::NOT_IMPLEMENTED,
                CODE_PARSE_ERROR | CODE_INTERNAL_ERROR => StatusCode::INTERNAL_SERVER_ERROR,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            },
        }
    }

    /// The daemon's own words, or the console's account of why it heard none.
    pub(crate) fn message(&self) -> &str {
        match self {
            Self::Unresolved(m) | Self::Unreachable(m) | Self::Malformed(m) => m,
            Self::Refused { message, .. } => message,
        }
    }
}

impl IntoResponse for SearchRpcError {
    /// Render the failure as the JSON body the SPA reads.
    ///
    /// `api.js` reads a non-2xx body as text and puts it in the thrown
    /// `ApiError`'s message, so the daemon's wording reaches the operator
    /// whatever the status was.
    /// Test: `a_dead_socket_is_a_bad_gateway_not_an_empty_success` in
    /// `tests/search_uds_bridge.rs`.
    fn into_response(self) -> Response {
        let status = self.status();
        let body = json!({
            "error": self.message(),
            "service": SEARCH_SERVICE,
        });
        (status, axum::Json(body)).into_response()
    }
}

/// Where trusty-search's socket is, or why the console could not work it out.
///
/// Why the error is carried rather than discarded: an unresolvable data
/// directory is operator-fixable (permissions, a `TRUSTY_DATA_DIR_OVERRIDE`
/// pointing somewhere unusable) and is indistinguishable on the dashboard from a
/// daemon that is simply not running. The reason has to survive to the caller —
/// the same argument `detect::AnalyzeConnector::socket_path` records.
/// What: `trusty_common::daemon_socket_path`, the ONE resolver the daemon itself
/// calls (`trusty_search::service::socket::socket_path`), so there is no second
/// answer to where the socket is.
/// Test: `socket_path_matches_the_daemon_resolver`.
pub(crate) fn socket_path() -> Result<PathBuf, String> {
    trusty_common::daemon_socket_path(SEARCH_SERVICE)
        .map_err(|e| format!("could not resolve the {SEARCH_SERVICE} socket path: {e:#}"))
}

/// One unary JSON-RPC exchange with trusty-search.
///
/// Why here rather than at each call site: the envelope, the framing, the
/// timeout and the two ways an exchange fails before a handler runs are
/// identical for every method, and a second copy of them is how one caller
/// starts reading an `error` frame as a success while another does not — the
/// argument `routes::memory_rpc` records for the trusty-memory side.
///
/// What: one framed request, then the envelope check. A response carrying
/// `error` is [`SearchRpcError::Refused`] with the daemon's code and message; a
/// response carrying neither half is [`SearchRpcError::Malformed`], never an
/// empty success.
///
/// # Errors
///
/// Every arm of [`SearchRpcError`].
///
/// Test: `call_reports_a_dead_socket_as_unreachable`,
/// `call_reports_a_jsonrpc_error_with_the_http_status_it_came_from`,
/// `call_reports_an_empty_answer_as_malformed`,
/// `call_returns_the_daemon_result`.
pub(crate) async fn call(
    socket: &Path,
    method: &str,
    params: Value,
    timeout: Duration,
) -> Result<Value, SearchRpcError> {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    });

    let response: RpcResponse =
        trusty_common::uds::send_framed_request_capped(socket, &request, timeout, MAX_FRAME_BYTES)
            .await
            .map_err(|e| {
                SearchRpcError::Unreachable(format!(
                    "{SEARCH_SERVICE} did not answer {method}: {e}"
                ))
            })?;

    if let Some(error) = response.error {
        return Err(SearchRpcError::Refused {
            code: error.code,
            message: error.message,
        });
    }

    response.result.ok_or_else(|| {
        SearchRpcError::Malformed(format!(
            "{SEARCH_SERVICE} answered {method} with neither a result nor an error"
        ))
    })
}

/// Open one streaming JSON-RPC exchange with trusty-search.
///
/// Why `"stream": true` is set here: the server negotiates on that field
/// (`trusty_common::uds::server`'s wire contract) and a streaming method called
/// without it answers `CODE_STREAM_REQUIRED`. Setting it at the one place that
/// opens a stream is what keeps a caller from having to know that.
///
/// What: dials and writes the request frame, then hands back the reader. Nothing
/// has been read yet — the first frame may still be the server's refusal, which
/// is why [`routes`] reads it before choosing an HTTP status.
///
/// Why `open_timeout` wraps the helper instead of being passed to it: the helper
/// takes ONE figure for the dial, the request write, and each frame read, and
/// the frame read needs [`STREAM_FRAME_TIMEOUT`]'s day. Wrapping bounds only the
/// open — a wedged listener answers in a minute — while the established stream
/// keeps the long per-frame budget.
///
/// `open_timeout` bounds the dial and the write only. The caller owns the rest
/// of the open: `routes::stream_response` computes one deadline before calling
/// this and bounds the first frame read against the same deadline, so a slow but
/// successful open leaves the peek less than the full figure rather than a
/// second copy of it.
///
/// # Errors
///
/// [`SearchRpcError::Unreachable`] for a dial or write failure, and for an open
/// that outlasts `open_timeout`.
///
/// Test: `open_stream_reports_a_dead_socket_as_unreachable`, and
/// `a_socket_that_never_answers_is_a_prompt_bad_gateway` in [`routes`].
pub(crate) async fn open_stream(
    socket: &Path,
    method: &str,
    params: Value,
    open_timeout: Duration,
) -> Result<FramedStream<Value>, SearchRpcError> {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
        "stream": true,
    });

    let opened = tokio::time::timeout(
        open_timeout,
        trusty_common::uds::stream_client::send_framed_stream_request_capped(
            socket,
            &request,
            STREAM_FRAME_TIMEOUT,
            MAX_FRAME_BYTES,
        ),
    )
    .await
    .map_err(|_| {
        SearchRpcError::Unreachable(format!(
            "{SEARCH_SERVICE} did not open {method} within {}s",
            open_timeout.as_secs_f32()
        ))
    })?;

    opened.map_err(|e| {
        SearchRpcError::Unreachable(format!("{SEARCH_SERVICE} did not open {method}: {e}"))
    })
}

/// Render a `serde_json::Value` as the JSON body an HTTP route would have sent.
///
/// Why not `axum::Json`: the daemon's cores already answer the exact document
/// the axum handler serialised, so re-encoding through a typed wrapper would be
/// a second chance to differ from it.
/// Test: `unary_route_returns_the_daemon_body_verbatim` in
/// `tests/search_uds_bridge.rs`.
pub(crate) fn json_response(value: &Value) -> Response {
    match serde_json::to_vec(value) {
        Ok(bytes) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(bytes))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
        Err(e) => SearchRpcError::Malformed(format!(
            "{SEARCH_SERVICE} answered a body the console could not re-encode: {e}"
        ))
        .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bind a socket that answers exactly one framed request with `reply`.
    ///
    /// The same stub shape `routes::memory_rpc`'s tests use, so the two clients
    /// are exercised against the same kind of daemon.
    pub(crate) fn stub_daemon(dir: &Path, reply: impl Into<String>) -> PathBuf {
        let socket = dir.join("sockets").join("search.sock");
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

    /// Why: the SPA branches on HTTP status, so every code trusty-search's
    /// `rpc_error_from_http` can produce has to come back out as the status it
    /// went in as. An unmapped code must be a 5xx, never a success.
    /// Test: this is the test.
    #[test]
    fn error_status_maps_every_documented_code() {
        let cases = [
            (CODE_NOT_FOUND, StatusCode::NOT_FOUND),
            (CODE_UNAVAILABLE, StatusCode::SERVICE_UNAVAILABLE),
            (CODE_UNAVAILABLE_PERMANENT, StatusCode::SERVICE_UNAVAILABLE),
            (CODE_DEADLINE_EXCEEDED, StatusCode::REQUEST_TIMEOUT),
            (CODE_FORBIDDEN, StatusCode::FORBIDDEN),
            (CODE_CONFLICT, StatusCode::CONFLICT),
            (CODE_TOO_MANY_REQUESTS, StatusCode::TOO_MANY_REQUESTS),
            (CODE_INVALID_PARAMS, StatusCode::BAD_REQUEST),
            (CODE_INVALID_REQUEST, StatusCode::BAD_REQUEST),
            (CODE_METHOD_NOT_FOUND, StatusCode::NOT_IMPLEMENTED),
            (CODE_INTERNAL_ERROR, StatusCode::INTERNAL_SERVER_ERROR),
            (CODE_PARSE_ERROR, StatusCode::INTERNAL_SERVER_ERROR),
            (-31999, StatusCode::INTERNAL_SERVER_ERROR),
        ];
        for (code, expected) in cases {
            let err = SearchRpcError::Refused {
                code,
                message: "refused".to_string(),
            };
            assert_eq!(err.status(), expected, "code {code}");
        }
    }

    /// Why: a daemon that is not running must never be reported as one that
    /// answered.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn call_reports_a_dead_socket_as_unreachable() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let err = call(
            &tmp.path().join("absent.sock"),
            METHOD_HEALTH,
            json!({}),
            Duration::from_secs(2),
        )
        .await
        .expect_err("a dead socket is not a success");
        assert!(matches!(err, SearchRpcError::Unreachable(_)), "{err:?}");
        assert_eq!(err.status(), StatusCode::BAD_GATEWAY);
    }

    /// Why: the fail-open branch this module closes. A JSON-RPC `error` is the
    /// daemon refusing, and it must reach the caller as the HTTP status the same
    /// refusal carried over HTTP — carrying the daemon's own wording.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn call_reports_a_jsonrpc_error_with_the_http_status_it_came_from() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let socket = stub_daemon(
            tmp.path(),
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32004,"message":"no index 'ghost'"}}"#,
        );
        let err = call(
            &socket,
            METHOD_INDEX_STATUS,
            json!({ "index_id": "ghost" }),
            Duration::from_secs(5),
        )
        .await
        .expect_err("an error frame is not a success");
        assert_eq!(err.status(), StatusCode::NOT_FOUND);
        assert!(err.message().contains("no index 'ghost'"), "{err:?}");
    }

    /// Why: a frame with neither half is a broken contract, and reading it as an
    /// empty success would render a healthy-but-empty dashboard for a daemon
    /// that told us nothing.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn call_reports_an_empty_answer_as_malformed() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let socket = stub_daemon(tmp.path(), r#"{"jsonrpc":"2.0","id":1}"#);
        let err = call(&socket, METHOD_HEALTH, json!({}), Duration::from_secs(5))
            .await
            .expect_err("an empty answer is not a success");
        assert!(matches!(err, SearchRpcError::Malformed(_)), "{err:?}");
        assert_eq!(err.status(), StatusCode::BAD_GATEWAY);
    }

    /// Why: the success path has to hand back the daemon's own document, since
    /// the SPA reads fields the console does not model.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn call_returns_the_daemon_result() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let socket = stub_daemon(
            tmp.path(),
            r#"{"jsonrpc":"2.0","id":1,"result":{"status":"ok","version":"9.9.9"}}"#,
        );
        let result = call(&socket, METHOD_HEALTH, json!({}), Duration::from_secs(5))
            .await
            .expect("the exchange succeeds");
        assert_eq!(result["version"], json!("9.9.9"));
    }

    /// Why: `search.query.all` with full content at a large `top_k` passes the
    /// shared 8 MiB default, and the default applies to the RESPONSE read — so
    /// under the plain helper that call would fail AFTER the daemon had done the
    /// work. This asserts the raised budget is actually in force.
    /// What: answers a result whose payload is over 8 MiB and reads it back.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_response_over_the_shared_default_is_read() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let big = "x".repeat(9 * 1024 * 1024);
        let reply = json!({ "jsonrpc": "2.0", "id": 1, "result": { "blob": big } }).to_string();
        assert!(
            reply.len() as u64 > trusty_common::uds::MAX_FRAME_BYTES,
            "the fixture must exceed the shared default to prove anything"
        );
        let socket = stub_daemon(tmp.path(), reply);
        let result = call(
            &socket,
            METHOD_QUERY_ALL,
            json!({}),
            Duration::from_secs(20),
        )
        .await
        .expect("an oversized-but-permitted response is read");
        assert_eq!(result["blob"].as_str().map(str::len), Some(9 * 1024 * 1024));
    }

    /// Why: a stream that cannot be opened must not read as an empty stream —
    /// the SPA treats a closed reindex stream as a completed reindex.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn open_stream_reports_a_dead_socket_as_unreachable() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let err = open_stream(
            &tmp.path().join("absent.sock"),
            METHOD_STATUS_STREAM,
            json!({}),
            STREAM_OPEN_TIMEOUT,
        )
        .await
        .expect_err("a dead socket cannot open a stream");
        assert!(matches!(err, SearchRpcError::Unreachable(_)), "{err:?}");
    }

    /// Why: [`MAX_FRAME_BYTES`] is a floor under the listener's own figure, and
    /// nothing in a `cargo check` couples the two — trusty-console does not
    /// depend on trusty-search, and adding a dev-dependency to compare the
    /// constants would build tantivy, tree-sitter and ORT into this crate's test
    /// run. Reading the declaration out of the listener's source keeps the two
    /// coupled at the cost of one file read.
    /// What: parses `pub const MAX_FRAME_BYTES` out of
    /// `crates/trusty-search/src/service/socket.rs` and asserts the direction.
    /// Fails when the declaration moves rather than passing on a missed match,
    /// which is what makes the check worth having.
    /// Test: this is the test.
    #[test]
    fn the_frame_budget_is_at_least_the_listeners() {
        let socket_rs = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../trusty-search/src/service/socket.rs");
        let source = std::fs::read_to_string(&socket_rs)
            .unwrap_or_else(|e| panic!("read {}: {e}", socket_rs.display()));

        let decl = source
            .lines()
            .find_map(|line| {
                line.trim()
                    .strip_prefix("pub const MAX_FRAME_BYTES: u64 = ")?
                    .strip_suffix(';')
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| {
                panic!(
                    "no `pub const MAX_FRAME_BYTES: u64 = …;` in {} — the listener's budget moved, \
                     so this floor is unverified",
                    socket_rs.display()
                )
            });

        // `64 * 1024 * 1024` — the only shape the declaration has ever taken.
        let listener: u64 = decl
            .split('*')
            .map(|factor| {
                factor
                    .trim()
                    .parse::<u64>()
                    .unwrap_or_else(|e| panic!("parse `{decl}`: {e}"))
            })
            .product();

        assert!(
            MAX_FRAME_BYTES >= listener,
            "this client's frame budget ({MAX_FRAME_BYTES}) is under the listener's ({listener}); \
             a response trusty-search already produced would come back as FrameTooLarge"
        );
    }

    /// Why: the console and the daemon must compute the SAME path, or the
    /// console dials a socket nothing is bound to and reports a running daemon
    /// as down. `trusty_search::service::socket::socket_path` is
    /// `daemon_socket_path("trusty-search")` and so is this.
    /// What: points `resolve_data_dir` at a tempdir — under the shared
    /// `detect::ENV_LOCK`, because that env var is process-global and the
    /// connector tests read it too — and asserts the whole resolved path.
    /// Test: this is the test.
    #[test]
    fn socket_path_matches_the_daemon_resolver() {
        let _guard = crate::detect::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::TempDir::new().expect("tempdir");
        unsafe {
            std::env::set_var(trusty_common::DATA_DIR_OVERRIDE_ENV, tmp.path());
        }
        let resolved = socket_path();
        unsafe {
            std::env::remove_var(trusty_common::DATA_DIR_OVERRIDE_ENV);
        }
        assert_eq!(
            resolved.expect("the override resolves"),
            tmp.path().join(SEARCH_SERVICE).join("trusty-search.sock")
        );
    }
}
