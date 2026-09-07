//! trusty-console's one client to trusty-analyze's `analyze.*` surface, over the
//! daemon's Unix socket (#6155).
//!
//! Why: #6287 moved trusty-analyze onto a hardened Unix socket and deleted its
//! HTTP listener, its `/ui` mount, its flat `/health` and `/indexes` routes and
//! its `/sse` broadcast with it (ADR-0032). The dashboard those routes served
//! was left orphaned in the tree — a committed bundle with no Rust code
//! referencing it at all. This module is what lets the console serve it instead:
//! every path the SPA calls becomes one `analyze.*` JSON-RPC call here.
//!
//! What: the method names this crate dials, the socket-path resolution every
//! caller shares, one unary exchange, and the mapping from a JSON-RPC refusal
//! back to the HTTP status the same refusal used to carry.
//!
//! ## Read-only, and there is no stream
//!
//! Every method below is one the dashboard reads or one fact write it makes.
//! No streaming arm exists because the daemon has no streaming method:
//! `trusty_analyze::service::rpc::METHODS` registers twenty methods and every
//! one is `typed`/`typed_liveness`. See `map`'s module docs.
//!
//! **There is no HTTP fallback.** trusty-analyze writes no discovery file since
//! #6287, and a fallback resolving `127.0.0.1:7879` would report whatever now
//! holds that port as a healthy trusty-analyze. A daemon with no socket reads as
//! unreachable, which is what it is — the same reason `proxy::routes::full_id`
//! has no `analyze` row.
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

// #6155: one name for this daemon in this crate, beside `SEARCH_SERVICE_ID` and
// `MEMORY_SERVICE`. `search_uds` minted a second `SEARCH_SERVICE` of its own;
// that duplication is not repeated here, for the reason `memory_uds` records.
pub(crate) use crate::routes::ANALYZE_SERVICE;

// ─── the method names ────────────────────────────────────────────────────────
//
// Duplicated as literals rather than imported: trusty-console has no Cargo edge
// on trusty-analyze and adding one would pull fifteen tree-sitter grammars, a
// redb fact store and a SCIP ingester into the console's build.
// `trusty_analyze::service::rpc`'s `METHODS` is the definition; these are the
// client's copy, and
// `every_analyze_method_this_bridge_dials_is_declared_by_the_daemon` in
// `tests.rs` is what keeps them equal.

/// Liveness, dependency reachability and the daemon's version — `GET /health`.
pub(crate) const METHOD_HEALTH: &str = "analyze.health";
/// The index roster trusty-search knows about — `GET /indexes`.
pub(crate) const METHOD_LIST_INDEXES: &str = "analyze.list_indexes";
/// The worst-complexity chunks — `GET /indexes/{id}/complexity_hotspots`.
pub(crate) const METHOD_COMPLEXITY_HOTSPOTS: &str = "analyze.complexity_hotspots";
/// Matched code smells, paged — `GET /indexes/{id}/smells`.
pub(crate) const METHOD_SMELLS: &str = "analyze.smells";
/// One index's aggregate quality report — `GET /indexes/{id}/quality`.
pub(crate) const METHOD_QUALITY: &str = "analyze.quality";
/// Ranked refactor suggestions — `GET /indexes/{id}/refactor-suggestions`.
pub(crate) const METHOD_REFACTOR_SUGGESTIONS: &str = "analyze.refactor_suggestions";
/// Concept clusters over an index's chunks — `GET /indexes/{id}/clusters`.
pub(crate) const METHOD_CLUSTERS: &str = "analyze.clusters";
/// Facts matching a subject/predicate filter — `GET /facts`.
pub(crate) const METHOD_FACTS_LIST: &str = "analyze.facts_list";
/// Write one fact — `POST /facts`.
pub(crate) const METHOD_FACTS_UPSERT: &str = "analyze.facts_upsert";
/// Remove one fact by id — `DELETE /facts/{id}`.
pub(crate) const METHOD_FACTS_DELETE: &str = "analyze.facts_delete";

