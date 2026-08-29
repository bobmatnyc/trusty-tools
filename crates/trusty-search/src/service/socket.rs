//! The daemon's hardened Unix-domain-socket listener (#6285 slice 1).
//!
//! Why: ADR-0032 moves every trusty-* service off loopback TCP and onto a Unix
//! socket, with `trusty-console` left as the workspace's only HTTP surface.
//! trusty-search's HTTP surface is ~35 routes over a `service::server` tree of
//! ~16k SLOC, with eleven consumer crates dialling `127.0.0.1:7878`, so a
//! single-PR cutover is not viable — the same conclusion #6288 reached for
//! trusty-mpm, which migrated in six slices. The socket is bound ALONGSIDE the
//! HTTP listener, route families move onto it one slice at a time, and the
//! retire slice deletes the axum surface and moves the consumers.
//!
//! What: the socket path (derived, never published to a discovery file), the
//! bind, the serve loop, and [`METHODS`] — the names this listener answers. A
//! name no slice has claimed answers `method_not_found` naming what is served,
//! which is the contract every later slice inherits.
//!
//! ## Slices
//!
//! | Slice | Surface | State |
//! |---|---|---|
//! | 1 | the listener itself, plus [`METHOD_HEALTH`] | landed (PR #6367) |
//! | 2 | the READ surface — indexes, status, config, chunks, graph, call chain | landed (PR #6368), in [`crate::service::rpc::reads`] |
//! | 3 | the QUERY surface — search and its fan-out, grep and its fan-out, similarity, typeahead | landed (PR #6377), in [`crate::service::rpc::queries`] |
//! | 4 | the WRITE surface — index create, delete, relocate, the two per-file writes, the reindex trigger, contributed-graph ingest | landed (PR #6381), in [`crate::service::rpc::writes`] |
//! | 5 | the STREAMS — reindex progress and the daemon status stream | landed (PR #6383), in [`crate::service::rpc::streams`] |
//! | 5.5 | the REMAINDER with a named consumer — the two config writes, the log tail, the registry orphan census — plus the frame-cap raise | landed (PR #6385), in [`crate::service::rpc::admin`] |
//! | 5.6 | the graceful stop and the chat answer | this one, in [`crate::service::rpc::admin`] and [`crate::service::rpc::chat`] |
//! | consumer wave | move the eleven dialling crates onto these names, one PR per crate | not started |
//! | retire | delete the axum surface, `ui.rs`, and the `http_addr` writer | not started, gated on the consumer wave |
//!
//! **Slice 5.6 closes the last two gaps a consumer named.** Slice 5.5 read
//! `POST /admin/stop` and `POST /chat` as having no external caller; both
//! readings were wrong. trusty-mpm's TUI drives the stop route from its `[X]`
//! key (found by PR #6388's implementation pass), and #6155 moves the search UI
//! — chat panel included — into `trusty-console`, which reaches this daemon over
//! the socket. Two routes remain deliberately HTTP-only:
//! `GET /api/chat/providers` is read by the UI shell alone and answers a
//! question `search.chat`'s own `provider` field already carries per call, and
//! `GET /metrics` is Prometheus text, which is HTTP-shaped by nature.
//! `POST /upgrade` has no observed caller off this box.
//! `GET /indexes/{id}/communities` is a DEAD route — `trusty-common`'s monitor
//! calls it and the daemon has never served it — and is a pre-existing bug with
//! its own ticket, not a gap this surface owes a method for.
//!
//! **Two doors, one daemon.** The socket serves the same `Arc<SearchAppState>`
//! the axum router was built on — see
//! [`crate::service::server::build_router_on`]. A second `Arc::new` would give
//! the socket its own registry and its own tickers, and the two transports
//! would disagree about which indexes exist.
//!
//! **The trust boundary is the socket, not the payload.**
//! `bind_singleton_hardened` puts a `0600` socket in a `0700` directory, and
//! `serve_until` runs the peer-uid check on every accepted connection before a
//! byte is read. None of `service::server`'s `SelfOrigins` machinery is carried
//! over: it is browser-CSRF defence for a listener a page can reach, and it
//! guards nothing here (#6277 design review).
//!
//! Test: `socket_tests.rs`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use tokio::net::UnixListener;
use trusty_common::uds::server::{serve_until, RpcRouter, RpcServeOptions};

use crate::service::rpc::{admin, chat, queries, reads, streams, writes};
use crate::service::server::SearchAppState;

