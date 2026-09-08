//! trusty-console's one client to trusty-memory's `memory.*` surface, over the
//! daemon's Unix socket (#6155).
//!
//! Why: #6286 moved trusty-memory onto a hardened Unix socket and deleted its
//! HTTP listener, its `/api/v1/*` routes and its `/sse` broadcast with it
//! (ADR-0032). The dashboard those routes served kept working only because
//! nothing served it at all — the bundle stayed embedded in a binary with no
//! listener. This module is what lets the console serve it instead: every path
//! the SPA calls becomes one `memory.*` JSON-RPC call here.
//!
//! What: the method names this crate dials, the socket-path resolution every
//! caller shares, one unary exchange, one stream open, and the mapping from a
//! JSON-RPC refusal back to the HTTP status the same refusal used to carry.
//!
//! ## This is not the console's only client to trusty-memory
//!
//! [`crate::routes::memory_rpc`] owns the OTHER one: an MCP `tools/call`
//! envelope, because `palace_delete` and `palace_compact` are tools the
//! dispatcher routes rather than folded methods, and a bare method name answers
//! `method_not_found` for them. The two are not duplicates — they speak
//! different envelopes to different halves of the same daemon — and the palace
//! management actions stay with `memory_rpc`, which already owns that domain.
//! One row in the `map` table DOES reach the tool dispatcher by raw name
//! (`kg_query`),
//! which the dispatcher permits for anything in its `TOOL_METHODS` list; that is
//! a raw-name call, not a second `tools/call` implementation.
//!
//! **There is no HTTP fallback.** trusty-memory writes no discovery file since
//! #6286, and a fallback resolving `127.0.0.1:7070` would report whatever now
//! holds that port as a healthy trusty-memory. A daemon with no socket reads as
//! unreachable, which is what it is.
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

// #6155: one name for this daemon in this crate. `search_uds` minted its own
// `SEARCH_SERVICE` beside the pre-existing `routes::SEARCH_SERVICE_ID`; that
// duplication is not repeated here — `routes::MEMORY_SERVICE` is the one
// definition and this module imports it.
pub(crate) use crate::routes::MEMORY_SERVICE;

// ─── the method names ────────────────────────────────────────────────────────
//
// Duplicated as literals rather than imported: trusty-console has no Cargo edge
// on trusty-memory and adding one would pull a whole vector store, embedder and
// knowledge graph into the console's build. `trusty_memory::transport::uds`'s
// `FOLDED_METHODS` is the definition; these are the client's copy, and
// `every_memory_method_this_bridge_dials_is_declared_by_the_daemon` in
// `tests.rs` is
// what keeps them equal.

