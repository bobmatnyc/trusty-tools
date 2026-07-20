//! HTTP JSON-RPC 2.0 transport for `tcode serve --http` (#2053, #2054).
//!
//! Why: matches how `trusty-memory` (`web::router` + `run_http_on`) and
//! `trusty-search` (`service::server::build_router`) stand up their axum
//! daemons: a pure `axum::Router` builder — testable via `oneshot`, no real
//! socket — wrapped in `trusty_common::server::with_standard_middleware`
//! (CORS/trace/gzip), plus a bind+serve entry point that installs the
//! shared `trusty_common::shutdown_signal()` graceful-shutdown watcher
//! (SIGTERM + SIGINT), exactly like `trusty-memory`'s `run_http_on`. `POST
//! /rpc` and `GET /health` dispatch through the SAME [`crate::jsonrpc::Router`]
//! the STDIO transport (`crate::serve::transport`) uses via
//! [`Router::dispatch_json`] — there is exactly one implementation of each
//! method (`ping`, `health`, `session.*`, …), not one per transport.
//!
//! #2054 adds `GET /sessions/{id}/events`: an SSE route that is HTTP's real
//! live-streaming mechanism for `session.attach`. HTTP `POST /rpc` is
//! fundamentally one request/one response, so `session.attach` over HTTP
//! acknowledges immediately (replay burst + this endpoint's URL in
//! `stream_url`) rather than holding the connection open — a browser or CLI
//! client then opens this endpoint to receive the ring-buffer replay
//! followed by live events, and simply closing that connection IS "detach"
//! for HTTP (there is nothing further to call). See
//! `crate::jsonrpc::ConnectionContext`'s docs and `session::protocol::attach`
//! for the full STDIO-vs-HTTP rationale. Multi-client attach falls out for
//! free here: every `GET` gets its own `crate::events::subscribe()`
//! receiver.
//!
//! What: [`build_axum_router`] (pure, unit-testable) and [`run_http`] (binds
//! a `TcpListener` — port `0` yields an OS-assigned ephemeral port — logs
//! the bound address to stderr, never stdout, and serves until graceful
//! shutdown).
//!
//! Deferred: batch JSON-RPC requests (a JSON array body) are not supported —
//! `POST /rpc` accepts a single request object per call, matching what the
//! ticket asked for ("single-request is fine for now").
//!
//! Test: `http::tests::*` drive [`build_axum_router`] via
//! `tower::util::ServiceExt::oneshot` (no real socket): `POST /rpc` ping
//! success, `POST /rpc` malformed JSON -> `-32700`, `GET /health`,
//! `session.create` + `GET /sessions/{id}/events` SSE replay + live event.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::{
    Json, Router as AxumRouter,
    body::Bytes,
    extract::{Path, State},
    response::sse::{Event as SseEvent, KeepAlive, Sse},
    routing::{get, post},
};
use futures_util::{Stream, StreamExt, stream};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_stream::wrappers::BroadcastStream;
use tracing::info;
use trusty_common::mcp::Response;

use crate::jsonrpc::{ConnectionContext, Router};
use crate::serve::methods::health_payload;
use crate::serve::rest;
use crate::session::SessionRegistry;
use crate::workstreams::SharedWorkstreamStore;

/// Shared axum state: the JSON-RPC router (for `POST /rpc`) and the session
/// registry (for the SSE route, which reads it directly rather than going
/// through the router).
///
/// Why: both fields are `Arc` so cloning per-request (axum requires `State`
/// to be `Clone`) is cheap and every route sees the same daemon-wide
/// instances.
#[derive(Clone)]
struct HttpState {
    router: Arc<Router>,
    sessions: Arc<SessionRegistry>,
}