#[cfg(test)]
#[path = "socket_tests.rs"]
mod tests;

/// Liveness, index count and this daemon's version.
///
/// This is the method `trusty-console`'s connector and `tctl`'s health probe
/// will dial once the retire slice moves them off `GET /health`. Renaming it
/// breaks both, in crates that have no Cargo edge on this one.
pub const METHOD_HEALTH: &str = "search.health";

/// Every method this listener serves, in registration order.
///
/// Why an array: eleven crates outside this one reach trusty-search, and after
/// the retire slice they will dial these names by literal rather than by a
/// Cargo edge. An array is what a contract test can compare against, so a
/// rename shows up as a failing assertion rather than as a consumer silently
/// reporting `method_not_found`.
/// What: the `<domain>.<verb>` convention `trusty-review`'s `review.*` and
/// `trusty-analyze`'s `analyze.*` sets established, one entry per
/// [`build_router`] registration. A family's own names are spliced in from its
/// module rather than restated here, so a rename is a compile error rather than
/// a drift between two lists. Each later slice appends its own family.
/// Test: `rpc_router_registers_every_documented_method`.
pub const METHODS: &[&str] = &[
    METHOD_HEALTH,
    // #6285 slice 2 — the read surface.
    reads::METHOD_INDEXES_LIST,
    reads::METHOD_INDEX_STATUS,
    reads::METHOD_INDEX_CONFIG_GET,
    reads::METHOD_CONFIG_GET,
    reads::METHOD_CHUNKS_LIST,
    reads::METHOD_GRAPH_GET,
    reads::METHOD_GRAPH_STATS,
    reads::METHOD_GRAPH_NEIGHBORS,
    reads::METHOD_CALL_CHAIN,
    // #6285 slice 3 — the query surface.
    queries::METHOD_QUERY,
    queries::METHOD_QUERY_ALL,
    queries::METHOD_GREP,
    queries::METHOD_GREP_ALL,
    queries::METHOD_SIMILAR,
    queries::METHOD_TYPEAHEAD,
    // #6285 slice 4 — the write surface.
    writes::METHOD_INDEX_CREATE,
    writes::METHOD_INDEX_DELETE,
    writes::METHOD_INDEX_RELOCATE,
    writes::METHOD_INDEX_FILE_PUT,
    writes::METHOD_INDEX_FILE_REMOVE,
    writes::METHOD_INDEX_REINDEX,
    writes::METHOD_GRAPH_INGEST,
    // #6285 slice 5 — the streams. Registered in a SEPARATE router table from
    // the twenty-three above: a name is streaming or unary, never both, so
    // `rpc_router_registers_every_documented_method` compares this array
    // against the union of the two.
    streams::METHOD_STATUS_STREAM,
    streams::METHOD_INDEX_REINDEX_STREAM,
    // #6285 slice 5.5 — the operational remainder. Back in the unary table with
    // the twenty-three above.
    admin::METHOD_INDEX_CONFIG_SET,
    admin::METHOD_CONFIG_SET,
    admin::METHOD_LOGS_TAIL,
    admin::METHOD_REGISTRY_ORPHANS,
    // #6285 slice 5.6 — the last two routes with a consumer: the TUI's stop key
    // and the search UI's chat panel. Both unary, both in the free lane.
    admin::METHOD_ADMIN_STOP,
    chat::METHOD_CHAT,
];

/// The params of a method that takes no arguments.
///
/// Why: [`RpcRouter::typed`] decodes `params` into the handler's request type
/// before the handler runs, and `params` is absent — `serde_json::Value::Null`
/// — on a well-formed call to a no-argument method. A plain unit struct refuses
/// `null`, so every health probe would answer `invalid_params`.
/// What: accepts anything and keeps nothing. A caller that sends a stray field
/// is not refused: this method has no arguments to get wrong, and refusing
/// would turn an additive client change into an outage.
/// Test: `rpc_health_answers_with_no_params`,
/// `rpc_health_answers_with_a_stray_params_object`.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct NoParams;

impl<'de> Deserialize<'de> for NoParams {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        serde::de::IgnoredAny::deserialize(deserializer)?;
        Ok(NoParams)
    }
}