/// How long one unary exchange may take, end to end.
///
/// Why not the 30 s `routes::ACTION_TIMEOUT` the console's other daemon clients
/// use: those bound one operator action against a store. These handlers do
/// ANALYSIS. `analyze.clusters` pulls every chunk in an index out of
/// trusty-search and runs k-means over them; `analyze.smells` and
/// `analyze.complexity_hotspots` walk the same corpus. The daemon's own
/// `serve_options` deliberately caps the socket READ and not the handler, for
/// exactly this reason (`service::rpc`'s `shutdown_drain` note names a
/// multi-minute `analyze.review`), so a 30 s client budget refuses work the
/// daemon was still doing.
///
/// Measured 2026-09-07 against the `trusty-tools` index on this workspace:
/// `GET /api/analyze/indexes/trusty-tools/clusters?k=2&method=bow` answered
/// `502` under a 30 s budget and `200` in 38.8 s under this one.
///
/// What: 120 s. Not unbounded — a browser tab must not hang forever, and a
/// handler past this still reaches the operator as a `502` naming the method
/// rather than as an empty success.
/// Test: `a_dead_socket_is_a_bad_gateway_not_an_empty_success` covers the
/// refusal shape; the budget itself is an operational figure, not an asserted
/// one.
pub(crate) const CALL_TIMEOUT: Duration = Duration::from_secs(120);

/// The frame budget this client applies — at least the listener's.
///
/// Why not [`trusty_common::uds::MAX_FRAME_BYTES`] (8 MiB):
/// `trusty_analyze::service::rpc`'s own `MAX_FRAME_BYTES` is 32 MiB, and that
/// figure applies to the RESPONSE read here too — so the shared default would
/// fail an `analyze.smells` or `analyze.clusters` answer the daemon had already
/// produced. The invariant is a FLOOR, not an equality: smaller breaks a
/// response that exists; larger has no failure mode, because the listener
/// refuses an oversized REQUEST on its own terms.
/// Test: `the_frame_budget_is_at_least_the_listeners`.
pub(crate) const MAX_FRAME_BYTES: u64 = 32 * 1024 * 1024;

// ─── the codes trusty-analyze's refusals carry ───────────────────────────────
//
// `crates/trusty-analyze/src/service/events.rs`'s `From<ApiError> for RpcError`
// maps each failure kind onto one of these; [`AnalyzeRpcError::status`] maps it
// back, so a refusal the SPA used to read as `404 Not Found` still reads as one
// through this bridge.

/// HTTP 404 — `events::CODE_NOT_FOUND`, which `ApiErrorKind::NotFound` carries.
const CODE_NOT_FOUND: i64 = -32004;
/// HTTP 504 — `events::CODE_DEADLINE_EXCEEDED`. `analyze.diagnostics` and
/// `analyze.deep_analysis` report a handler cutoff with it, and #6034/#6041
/// exist because that is not the same failure as a broken daemon.
const CODE_DEADLINE_EXCEEDED: i64 = -32005;

/// Why one exchange with trusty-analyze did not produce an answer.
///
/// Why the four arms are separate: the dashboard must not render "the daemon is
/// not running" and "the daemon said no" the same way, and neither may render as
/// an empty success — the fail-open branch this module exists to close.
/// Test: `error_status_maps_every_documented_code`, and the `call_reports_*`
/// tests below.
#[derive(Debug)]
pub(crate) enum AnalyzeRpcError {
    /// The socket path itself could not be resolved — an unusable data
    /// directory, not a daemon that is down.
    Unresolved(String),
    /// Nothing answered on the socket, or the exchange failed in transport.
    Unreachable(String),
    /// The daemon answered with a JSON-RPC `error` frame.
    Refused {
        /// The daemon's own code.
        code: i64,
        /// The daemon's own words.
        message: String,
    },
    /// The daemon answered, but with neither a result nor an error.
    Malformed(String),
}