/// Build the pure axum router (no listener bound) exposing `POST /rpc`,
/// `GET /health`, `GET /sessions/{id}/events`, and (#2983 Slice 2 + Slice 3)
/// the full `/sessions*` REST resource group.
///
/// Why: kept separate from [`run_http`] so unit tests exercise routing and
/// dispatch via `tower::util::ServiceExt::oneshot` without opening a real
/// socket, and so the standard CORS/trace/gzip middleware stack wraps it
/// identically to trusty-memory/trusty-search.
/// What: `POST /rpc` dispatches the request body through
/// `Router::dispatch_json` (shared with the STDIO transport); `GET /health`
/// returns [`health_payload`]; `GET /sessions/{id}/events` streams that
/// session's ring-buffer replay then live events as SSE; `crate::serve::rest`
/// merges in the `session.*` REST read routes (`GET /sessions`,
/// `GET /sessions/{id}`, `.../transcript`, `.../readiness`, `.../goals`,
/// (issue #3015) `.../budget`),
/// (Slice 3) the write routes (`POST /sessions`,
/// `POST /sessions/{id}/messages`, `POST /sessions/{id}/cancel`,
/// `PUT`/`DELETE /sessions/{id}/goal`), (Slice 4) `POST /tasks`, (Slice 5)
/// `GET /fs`, (Slice 6) `GET /sessions/{id}/agents`, (Slice 7, issue #3072)
/// `GET /sessions/{id}/search-audit`, and the full `workstream.*` REST
/// surface: `POST /workstreams`, `GET /workstreams/{id}`, `GET /workstreams`,
/// `POST /workstreams/{id}/close` (issue #3295) plus
/// `POST /workstreams/{id}/activate`/`.../deactivate` (DOC-48 §5.2, issue
/// #3294) — plus (issue #3297) `GET /workstreams/{id}/events` (the
/// workstream-level SSE aggregation route, `crate::workstreams::sse::routes`)
/// — plus (issue #3449) `GET`/`POST /agents`, `DELETE /agents/{name}` and the
/// `skills` twin (the Foundry GUI's Agents/Skills management tabs,
/// `rest::agent_catalog`/`rest::skill_catalog`) — none of which collide with
/// the paths registered directly on this router or with each other.
/// Test: `http_rpc_ping_returns_pong`,
/// `http_rpc_malformed_json_returns_parse_error`,
/// `http_health_matches_jsonrpc_health_payload`,
/// `http_session_events_sse_streams_replay_then_live_event`,
/// `http_rest_sessions_route_is_merged_in`,
/// `http_rest_post_sessions_route_is_merged_in`,
/// `http_rest_post_tasks_route_is_merged_in`,
/// `http_rest_get_fs_route_is_merged_in`,
/// `http_rest_get_session_agents_route_is_merged_in`,
/// `http_rest_get_session_budget_route_is_merged_in`,
/// `http_rest_get_session_search_audit_route_is_merged_in`,
/// `http_rest_get_workstreams_route_is_merged_in` (also pins the
/// `GET /workstreams/{id}/events` merge),
/// `http_rest_get_projects_route_is_merged_in`,
/// `http_rest_get_agent_and_skill_catalog_routes_are_merged_in`.
pub fn build_axum_router(
    router: Arc<Router>,
    sessions: Arc<SessionRegistry>,
    workstreams: SharedWorkstreamStore,
) -> AxumRouter {
    let state = HttpState {
        router: router.clone(),
        sessions,
    };
    let core = AxumRouter::new()
        .route("/rpc", post(rpc_handler))
        .route("/health", get(health_handler))
        .route("/sessions/{id}/events", get(session_events_sse))
        .with_state(state);
    let app = core
        .merge(rest::sessions::routes(router.clone()))
        .merge(rest::sessions_write::routes(router.clone()))
        .merge(rest::tasks::routes(router.clone()))
        .merge(rest::fs::routes(router.clone()))
        .merge(rest::projects::routes(router.clone()))
        .merge(rest::agents::routes(router.clone()))
        .merge(rest::agent_catalog::routes(router.clone()))
        .merge(rest::skill_catalog::routes(router.clone()))
        .merge(rest::search_audit::routes(router.clone()))
        .merge(rest::workstreams::routes(router))
        .merge(crate::workstreams::sse::routes(workstreams));
    trusty_common::server::with_standard_middleware(app)
}