/// Liveness, resource metrics and the daemon's version.
pub(crate) const METHOD_HEALTH: &str = "memory.health";
/// Palace/drawer/vector/triple counts — `GET /api/v1/status`.
pub(crate) const METHOD_STATUS: &str = "memory.status";
/// Provider, model and data root — `GET /api/v1/config`.
pub(crate) const METHOD_CONFIG: &str = "memory.config";
/// The palace roster — `GET /api/v1/palaces`.
pub(crate) const METHOD_PALACES_LIST: &str = "memory.palaces_list";
/// One palace's detail — `GET /api/v1/palaces/{id}`.
pub(crate) const METHOD_PALACE_GET: &str = "memory.palace_get";
/// Drawers in one palace — `GET /api/v1/palaces/{id}/drawers`.
pub(crate) const METHOD_DRAWERS_LIST: &str = "memory.drawers_list";
/// The in-memory log ring — `GET /api/v1/logs/tail`.
pub(crate) const METHOD_LOGS_TAIL: &str = "memory.logs_tail";
/// Aggregate dream-cycle stats — `GET /api/v1/dream/status`.
pub(crate) const METHOD_DREAM_STATUS: &str = "memory.dream_status";
/// Run a dream cycle across every palace — `POST /api/v1/dream/run`.
pub(crate) const METHOD_DREAM_RUN: &str = "memory.dream_run";
/// Ask the daemon to shut down — `POST /api/v1/admin/stop`.
pub(crate) const METHOD_ADMIN_STOP: &str = "memory.admin_stop";
/// Distinct KG subjects with their triple counts —
/// `GET /api/v1/palaces/{id}/kg/subjects_with_counts`.
pub(crate) const METHOD_KG_SUBJECTS_WITH_COUNTS: &str = "memory.kg_subjects_with_counts";
/// Every active triple, paged — `GET /api/v1/palaces/{id}/kg/all`.
pub(crate) const METHOD_KG_ALL: &str = "memory.kg_all";
/// How many triples are active — `GET /api/v1/palaces/{id}/kg/count`.
pub(crate) const METHOD_KG_COUNT: &str = "memory.kg_count";
/// The whole active graph, capped — `GET /api/v1/palaces/{id}/kg/graph`.
pub(crate) const METHOD_KG_GRAPH: &str = "memory.kg_graph";
/// The top-degree seed — `GET /api/v1/palaces/{id}/kg/graph/seed`.
pub(crate) const METHOD_KG_GRAPH_SEED: &str = "memory.kg_graph_seed";
/// Bounded expansion around one node —
/// `GET /api/v1/palaces/{id}/kg/graph/neighbors`.
pub(crate) const METHOD_KG_GRAPH_NEIGHBORS: &str = "memory.kg_graph_neighbors";
/// The persistent activity log — `GET /api/v1/activity`.
pub(crate) const METHOD_ACTIVITY: &str = "memory.activity";
/// Live daemon events — what `/sse` was, as a stream.
pub(crate) const METHOD_ACTIVITY_STREAM: &str = "memory.activity_stream";

/// Active triples for one subject — `GET /api/v1/palaces/{id}/kg?subject=…`.
///
/// Why this one has no `memory.` prefix: #6286 folded the axum routes that had
/// no tool equivalent, and this one HAD a tool equivalent already, so it was not
/// folded. `trusty_memory::transport::rpc`'s `TOOL_METHODS` forwards a raw
/// `kg_query` straight into the tool dispatcher, which is the only way to this
/// read without adding a daemon-side method. Its answer is the tool's
/// `{subject, triples, kg_triple_count}` rather than the bare array the retired
/// route returned; `ui-memory/src/lib/api.js` unwraps `triples`, because this
/// table maps NAMES and leaves payloads alone.
/// Test: `maps_every_endpoint_the_spa_calls`.
pub(crate) const METHOD_KG_QUERY_TOOL: &str = "kg_query";

/// How long one unary exchange may take, end to end.
///
/// Matches `routes::ACTION_TIMEOUT`, which the console's other trusty-memory
/// client already uses: `memory.dream_run` walks every palace's vector index and
/// `memory.kg_graph` reads a whole palace's triple set, so this is bounded by
/// disk work rather than by a round trip.
pub(crate) const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// How long opening a stream may take before the console gives up on it.
///
/// Why separate from the per-frame budget: `memory.activity_stream` emits only
/// when something happens, so an idle daemon legitimately sends nothing for
/// minutes and the frame budget has to be effectively unbounded. A socket whose
/// backlog is full, on the other hand, accepts the connect and then never
/// reads — so the OPEN gets its own, short, budget, and the browser is told the
/// daemon is unreachable instead of waiting out the frame budget for a response
/// head. An ABSENT socket is not this case; it fails immediately with `ENOENT`.
/// Test: `open_stream_reports_a_dead_socket_as_unreachable`.
pub(crate) const STREAM_OPEN_TIMEOUT: Duration = Duration::from_secs(60);

