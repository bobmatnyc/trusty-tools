//! trusty-console's own Unix socket: `console.health` and
//! `console.fleet_status` (#10902, D8b (a), ADR-0065).
//!
//! Why: ADR-0032 puts service-to-service traffic on UDS and keeps the console
//! as the only HTTP surface; ADR-0035 has the console aggregate health over
//! UDS. Until now the console only DIALLED sockets. This listener lets the TUI
//! read the console's fleet view without HTTP, from the cache the console's
//! poller already keeps warm.
//! What: [`start`] binds `daemon_socket_path("trusty-console")` with
//! [`bind_singleton_hardened`] (0600 socket in a 0700 directory, a stale file
//! reclaimed, a live owner refused) and serves [`router`] through
//! `trusty_common::uds::server::serve_until`, which checks the peer's uid on
//! every connection before reading a byte. [`ConsoleSocket::shutdown`] stops
//! the loop and unlinks the socket before dropping the listener.
//!
//! **Bind failure is a warning, not an exit.** The HTTP listener stays the
//! console's primary surface and is unaffected; the TUI falls back to direct
//! per-service probes when this socket does not answer (D8b (b)). Services
//! whose UDS IS their transport (trusty-memory, trusty-search, trusty-events,
//! the tm daemon) exit on a failed bind instead.
//!
//! Test: `tests.rs` beside this file.

// See #10902

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use tokio::net::UnixListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use trusty_common::console_rpc::{
    CONSOLE_SERVICE, ConsoleFleetStatus, ConsoleHealth, METHOD_FLEET_STATUS, METHOD_HEALTH,
};
use trusty_common::uds::server::{RpcError, RpcRouter, RpcServeOptions, serve_until};
use trusty_common::uds::{UdsSecurityError, bind_singleton_hardened};

use crate::server::AppState;

#[cfg(test)]
mod tests;

/// Longest a `console.fleet_status` call waits on the MCP-handle overlay.
///
/// Why: the overlay reads each handle's state lock, and the metrics poller can
/// hold that lock across a child spawn and MCP `initialize`. A slow poller must
/// never stall a UDS reply, so past this budget the call answers with the
/// poller's own rows and `overlaid: false`.
/// Test: `fleet_status_equals_the_http_services_body` (the overlay path).
pub const OVERLAY_BUDGET: Duration = Duration::from_millis(250);

/// Params for a method that takes none: any value, including absent, decodes.
#[derive(Debug, Clone, Copy, Default)]
struct NoParams;

impl<'de> Deserialize<'de> for NoParams {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        serde::de::IgnoredAny::deserialize(deserializer)?;
        Ok(NoParams)
    }
}

/// The `console.health` answer for `state`.
///
/// Test: `router_answers_both_methods`.
pub fn health(state: &AppState) -> ConsoleHealth {
    ConsoleHealth {
        status: "ok".to_string(),
        service: CONSOLE_SERVICE.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        uptime_secs: state.uptime_secs(),
    }
}

/// The `console.fleet_status` answer: the services snapshot from the cache.
///
/// Why: D8b (b) — the TUI reads the console's view instead of probing every
/// service itself, which only pays if no call triggers a probe.
/// What: reads the poller cache once. Empty cache → `ready: false` and no rows
/// (the HTTP route's synchronous first-boot detection is deliberately NOT
/// reproduced). Otherwise the rows pass through `server::overlay_services`,
/// the function `GET /api/console/services` uses, bounded by
/// [`OVERLAY_BUDGET`].
/// Test: `fleet_status_equals_the_http_services_body`,
/// `fleet_status_on_a_cold_cache_answers_not_ready_without_probing`.
pub async fn fleet_status(state: &AppState) -> ConsoleFleetStatus {
    let Some(snapshot) = state.poller_cache().snapshot().await else {
        return ConsoleFleetStatus::default();
    };
    let overlay = crate::server::overlay_services(state, snapshot.services.clone());
    match tokio::time::timeout(OVERLAY_BUDGET, overlay).await {
        Ok(services) => ConsoleFleetStatus {
            ready: true,
            overlaid: true,
            services,
        },
        Err(_) => {
            tracing::warn!(
                budget_ms = OVERLAY_BUDGET.as_millis() as u64,
                "console.fleet_status: handle overlay exceeded its budget; serving raw cached rows"
            );
            let mut services = snapshot.services;
            crate::detect::order_for_display(&mut services);
            ConsoleFleetStatus {
                ready: true,
                overlaid: false,
                services,
            }
        }
    }
}

