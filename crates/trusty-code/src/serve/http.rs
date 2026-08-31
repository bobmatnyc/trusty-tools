//! HTTP JSON-RPC 2.0 transport for `tcode serve --http` (#2053, #2054).
//!
//! Why: matches how `trusty-memory` (`web::router` + `run_http_on`) and
//! `trusty-search` (`service::server::build_router`) stand up their axum
//! daemons: a pure `axum::Router` builder — testable via `oneshot`, no real
//! socket — wrapped in the shared guarded middleware stack
//! (`trusty_common::server::with_guarded_middleware_same_origin_cors`, #6003),
//! plus a bind+serve entry point that installs the
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
//! #5439 puts every route behind a credential. `trusty_common::daemon_token`
//! mints and stores it at `0600`; `trusty_common::server::bearer_auth` is the
//! router-wide guard, applied here to the FULLY-MERGED router so a route added
//! later cannot slip past it. [`PUBLIC_HEALTH_PATH`] is the single exemption
//! and discloses only liveness to an anonymous caller (#6472);
//! [`SSE_TICKET_PATH`] is how a browser `EventSource`, which cannot send a
//! header, carries an existing right onto a URL.
//!
//! Deferred: batch JSON-RPC requests (a JSON array body) are not supported —
//! `POST /rpc` accepts a single request object per call, matching what the
//! ticket asked for ("single-request is fine for now").
//!
//! Also deferred, and larger: this whole listener is scheduled for replacement
//! by a Unix socket fronted by trusty-console (ADR-0032, the #5439 owner
//! ruling of 2026-08-19), which authenticates by peer credential rather than
//! by a shared secret. This layer is the hardening that stands until then, not
//! the end state.
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
    Extension, Json, Router as AxumRouter,
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
use trusty_common::server::SelfOrigins;
use trusty_common::server::bearer_auth::{Authenticated, DaemonAuth, require_bearer};
use trusty_mcp::Response;

use crate::binding::ProjectBinding;
use crate::jsonrpc::{ConnectionContext, Router};
use crate::serve::methods::{health_payload, liveness_payload};
use crate::serve::rest;
use crate::session::SessionRegistry;
use crate::workstreams::SharedWorkstreamStore;

/// The app name whose data directory holds this daemon's credential file.
///
/// Why: server (`run_http`) and client (`crate::tui_client`) resolve the token
/// through the same constant, so they cannot end up reading two files.
pub const TOKEN_APP_NAME: &str = "trusty-code";

/// The one route that answers an unauthenticated caller (#6472).
///
/// Why: trusty-console's gateway polls a daemon for liveness and holds no
/// credential, and the ecosystem convention (trusty-search, trusty-memory) is
/// an open `GET /health`. Keeping the ROUTE public while moving its
/// disclosures behind the credential preserves that poll and still stops an
/// anonymous caller from reading the daemon's pid, bound project, and version.
pub const PUBLIC_HEALTH_PATH: &str = "/health";

/// `POST` here (authenticated) to mint a single-use SSE ticket.
///
/// Why: `EventSource` cannot send an `Authorization` header, so a browser
/// client exchanges its credential for a ticket here and puts THAT in the SSE
/// URL — see `trusty_common::server::bearer_auth`'s module docs.
pub const SSE_TICKET_PATH: &str = "/auth/sse-ticket";

/// Shared axum state: the JSON-RPC router (for `POST /rpc`), the session
/// registry (for the SSE route, which reads it directly rather than going
/// through the router), and the daemon's project binding (for `GET /health`).
///
/// Why: every field is an `Arc` so cloning per-request (axum requires
/// `State` to be `Clone`) is cheap and every route sees the same daemon-wide
/// instances. `binding` is here rather than resolved per request because a
/// daemon binds exactly ONE project for its whole life — that immutability
/// is precisely what makes it worth publishing on `/health` (#4512).
#[derive(Clone)]
struct HttpState {
    router: Arc<Router>,
    sessions: Arc<SessionRegistry>,
    binding: Arc<ProjectBinding>,
    // #5439: the same handle the router-wide guard holds, so `POST
    // /auth/sse-ticket` mints into the table the guard redeems from.
    auth: DaemonAuth,
}