/// `POST /rpc` — JSON-RPC 2.0 dispatch endpoint.
///
/// Why: lets browser clients, curl, and any HTTP-only caller reach the same
/// method surface the STDIO transport exposes, without learning a REST
/// vocabulary per method.
/// What: builds a throwaway [`ConnectionContext`] for this one call (its
/// `notify` receiver is dropped once this function returns, so any handler
/// that queued a notification via it — e.g. `session.attach`'s forwarder —
/// simply stops on its next send; the real HTTP live-streaming path is
/// `GET /sessions/{id}/events`, not this channel), then dispatches the raw
/// request body through `Router::dispatch_json` (single request per call —
/// batch/array bodies are not supported, see module docs). Always returns
/// HTTP 200; JSON-RPC errors, including malformed JSON (mapped to
/// `-32700`), are carried in the response envelope's `error` field rather
/// than the HTTP status.
/// Test: `http_rpc_ping_returns_pong`, `http_rpc_malformed_json_returns_parse_error`.
async fn rpc_handler(State(state): State<HttpState>, body: Bytes) -> Json<Response> {
    let (notify_tx, _notify_rx) = mpsc::unbounded_channel();
    let ctx = ConnectionContext::new(notify_tx);
    Json(state.router.dispatch_json(&body, &ctx).await)
}

/// `GET /health` — liveness probe matching the `health` JSON-RPC method.
///
/// Why: the ecosystem convention (trusty-search, trusty-memory) is an
/// unauthenticated `GET /health`; trusty-console's gateway polls it to
/// confirm a daemon is alive. Reusing [`health_payload`] — the exact
/// function the `health` JSON-RPC method calls — keeps the two transports
/// from drifting into two different health shapes.
/// What: always returns HTTP 200 with `{"server","version","status"}`.
/// Test: `http_health_matches_jsonrpc_health_payload`.
async fn health_handler() -> Json<serde_json::Value> {
    Json(health_payload())
}

/// `GET /sessions/{id}/events` — Server-Sent Events stream of a session's
/// ring-buffer replay followed by its live events.
///
/// Why: HTTP's real live-streaming mechanism for `session.attach` (see
/// module docs for why `POST /rpc` itself can't hold the stream open).
/// Multiple concurrent `GET`s against the same session id each get their
/// own subscription (multi-client attach, vision spec Axiom 4) — no extra
/// bookkeeping needed because `crate::events::subscribe()` already hands
/// out an independent `broadcast::Receiver` per call.
/// What: 404 with a JSON-RPC error envelope if the session id is unknown
/// (`SessionRegistry::replay` -> `session_not_found`, spec-mapped HTTP
/// status per §13.2). Otherwise: an SSE stream that first emits the
/// ring-buffer replay (oldest-first) then chains the live bus, filtered to
/// this `session_id`, forever (until the client disconnects — which is
/// "detach" for HTTP).
/// Test: `http_session_events_sse_streams_replay_then_live_event`,
/// `http_session_events_sse_unknown_session_returns_404`.
async fn session_events_sse(
    State(state): State<HttpState>,
    Path(session_id): Path<String>,
) -> Result<
    Sse<impl Stream<Item = Result<SseEvent, Infallible>>>,
    (axum::http::StatusCode, Json<Response>),