/// The console socket's method table.
///
/// Why: the only console-specific half of the server; framing, the peer check
/// and the accept loop are `trusty_common::uds::server`'s.
/// What: `console.health` is registered with `typed_liveness` (a pure status
/// answer); `console.fleet_status` with `typed`. Any other name gets the
/// router's `method_not_found` error.
/// Test: `router_answers_both_methods`,
/// `router_registers_exactly_the_documented_methods`,
/// `unknown_method_returns_an_rpc_error`.
pub fn router(state: AppState) -> RpcRouter {
    let health_state = state.clone();
    RpcRouter::new()
        .typed_liveness::<NoParams, ConsoleHealth, _, _>(METHOD_HEALTH, move |_| {
            let state = health_state.clone();
            async move { Ok::<_, RpcError>(health(&state)) }
        })
        .typed::<NoParams, ConsoleFleetStatus, _, _>(METHOD_FLEET_STATUS, move |_| {
            let state = state.clone();
            async move { Ok::<_, RpcError>(fleet_status(&state).await) }
        })
}

/// A running console socket. Dropping it without [`Self::shutdown`] leaves
/// the file for the next start's singleton bind to reclaim.
#[derive(Debug)]
pub struct ConsoleSocket {
    path: PathBuf,
    stop: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl ConsoleSocket {
    /// The socket path being served.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Stop accepting, let in-flight calls drain, and unlink the socket.
    ///
    /// Test: `listener_serves_both_methods_over_a_real_socket`.
    pub async fn shutdown(self) {
        let _ = self.stop.send(());
        if let Err(e) = self.task.await {
            tracing::warn!(error = %e, "console socket task ended abnormally");
        }
    }
}

/// Bind `path` and serve [`router`] on it in a background task.
///
/// # Errors
///
/// The [`UdsSecurityError`] from [`bind_singleton_hardened`]: the path is
/// unbindable, unsafe, or another console is serving it.
///
/// Test: `listener_serves_both_methods_over_a_real_socket`,
/// `socket_is_0600_inside_a_0700_directory`.
pub async fn start_at(state: AppState, path: &Path) -> Result<ConsoleSocket, UdsSecurityError> {
    let listener = bind_singleton_hardened(path).await?;
    let router = Arc::new(router(state));
    let (stop, stopped) = oneshot::channel::<()>();
    let socket = path.to_path_buf();
    let task = tokio::spawn(serve(listener, router, socket.clone(), async move {
        let _ = stopped.await;
    }));
    tracing::info!(socket = %path.display(), "trusty-console UDS listener bound");
    Ok(ConsoleSocket {
        path: socket,
        stop,
        task,
    })
}

/// Serve until `shutdown`, then unlink the socket BEFORE dropping the
/// listener, so a successor that rebinds in between never has its fresh
/// socket deleted (the order `RpcServer::run` records).
async fn serve(
    listener: UnixListener,
    router: Arc<RpcRouter>,
    socket: PathBuf,
    shutdown: impl std::future::Future<Output = ()> + Send,
) {
    serve_until(&listener, router, RpcServeOptions::default(), shutdown).await;
    if let Err(e) = std::fs::remove_file(&socket) {
        tracing::debug!(socket = %socket.display(), error = %e, "console socket already gone");
    }
    drop(listener);
}

/// Bind the console socket at its canonical path, or warn and return `None`.
///
/// Why: see the module docs — a failed bind degrades the console to HTTP-only
/// rather than stopping it, because the HTTP surface is primary and the TUI
/// has a ruled fallback.
/// What: resolves `daemon_socket_path("trusty-console")` and calls
/// [`start_at`]; either failure is logged at `warn` with the path and cause.
/// Test: `start_at` is covered by `listener_serves_both_methods_over_a_real_socket`;
/// the warn path by `a_failed_bind_warns_and_http_keeps_serving`.
pub async fn start(state: AppState) -> Option<ConsoleSocket> {
    let path = match trusty_common::daemon_socket_path(CONSOLE_SERVICE) {
        Ok(path) => path,
        Err(e) => {
            tracing::warn!(error = %e, "trusty-console: no socket path; serving HTTP only");
            return None;
        }
    };
    match start_at(state, &path).await {
        Ok(socket) => Some(socket),
        Err(e) => {
            tracing::warn!(
                socket = %path.display(),
                error = %e,
                "trusty-console: UDS listener not bound; serving HTTP only"
            );
            None
        }
    }
}