impl AnalyzeRpcError {
    /// The HTTP status this failure surfaces as.
    ///
    /// Why: the SPA branches on status (`api.js`'s `request` throws on non-2xx
    /// carrying the status text), so a refusal has to arrive as the status the
    /// HTTP route sent for the same condition. An unmapped code is `500` rather
    /// than `200` with an error body — no failure arm here may reach the browser
    /// as a success.
    /// Test: `error_status_maps_every_documented_code`.
    pub(crate) fn status(&self) -> StatusCode {
        match self {
            Self::Unresolved(_) | Self::Unreachable(_) | Self::Malformed(_) => {
                StatusCode::BAD_GATEWAY
            }
            Self::Refused { code, .. } => match *code {
                CODE_NOT_FOUND => StatusCode::NOT_FOUND,
                CODE_DEADLINE_EXCEEDED => StatusCode::GATEWAY_TIMEOUT,
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

impl IntoResponse for AnalyzeRpcError {
    /// Render the failure as the JSON body the SPA reads.
    ///
    /// `api.js` reads a non-2xx body as text and puts it in the thrown error's
    /// message, so the daemon's wording reaches the operator whatever the status
    /// was.
    /// Test: `a_dead_socket_is_a_bad_gateway_not_an_empty_success` in
    /// `tests/analyze_uds_bridge.rs`.
    fn into_response(self) -> Response {
        let status = self.status();
        let body = json!({
            "error": self.message(),
            "service": ANALYZE_SERVICE,
        });
        (status, axum::Json(body)).into_response()
    }
}

/// Where trusty-analyze's socket is, or why the console could not work it out.
///
/// Why the error is carried rather than discarded: an unresolvable data
/// directory is operator-fixable (permissions, a `TRUSTY_DATA_DIR_OVERRIDE`
/// pointing somewhere unusable) and is indistinguishable on the dashboard from a
/// daemon that is simply not running.
/// What: `trusty_common::daemon_socket_path`, the ONE resolver the daemon itself
/// calls (`trusty_analyze::service::rpc::socket_path`), so there is no second
/// answer to where the socket is — and the same one `detect::AnalyzeConnector`
/// already uses.
/// Test: `socket_path_matches_the_daemon_resolver`.
pub(crate) fn socket_path() -> Result<PathBuf, String> {
    trusty_common::daemon_socket_path(ANALYZE_SERVICE)
        .map_err(|e| format!("could not resolve the {ANALYZE_SERVICE} socket path: {e:#}"))
}

/// One unary JSON-RPC exchange with trusty-analyze.
///
/// Why here rather than at each call site: the envelope, the framing, the
/// timeout and the two ways an exchange fails before a handler runs are
/// identical for every method, and a second copy of them is how one caller
/// starts reading an `error` frame as a success while another does not.
///
/// What: one framed request, then the envelope check. A response carrying
/// `error` is [`AnalyzeRpcError::Refused`] with the daemon's code and message; a
/// response carrying neither half is [`AnalyzeRpcError::Malformed`], never an
/// empty success.
///
/// # Errors
///
/// Every arm of [`AnalyzeRpcError`].
///
/// Test: `call_reports_a_dead_socket_as_unreachable`,
/// `call_reports_a_jsonrpc_error_with_the_http_status_it_came_from`,
/// `call_reports_an_empty_answer_as_malformed`, `call_returns_the_daemon_result`.
pub(crate) async fn call(
    socket: &Path,
    method: &str,
    params: Value,
    timeout: Duration,
) -> Result<Value, AnalyzeRpcError> {
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
                AnalyzeRpcError::Unreachable(format!(
                    "{ANALYZE_SERVICE} did not answer {method}: {e}"
                ))
            })?;

    if let Some(error) = response.error {
        return Err(AnalyzeRpcError::Refused {
            code: error.code,
            message: error.message,
        });
    }

    response.result.ok_or_else(|| {
        AnalyzeRpcError::Malformed(format!(
            "{ANALYZE_SERVICE} answered {method} with neither a result nor an error"
        ))
    })
}

/// Render a `serde_json::Value` as the JSON body an HTTP route would have sent.
///
/// Why not `axum::Json`: the daemon's handlers already answer the exact document
/// the axum route serialised, so re-encoding through a typed wrapper would be a
/// second chance to differ from it.
/// Test: `unary_route_returns_the_daemon_body_verbatim` in
/// `tests/analyze_uds_bridge.rs`.
pub(crate) fn json_response(value: &Value) -> Response {
    match serde_json::to_vec(value) {
        Ok(bytes) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(bytes))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
        Err(e) => AnalyzeRpcError::Malformed(format!(
            "{ANALYZE_SERVICE} answered a body the console could not re-encode: {e}"
        ))
        .into_response(),
    }
}

#[cfg(test)]
mod tests;