> {
    let replay = state.sessions.replay(&session_id).map_err(|e| {
        let body = Response::err(None, e.code, e.message);
        (axum::http::StatusCode::NOT_FOUND, Json(body))
    })?;

    let replay_stream = stream::iter(
        replay
            .into_iter()
            .map(|envelope| Ok(sse_event_for(&envelope))),
    );

    let filter_id = session_id.clone();
    let live_stream = BroadcastStream::new(crate::events::subscribe()).filter_map(move |item| {
        let filter_id = filter_id.clone();
        async move {
            match item {
                Ok(envelope) if envelope.session_id == filter_id => {
                    Some(Ok(sse_event_for(&envelope)))
                }
                _ => None,
            }
        }
    });

    Ok(Sse::new(replay_stream.chain(live_stream)).keep_alive(KeepAlive::default()))
}

/// Serialise one `crate::events::SessionEventEnvelope` as an SSE `data:`
/// frame.
///
/// Why: centralises the (practically infallible — the envelope has no
/// untagged maps or non-string keys) JSON encoding so `session_events_sse`
/// doesn't repeat the fallback logic at two call sites. Sending the full
/// envelope (not just the bare `event`) means the HTTP SSE transport carries
/// exactly the same `session_id`/`seq`/`at`/`kind`/`event` shape the STDIO
/// `session.event` notification does (#2055 requirement: both transports
/// carry the full taxonomy with the envelope).
/// What: `SseEvent::default().json_data(envelope)`, falling back to an empty
/// JSON object on the (unreachable in practice) serialisation error rather
/// than `unwrap`ing.
fn sse_event_for(envelope: &crate::events::SessionEventEnvelope) -> SseEvent {
    SseEvent::default()
        .json_data(envelope)
        .unwrap_or_else(|_| SseEvent::default().data("{}"))
}

/// Bind a `TcpListener` and serve `POST /rpc` + `GET /health` +
/// `GET /sessions/{id}/events` until SIGTERM/SIGINT (graceful) or an
/// internal axum error.
///
/// Why: the top-level entry point `crate::serve::run_http` delegates to this
/// once the router and session registry are assembled.
/// What: binds `127.0.0.1:port` (`port = 0` lets the OS assign an ephemeral
/// port), logs the real bound address to stderr — mirroring
/// `trusty-memory`'s `run_http_on`, and never touching stdout — then serves
/// with `trusty_common::shutdown_signal()` installed via
/// `with_graceful_shutdown`: SIGTERM/SIGINT stop new connections and drain
/// in-flight requests before this function returns (issue #534 convention).
/// Test: not directly unit-tested (would require a real socket + a real
/// signal); [`build_axum_router`]'s tests cover the routing/dispatch logic
/// this serves.
pub async fn run_http(
    router: Router,
    sessions: Arc<SessionRegistry>,
    workstreams: SharedWorkstreamStore,
    port: u16,
) -> Result<()> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("tcode serve --http: bind {addr}"))?;
    let bound = listener
        .local_addr()
        .context("tcode serve --http: read bound local address")?;
    info!("tcode serve --http: listening on http://{bound}");
    eprintln!("tcode serve --http: listening on http://{bound}");

    // Issue #3415 (DOC-50 §3.4): write the discovery file `CodeEngine` reads
    // to find this daemon without a hardcoded port. Best-effort — a write
    // failure only means discovery degrades to the `TCODE_DAEMON_URL` env
    // var; it must never block the daemon from serving.
    let discovery_path = crate::serve::discovery::http_addr_path();
    if let Some(path) = &discovery_path
        && let Err(e) = crate::serve::discovery::write_http_addr_file(path, &bound)
    {
        tracing::warn!(
            error = %e,
            path = %path.display(),
            "tcode serve --http: failed to write daemon discovery file"
        );
    }

    let app = build_axum_router(Arc::new(router), sessions, workstreams);
    axum::serve(listener, app)
        .with_graceful_shutdown(trusty_common::shutdown_signal())
        .await
        .context("tcode serve --http: axum serve failed")?;

    if let Some(path) = &discovery_path {
        crate::serve::discovery::remove_http_addr_file(path);
    }
    info!("tcode serve --http: stopped");
    Ok(())
}

#[cfg(test)]
#[path = "http_tests.rs"]
mod tests;