/// Build the pure axum router (no listener bound) exposing `POST /rpc`,
/// `GET /health`, `GET /sessions/{id}/events`, and (#2983 Slice 2 + Slice 3)
/// the full `/sessions*` REST resource group.
///
/// Why: kept separate from [`run_http`] so unit tests exercise routing and
/// dispatch via `tower::util::ServiceExt::oneshot` without opening a real
/// socket, and so the shared guarded middleware stack wraps it identically to
/// trusty-agents/trusty-memory/trusty-search.
///
/// #6003: the stack is
/// `trusty_common::server::with_guarded_middleware_same_origin_cors`, not the
/// permissive `with_standard_middleware` this router used to call. Two things
/// change. `guard_write_origin` runs router-wide and innermost, so a browser
/// POST/PUT/PATCH/DELETE carrying a foreign `Origin` is `403`ed before any
/// handler runs — `POST /rpc` (the whole JSON-RPC method surface), `/tasks`,
/// `/sessions`, `/sessions/{id}/messages`, `/agents`, and the `workstream.*`
/// write routes. And the CORS policy reflects only same-machine origins
/// instead of `Any`, so a page on a foreign origin cannot READ
/// `GET /sessions/{id}/transcript` or either SSE stream either — those are
/// `GET`, which the method-gated guard deliberately lets through, and they
/// carry conversation content. Callers that send no `Origin` at all (the
/// `tcode tui` client, the console reverse proxy, `curl`) are untouched: the
/// guard passes an absent `Origin` through, and CORS is browser-enforced.
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
/// `http_rest_get_agent_and_skill_catalog_routes_are_merged_in`;
/// the #6003 guard arms live in `http_origin_guard_tests`.
pub fn build_axum_router(
    router: Arc<Router>,
    sessions: Arc<SessionRegistry>,
    workstreams: SharedWorkstreamStore,
    binding: Arc<ProjectBinding>,
    // #5439: the daemon's local-client credential. Taken as a parameter (not
    // read from disk here) so the router stays pure and unit-testable, and so
    // there is no construction path that yields an unguarded router.
    auth: DaemonAuth,
) -> AxumRouter {
    let state = HttpState {
        router: router.clone(),
        sessions,
        binding,
        auth: auth.clone(),
    };
    let core = AxumRouter::new()
        .route("/rpc", post(rpc_handler))
        .route(PUBLIC_HEALTH_PATH, get(health_handler))
        .route(SSE_TICKET_PATH, post(sse_ticket_handler))
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
    // #5439: applied to the FULLY-MERGED router with `.layer()`, never
    // `route_layer` — the latter covers only routes registered before it in the
    // same chain, which is how #3268 left routes unguarded on the sibling
    // origin guard. Every route above, and every route a later edit merges in
    // here, is guarded without anyone remembering to add it.
    let authed = app.layer(axum::middleware::from_fn_with_state(auth, require_bearer));
    // #6003: the daemon binds loopback only (`run_http`), so the guard needs no
    // bind-derived allowlist — `SelfOrigins::default()` trusts loopback and
    // nothing else. This stack sits OUTSIDE the credential check, so a CORS
    // preflight is answered without a credential and a foreign-origin write is
    // still 403ed rather than 401ed.
    trusty_common::server::with_guarded_middleware_same_origin_cors(authed, SelfOrigins::default())
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

/// `GET /health` — liveness for anyone, identity for a credentialed caller.
///
/// Why: two callers with different rights meet on this one route. The
/// ecosystem convention (trusty-search, trusty-memory) is an unauthenticated
/// `GET /health`, and trusty-console's gateway polls it holding no credential,
/// so the route must keep answering an anonymous caller. But #6472: the
/// payload it used to answer with carried the daemon's pid, its bound project
/// ROOT PATH, and its version — process and filesystem facts an anonymous
/// local caller has no claim on, and a fingerprint for anything probing
/// loopback ports. Splitting by credential rather than by route keeps both the
/// gateway poll and `tcode tui`'s auto-attach working: the TUI reads the same
/// credential file the daemon writes, so it still gets the `binding` it needs
/// (#4512) to refuse a daemon bound to a different project.
///
/// What: an anonymous caller gets [`liveness_payload`] — `{"status":"ok"}`,
/// nothing else. A caller the guard marked [`Authenticated`] gets
/// [`health_payload`], the exact payload the `health` JSON-RPC method returns,
/// unchanged. Always HTTP 200 either way; a health route must not turn a
/// missing credential into a failure, or every liveness poller would read a
/// live daemon as down.
/// Test: `http_health_anonymous_discloses_only_liveness`,
/// `http_health_matches_jsonrpc_health_payload`,
/// `http_health_reports_the_daemons_project_binding`.
async fn health_handler(
    State(state): State<HttpState>,
    authenticated: Option<Extension<Authenticated>>,
) -> Json<serde_json::Value> {
    if authenticated.is_some() {
        return Json(health_payload(&state.binding));
    }
    Json(liveness_payload())
}

/// `POST /auth/sse-ticket` — exchange a bearer credential for a single-use SSE
/// ticket.
///
/// Why: the two SSE routes (`GET /sessions/{id}/events`,
/// `GET /workstreams/{id}/events`) are opened by browser `EventSource`, which
/// cannot attach an `Authorization` header. Putting the durable token in the
/// query string instead would write it into every access log and tracing span;
/// a ticket that expires in seconds and dies on first use does not carry that
/// cost. Reaching this route already requires the header, so a ticket is never
/// a way IN — only a way to carry an existing right onto a URL.
/// What: takes the path the caller means to open (`?path=`), refuses anything
/// that is not one of this daemon's two SSE routes, and mints a ticket bound to
/// that exact path. The route is NOT public, so an anonymous `POST` is `401`ed
/// by the guard before this function runs.
///
/// The binding is what keeps a ticket cheap to leak. Unbound, a ticket read
/// from a trace log within its TTL bought one arbitrary authenticated request —
/// `POST /rpc`, the whole method surface, included. Bound, it buys the one
/// stream its holder already had the credential to open. The route allowlist
/// lives here rather than in `trusty_common` because it is this daemon's route
/// table, not a property of the guard.
/// Test: `http_auth_tests::sse_ticket_requires_a_credential`,
/// `http_auth_tests::an_sse_ticket_opens_one_stream_then_is_spent`,
/// `http_auth_tests::a_ticket_cannot_be_minted_for_a_non_sse_path`.
async fn sse_ticket_handler(
    State(state): State<HttpState>,
    axum::extract::Query(params): axum::extract::Query<SseTicketRequest>,
) -> Result<Json<serde_json::Value>, (axum::http::StatusCode, Json<serde_json::Value>)> {
    if !path_is_ticketable(&params.path) {
        // No echo of the requested path — this response reaches a browser.
        return Err((
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "not a ticketable stream"})),
        ));
    }
    Ok(Json(
        serde_json::json!({ "ticket": state.auth.issue_ticket(&params.path) }),
    ))
}