/// How long the bridge waits for a refusal before committing to `200`.
///
/// Why this is NOT [`STREAM_OPEN_TIMEOUT`]: `memory.activity_stream` sends no
/// opener. It subscribes to the daemon's event broadcast and forwards what
/// arrives, so an idle daemon emits its first frame whenever something next
/// happens — minutes, or never. Peeking that frame under the open budget held
/// the response HEAD for up to a minute against a healthy daemon, and the
/// browser showed a feed that never connected; the retired `/sse` route wrote
/// its head immediately. Observed live on 2026-09-07 against the running daemon:
/// `curl /api/memory/sse` returned no status inside two seconds.
///
/// What: the peek gets its own short budget, and a peek that expires commits to
/// `200` with no leading item rather than failing. A refusal is produced by the
/// handler before any event and arrives at once, so this window separates the
/// two cases that actually occur. A refusal slower than this window is not lost:
/// it arrives on the open stream as the `{"type":"error"}` event
/// `trusty_common::uds::sse` writes for a terminal frame, which the feed renders
/// rather than swallowing.
/// Test: `an_idle_stream_still_gets_its_response_head`.
pub(crate) const STREAM_FIRST_FRAME_PEEK: Duration = Duration::from_secs(2);

/// The per-frame budget on an open stream — effectively none.
///
/// Why so large: the shared helper applies this to EACH frame read, and
/// `memory.activity_stream` is event-driven. A quiet daemon emits nothing for as
/// long as nothing happens, and the SPA reads a closed stream as "the feed
/// ended" (`ui-memory/src/lib/components/ActivityFeed.svelte`), so a budget short
/// enough to cut an idle period would report a live feed as gone.
/// What: a day. Not `Duration::MAX`, which overflows tokio's timer when it is
/// added to `Instant::now()`.
pub(crate) const STREAM_FRAME_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

/// The frame budget this client applies — at least the listener's.
///
/// Why not [`trusty_common::uds::MAX_FRAME_BYTES`] (8 MiB):
/// `trusty_memory::transport::uds`'s own `MAX_FRAME_BYTES` is 32 MiB, and that
/// figure applies to the RESPONSE read here too — so the shared default would
/// fail a `memory.kg_graph` or `memory.activity` answer the daemon had already
/// produced. The invariant is a FLOOR, not an equality: smaller breaks a
/// response that exists; larger has no failure mode, because the listener
/// refuses an oversized REQUEST on its own terms.
/// Test: `the_frame_budget_is_at_least_the_listeners`.
pub(crate) const MAX_FRAME_BYTES: u64 = 32 * 1024 * 1024;

// ─── the codes trusty-memory's refusals carry ────────────────────────────────
//
// `crates/trusty-memory/src/transport/api_error.rs` maps each failure kind onto
// one of these; [`MemoryRpcError::status`] maps it back, so a refusal the SPA
// used to read as `404 Not Found` still reads as one through this bridge.

/// HTTP 404 — `api_error::CODE_NOT_FOUND`.
const CODE_NOT_FOUND: i64 = -32004;
/// `api_error::CODE_REFUSED`, which carries BOTH of the kinds that used to be
/// 409 and 403. One code cannot be projected back onto two statuses, and 409 is
/// the one the SPA's palace flows actually branch on.
const CODE_REFUSED: i64 = -32006;

