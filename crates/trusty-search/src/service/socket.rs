//! The daemon's hardened Unix-domain-socket listener (#6285 slice 1).
//!
//! Why: ADR-0032 moves every trusty-* service off loopback TCP and onto a Unix
//! socket, with `trusty-console` left as the workspace's only HTTP surface.
//! trusty-search's HTTP surface is ~35 routes over a `service::server` tree of
//! ~16k SLOC, with eleven consumer crates dialling `127.0.0.1:7878`, so a
//! single-PR cutover is not viable — the same conclusion #6288 reached for
//! trusty-mpm, which migrated in six slices. This is slice 1: the socket is
//! bound ALONGSIDE the HTTP listener, route families move onto it one slice at
//! a time, and the retire slice deletes the axum surface and moves the
//! consumers.
//!
//! What: the socket path (derived, never published to a discovery file), the
//! bind, the serve loop, and [`METHODS`] — the names this listener answers.
//! Slice 1 serves [`METHOD_HEALTH`] only. A name no slice has claimed answers
//! `method_not_found` naming what is served, which is the contract every later
//! slice inherits.
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
/// [`build_router`] registration. Each later slice appends its own family.
/// Test: `rpc_router_registers_every_documented_method`.
pub const METHODS: &[&str] = &[METHOD_HEALTH];

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
/// What: slice 1 registers [`METHOD_HEALTH`], which reads the report through
/// [`crate::service::server::health_report`] — the same call `GET /health`
/// makes, so the two transports cannot answer differently. Each later slice
/// appends one `register` call for its own route family.
/// Test: `rpc_router_registers_every_documented_method`,
/// `rpc_reports_method_not_found_for_an_unknown_method`.
fn build_router(state: &Arc<SearchAppState>) -> RpcRouter {
    let health_state = Arc::clone(state);
    RpcRouter::new().typed::<NoParams, serde_json::Value, _, _>(METHOD_HEALTH, move |_params| {
        let state = Arc::clone(&health_state);
        async move { Ok(crate::service::server::health_report(state).await) }
    })
}

/// Per-connection budgets for this listener.
///
/// The shared defaults: a 30-second bound on delivering a REQUEST frame, and
/// the 8 MiB shared frame budget. Neither bounds a handler, and health has
/// nothing to bound. The slice that moves `POST /indexes/{id}/graph` (a 64 MiB
/// axum body limit today) raises `max_frame_bytes` here and on the client
/// together.
fn serve_options() -> RpcServeOptions {
    RpcServeOptions::default()
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
        "trusty-search serving rpc"
    );

    serve_until(&listener, router, serve_options(), shutdown).await;

    if let Err(e) = std::fs::remove_file(&path) {
        tracing::debug!(socket = %path.display(), error = %e, "socket already gone");
    }
    drop(listener);
}