/// The `?path=` a ticket request names.
#[derive(serde::Deserialize)]
struct SseTicketRequest {
    path: String,
}

/// Is `path` one of the two SSE streams a ticket may open?
///
/// Why: a ticket is only as narrow as the set of paths it can be minted for. A
/// handler that minted for whatever it was handed would give back the arbitrary
/// authenticated request the binding exists to remove.
/// What: exact shape match on `/sessions/{id}/events` and
/// `/workstreams/{id}/events` — three segments, the first and last fixed, the
/// id non-empty and free of `/`. Anything else, `/rpc` included, is refused.
/// Test: `http_auth_tests::a_ticket_cannot_be_minted_for_a_non_sse_path`.
fn path_is_ticketable(path: &str) -> bool {
    let segments: Vec<&str> = path.split('/').collect();
    // A leading `/` yields an empty first segment: ["", group, id, "events"].
    matches!(segments.as_slice(),
        ["", group, id, "events"]
            if (*group == "sessions" || *group == "workstreams") && !id.is_empty()
    )
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
    // #4512: published on `GET /health` so an auto-attaching client can tell
    // WHICH project this daemon serves before it starts driving it.
    binding: Arc<ProjectBinding>,
) -> Result<()> {
    // #5439: establish the credential BEFORE binding. A daemon that cannot
    // write its token file must refuse to serve rather than serve unguarded,
    // so this `?` is deliberately fatal — the one place in this function that
    // is not best-effort.
    let auth = DaemonAuth::new(
        trusty_common::daemon_token::ensure_token(TOKEN_APP_NAME)
            .context("tcode serve --http: establish the daemon credential")?,
        [PUBLIC_HEALTH_PATH],
    )
    .context("tcode serve --http: the stored daemon credential is too weak to guard the API")?;

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

    let app = build_axum_router(Arc::new(router), sessions, workstreams, binding, auth);
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

#[cfg(test)]
#[path = "http_origin_guard_tests.rs"]
mod origin_guard_tests;

#[cfg(test)]
#[path = "http_auth_tests.rs"]
mod auth_tests;