/// Why one exchange with trusty-memory did not produce an answer.
///
/// Why the four arms are separate: the dashboard must not render "the daemon is
/// not running" and "the daemon said no" the same way, and neither may render as
/// an empty success — the fail-open branch this module exists to close.
/// Test: `error_status_maps_every_documented_code`, and the `call_reports_*`
/// tests below.
#[derive(Debug)]
pub(crate) enum MemoryRpcError {
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

impl MemoryRpcError {
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
                CODE_REFUSED => StatusCode::CONFLICT,
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

impl IntoResponse for MemoryRpcError {
    /// Render the failure as the JSON body the SPA reads.
    ///
    /// `api.js` reads a non-2xx body as text and puts it in the thrown error's
    /// message, so the daemon's wording reaches the operator whatever the status
    /// was.
    /// Test: `a_dead_socket_is_a_bad_gateway_not_an_empty_success` in
    /// `tests/memory_uds_bridge.rs`.
    fn into_response(self) -> Response {
        let status = self.status();
        let body = json!({
            "error": self.message(),
            "service": MEMORY_SERVICE,
        });
        (status, axum::Json(body)).into_response()
    }
}

/// Where trusty-memory's socket is, or why the console could not work it out.
///
/// Why the error is carried rather than discarded: an unresolvable data
/// directory is operator-fixable (permissions, a `TRUSTY_DATA_DIR_OVERRIDE`
/// pointing somewhere unusable) and is indistinguishable on the dashboard from a
/// daemon that is simply not running.
/// What: `trusty_common::daemon_socket_path`, the ONE resolver the daemon itself
/// calls (`trusty_memory::transport::uds::socket_path`), so there is no second
/// answer to where the socket is — and the same one `routes::deletes` and
/// `routes::cleanup` already use.
/// Test: `socket_path_matches_the_daemon_resolver`.
pub(crate) fn socket_path() -> Result<PathBuf, String> {
    trusty_common::daemon_socket_path(MEMORY_SERVICE)
        .map_err(|e| format!("could not resolve the {MEMORY_SERVICE} socket path: {e:#}"))
}

/// One unary JSON-RPC exchange with trusty-memory.
///
/// Why here rather than at each call site: the envelope, the framing, the
/// timeout and the two ways an exchange fails before a handler runs are
/// identical for every method, and a second copy of them is how one caller
/// starts reading an `error` frame as a success while another does not.
///
/// What: one framed request, then the envelope check. A response carrying
/// `error` is [`MemoryRpcError::Refused`] with the daemon's code and message; a
/// response carrying neither half is [`MemoryRpcError::Malformed`], never an
/// empty success.
///
/// # Errors
///
/// Every arm of [`MemoryRpcError`].
///
/// Test: `call_reports_a_dead_socket_as_unreachable`,
/// `call_reports_a_jsonrpc_error_with_the_http_status_it_came_from`,
/// `call_reports_an_empty_answer_as_malformed`, `call_returns_the_daemon_result`.
pub(crate) async fn call(
    socket: &Path,
    method: &str,
    params: Value,
    timeout: Duration,
) -> Result<Value, MemoryRpcError> {
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
                MemoryRpcError::Unreachable(format!(
                    "{MEMORY_SERVICE} did not answer {method}: {e}"
                ))
            })?;

    if let Some(error) = response.error {
        return Err(MemoryRpcError::Refused {
            code: error.code,
            message: error.message,
        });
    }

    response.result.ok_or_else(|| {
        MemoryRpcError::Malformed(format!(
            "{MEMORY_SERVICE} answered {method} with neither a result nor an error"
        ))
    })
}

/// Open one streaming JSON-RPC exchange with trusty-memory.
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
/// `open_timeout` wraps the helper instead of being passed to it because the
/// helper takes ONE figure for the dial, the write and each frame read, and the
/// frame read needs [`STREAM_FRAME_TIMEOUT`]'s day.
///
/// # Errors
///
/// [`MemoryRpcError::Unreachable`] for a dial or write failure, and for an open
/// that outlasts `open_timeout`.
///
/// Test: `open_stream_reports_a_dead_socket_as_unreachable`.
pub(crate) async fn open_stream(
    socket: &Path,
    method: &str,
    params: Value,
    open_timeout: Duration,
) -> Result<FramedStream<Value>, MemoryRpcError> {
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
        MemoryRpcError::Unreachable(format!(
            "{MEMORY_SERVICE} did not open {method} within {}s",
            open_timeout.as_secs_f32()
        ))
    })?;

    opened.map_err(|e| {
        MemoryRpcError::Unreachable(format!("{MEMORY_SERVICE} did not open {method}: {e}"))
    })
}

/// Render a `serde_json::Value` as the JSON body an HTTP route would have sent.
///
/// Why not `axum::Json`: the daemon's handlers already answer the exact document
/// the axum route serialised, so re-encoding through a typed wrapper would be a
/// second chance to differ from it.
/// Test: `unary_route_returns_the_daemon_body_verbatim` in
/// `tests/memory_uds_bridge.rs`.
pub(crate) fn json_response(value: &Value) -> Response {
    match serde_json::to_vec(value) {
        Ok(bytes) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(bytes))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
        Err(e) => MemoryRpcError::Malformed(format!(
            "{MEMORY_SERVICE} answered a body the console could not re-encode: {e}"
        ))
        .into_response(),
    }
}

#[cfg(test)]
mod tests;