/// The socket this daemon binds, as both it and its consumers resolve it.
///
/// Why derived rather than published: `trusty_common::daemon_socket_path` is
/// the same entry point trusty-memory, trusty-review, trusty-analyze and
/// trusty-mpm resolve, so the daemon and every consumer compute the identical
/// path and there is no discovery file for a stale write to contradict. The
/// `http_addr` / port files this daemon still writes belong to the TCP listener
/// and are retired with it, not here.
///
/// # Errors
///
/// When the data directory cannot be resolved or created.
///
/// Test: `socket_path_is_the_product_named_socket_under_the_data_dir`.
pub fn socket_path() -> Result<PathBuf> {
    // #6285: the ONE data-dir entry point; never a second hand-rolled resolver.
    trusty_common::daemon_socket_path("trusty-search")
}

/// Map every method onto its handler.
///
/// Why: the only trusty-search-specific half of the server. Everything else —
/// the peer-uid check, the framing, the JSON-RPC envelope, the accept loop — is
/// [`trusty_common::uds::server`]'s.
/// What: [`METHOD_HEALTH`] reads its report through
/// [`crate::service::server::health_report`] — the same call `GET /health`
/// makes, so the two transports cannot answer differently. Each slice appends
/// one `register` call for its own route family; slice 5's
/// [`streams::register`] is the only one that registers into the router's
/// STREAMING table rather than its unary one.
/// Test: `rpc_router_registers_every_documented_method`,
/// `rpc_reports_method_not_found_for_an_unknown_method`.
fn build_router(state: &Arc<SearchAppState>) -> RpcRouter {
    let health_state = Arc::clone(state);
    let router = RpcRouter::new().typed::<NoParams, serde_json::Value, _, _>(
        METHOD_HEALTH,
        move |_params| {
            let state = Arc::clone(&health_state);
            async move { Ok(crate::service::server::health_report(state).await) }
        },
    );
    let router = reads::register(router, state);
    let router = queries::register(router, state);
    let router = writes::register(router, state);
    let router = admin::register(router, state);
    let router = chat::register(router, state);
    streams::register(router, state)
}

/// The frame budget this listener accepts, in bytes (#6285 slice 5.5).
///
/// Why 64 MiB and not a figure of this surface's own: it is exactly the
/// `DefaultBodyLimit` `POST /indexes/{id}/graph` carries
/// (`service::server::build_router_on`), which is what makes the two doors take
/// the same payload rather than nearly the same one. A contributed graph is the
/// largest thing either transport moves — PR #1129 recorded ~20 MB maxima from
/// large pilot corpora and sized the HTTP limit at ~3× that — and a slice-3
/// query response can also pass 8 MiB when a caller asks for full content at a
/// large `top_k`. Both were refused here and accepted there until this slice.
///
/// A caller must raise its own budget to match with
/// [`trusty_common::uds::send_framed_request_capped`]; see [`serve_options`].
///
/// Test: `serve_options_carries_the_raised_frame_budget`,
/// `a_request_frame_over_the_shared_default_is_accepted_and_refused_at_the_default`.
pub const MAX_FRAME_BYTES: u64 = 64 * 1024 * 1024;

/// Per-connection budgets for this listener.
///
/// The read bound is the shared default — 30 seconds to deliver a REQUEST
/// frame. It bounds no handler; slice 3's query methods are bounded by the
/// interactive query deadline instead, which they share with their HTTP routes
/// (see [`queries::register`]). It is not a stream's idle budget either: neither
/// end applies a deadline between two frames of a response (#6286).
///
/// **The frame budget is [`MAX_FRAME_BYTES`], and the two ends now match.** The
/// shared 8 MiB default is a control-plane figure, and this surface is not one:
/// `search.graph.ingest` carries a document HTTP accepts up to 64 MiB, so until
/// this slice a producer whose graph exceeded 8 MiB was refused on the socket
/// and served on HTTP. Raising the listener alone would only have moved which
/// end refuses — [`trusty_common::uds::send_framed_request`] applies the 8 MiB
/// default to the RESPONSE it reads — so a consumer moving onto the query or
/// ingest names dials through
/// [`trusty_common::uds::send_framed_request_capped`] with this same figure
/// rather than the plain helper. Anything smaller on the client turns a request
/// this listener accepted into a `FrameTooLarge` on the way back.
///
/// The raise costs no memory at rest. It is a ceiling on how far a single
/// unterminated frame may grow before the read is refused, not an allocation:
/// an ordinary `search.health` frame is a few hundred bytes either way.
///
/// Slice 5's streams are unaffected. The budget bounds each streamed frame
/// separately rather than their sum, and one progress event or one `DaemonEvent`
/// is a few hundred bytes — a stream reaches the cap only if a single event
/// does.
///
/// The HTTP half of the pairing is a compile-time link rather than a test:
/// `build_router_on` names [`MAX_FRAME_BYTES`] in its `DefaultBodyLimit` instead
/// of restating the literal, so the two doors cannot drift apart.
///
/// Test: `serve_options_carries_the_raised_frame_budget`,
/// `a_request_frame_over_the_shared_default_is_accepted_and_refused_at_the_default`,
/// `a_client_budget_below_this_listeners_refuses_a_response_it_serves`.
fn serve_options() -> RpcServeOptions {
    RpcServeOptions {
        max_frame_bytes: MAX_FRAME_BYTES,
        ..RpcServeOptions::default()
    }
}

