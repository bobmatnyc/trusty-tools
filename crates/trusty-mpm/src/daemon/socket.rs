//! The daemon's hardened Unix-domain-socket listener (#6288 slice 1).
//!
//! Why: ADR-0032 moves every trusty-* service off loopback TCP and onto a Unix
//! socket, and trusty-mpm's HTTP surface is ~35k SLOC across a dozen route
//! files. A single-PR cutover is not viable, so the socket was added ALONGSIDE
//! `127.0.0.1:7880` (slice 1) and route families move onto it one slice at a
//! time; the retire slice deletes the axum surface.
//!
//! What: the socket path (derived, never published to a discovery file), the
//! bind, and the serve loop. Which METHODS are served is `daemon::rpc`'s —
//! [`build_router`] names one `register` call per family, and slice 2 mounted
//! the first twenty. A name no family claims still answers `method_not_found`.
//!
//! The trust boundary is the socket, not the payload: `bind_singleton_hardened`
//! puts a `0600` socket in a `0700` directory, and `serve_until` runs the
//! peer-uid check on every accepted connection before a byte is read. None of
//! `daemon::api`'s origin-guard machinery is carried over — it is browser-CSRF
//! defence for a listener a page can reach, and it guards nothing here.
//!
//! Test: `socket_tests.rs`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::net::UnixListener;
use trusty_common::uds::server::{RpcRouter, RpcServeOptions, serve_until};

use super::rpc;
use super::state::DaemonState;

#[cfg(test)]
#[path = "socket_tests.rs"]
mod tests;

/// The socket this daemon binds, as both it and its consumers resolve it.
///
/// Why: derived, not published. `trusty_common::daemon_socket_path` is the same
/// entry point trusty-memory, trusty-review, and trusty-analyze resolve, so the
/// daemon and every consumer compute the identical path and there is no
/// discovery file for a stale write to contradict. It resolves
/// `<data dir>/trusty-mpm.sock` through `trusty_common::resolve_data_dir`,
/// which this crate already routes its bug-report store through.
///
/// # Errors
///
/// When the data directory cannot be resolved or created.
///
/// Test: `socket_path_is_the_product_named_socket_under_the_data_dir`.
pub fn socket_path() -> Result<PathBuf> {
    // #6288: the ONE data-dir entry point; never a second hand-rolled resolver.
    trusty_common::daemon_socket_path("trusty-mpm")
}

/// The methods this listener serves.
///
/// Why: the transport is fixed and this is the only part that grows. Slice 1
/// registered nothing; each later slice adds one `register` call for its own
/// route family, so a family arrives without the accept loop, the framing, or
/// the peer check being touched.
///
/// A name no family claims still answers `method_not_found` — the contract
/// slice 1 established for the whole surface now holds for the remainder of it.
///
/// Test: `unknown_method_gets_a_method_not_found_frame`,
/// `rpc_router_registers_every_documented_method`,
/// `every_scoped_route_has_a_method`.
fn build_router(state: &Arc<DaemonState>) -> RpcRouter {
    // #6288 slice 2: the core request/response families. Slices 5-6 append here.
    let router = rpc::core::register(RpcRouter::new(), state);
    // #6288 slice 3: the legacy session registry, hooks, and the polled feeds.
    let router = rpc::sessions_legacy::register(router, state);
    // #6288 slice 4: managed sessions, the control plane, and the L2 proxy.
    rpc::managed::register(router, state)
}

/// Per-connection budgets for this listener.
///
/// The shared defaults: a 30-second bound on delivering a REQUEST frame, and
/// the shared control-plane frame budget. Neither bounds a handler, and this
/// slice has no handler to bound. A later slice that moves a bulk route across
/// raises `max_frame_bytes` here and on the client together.
fn serve_options() -> RpcServeOptions {
    RpcServeOptions::default()
}

/// A bound RPC socket, with the path it occupies.
///
/// Why the two travel together: the listener must be served and the path must
/// be unlinked afterwards, and the unlink has to name the path this listener
/// actually took. Passing them separately let a caller hand
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
/// supervised by launchd, and a predecessor that is SIGKILLed never reaches the
/// unlink in [`serve_until_shutdown`]. `bind_hardened` refuses an occupied path
/// outright, so the replacement launchd starts would fail its bind, exit, be
/// restarted, and fail again — a crash loop with no operator-visible cause.
/// `bind_singleton_hardened` probes first and takes over only a socket the
/// kernel proves nobody is serving, so a LIVE daemon is still never clobbered.
///
/// # Errors
///
/// Any bind failure, including `UdsSecurityError::AlreadyServing` — which means
/// another trusty-mpm is answering this path and this process must not start.
/// The error names the path; the caller propagates it rather than degrading to
/// an HTTP-only daemon (the Fail-Open Check, #6288).
///
/// Test: `bind_refuses_a_socket_another_process_is_serving`,
/// `bind_reclaims_a_stale_socket_file`.
pub async fn bind(socket: &Path) -> Result<BoundSocket> {
    let listener = trusty_common::uds::bind_singleton_hardened(socket)
        .await
        .with_context(|| format!("bind trusty-mpm socket at {}", socket.display()))?;
    Ok(BoundSocket {
        listener,
        path: socket.to_path_buf(),
    })
}

/// Serve `listener` until `shutdown` resolves, then unlink `socket`.
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
/// `unknown_method_gets_a_method_not_found_frame`.
pub async fn serve_until_shutdown(
    bound: BoundSocket,
    state: Arc<DaemonState>,
    shutdown: impl std::future::Future<Output = ()> + Send,
) {
    let BoundSocket { listener, path } = bound;
    let router = Arc::new(build_router(&state));
    tracing::info!(
        socket = %path.display(),
        methods = router.method_names().count(),
        "trusty-mpm serving rpc"
    );

    serve_until(&listener, router, serve_options(), shutdown).await;

    if let Err(e) = std::fs::remove_file(&path) {
        tracing::debug!(socket = %path.display(), error = %e, "socket already gone");
    }
    drop(listener);
}