/// A bound RPC socket, with the path it occupies.
///
/// Why the two travel together: the listener must be served and the path must
/// be unlinked afterwards, and the unlink has to name the path this listener
/// actually took. Passing them separately lets a caller hand
/// [`serve_until_shutdown`] a path that was not the one [`bind`] used, which
/// unlinks somebody else's socket. One value cannot be assembled wrong.
///
/// Test: exercised by every test in `socket_tests.rs`.
#[derive(Debug)]
pub struct BoundSocket {
    /// The listener [`bind`] took.
    pub listener: UnixListener,
    /// The path it occupies, unlinked by [`serve_until_shutdown`].
    pub path: PathBuf,
}

/// Bind `socket`, refusing to start when another daemon is live on it.
///
/// Why `bind_singleton_hardened` rather than `bind_hardened`: this daemon is
/// supervised by launchd with `KeepAlive`, and a predecessor that is SIGKILLed
/// — which is what `launchctl kickstart -k` does at the `ExitTimeOut` boundary
/// — never reaches the unlink in [`serve_until_shutdown`]. `bind_hardened`
/// refuses an occupied path outright, so the replacement launchd starts would
/// fail its bind, exit, be restarted, and fail again — a crash loop with no
/// operator-visible cause, the same shape as the #2566 port collision.
/// `bind_singleton_hardened` probes first and takes over only a socket the
/// kernel proves nobody is serving, so a LIVE daemon is still never clobbered.
///
/// # Errors
///
/// Any bind failure, including `UdsSecurityError::AlreadyServing` — which means
/// another trusty-search is answering this path and this process must not
/// start. The error names the path; `run_daemon` propagates it rather than
/// degrading to an HTTP-only daemon.
///
/// Test: `bind_refuses_a_socket_another_process_is_serving`,
/// `bind_reclaims_a_stale_socket_file`.
pub async fn bind(socket: &Path) -> Result<BoundSocket> {
    let listener = trusty_common::uds::bind_singleton_hardened(socket)
        .await
        .with_context(|| format!("bind trusty-search socket at {}", socket.display()))?;
    Ok(BoundSocket {
        listener,
        path: socket.to_path_buf(),
    })
}

/// Serve `listener` until `shutdown` resolves, then unlink its socket file.
///
/// What: the accept loop from `trusty_common::uds::server`, then the socket
/// file is removed BEFORE the listener is dropped. That order is the one
/// `webhook_relay::listener` records: reversed, there is a window in which
/// nothing answers the path but the file is still there, and a successor that
/// rebinds in that window has its fresh socket deleted by this process.
///
/// A SIGKILLed daemon never reaches this unlink. That is not a leak to guard
/// against here — [`bind`] reclaims a socket file nobody is serving.
///
/// Test: `serve_unlinks_its_socket_on_shutdown`,
/// `rpc_reports_method_not_found_for_an_unknown_method`.
pub async fn serve_until_shutdown(
    bound: BoundSocket,
    state: Arc<SearchAppState>,
    shutdown: impl std::future::Future<Output = ()> + Send,
) {
    let BoundSocket { listener, path } = bound;
    let router = Arc::new(build_router(&state));
    tracing::info!(
        socket = %path.display(),
        methods = router.method_names().count(),
        streams = router.stream_names().count(),
        "trusty-search serving rpc"
    );

    serve_until(&listener, router, serve_options(), shutdown).await;

    if let Err(e) = std::fs::remove_file(&path) {
        tracing::debug!(socket = %path.display(), error = %e, "socket already gone");
    }
    drop(listener);
}
