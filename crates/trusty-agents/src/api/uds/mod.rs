//! The `--serve` / `--api` daemon's transport: a hardened Unix socket
//! (#6433 slice 2, ADR-0032, ADR-0018).
//!
//! Why: this daemon bound loopback TCP — `127.0.0.1:8080` by default, `8765`
//! as the Tauri shell launched it — and published the bound address in the
//! `http_addr` discovery file. It can spawn arbitrary subprocesses and mutate
//! live sessions, so every process on the host could reach that, and the
//! defences were browser-shaped: a same-origin CORS variant, a write guard, an
//! optional bearer token. ADR-0018 makes trusty-console the only HTTP surface
//! in the workspace; slice 1 (#7026) moved this crate's search daemon and this
//! is the same migration applied to the API daemon.
//!
//! What: the 47 routes are unchanged. `Router` still defines them; the
//! transport under it is [`trusty_common::uds::server`] rather than
//! `axum::serve`, and one method — [`wire::METHOD_REQUEST`] — carries an HTTP
//! exchange into it (see [`dispatch`]). The owner's #6433 ruling took this
//! branch over ~47 typed methods so the route table is not restated in a
//! second place where the two can drift.
//!
//! **The trust boundary is the socket, not the payload.** The socket sits at
//! `0600` inside a `0700` directory, and `serve_until` runs
//! `ensure_peer_is_self` on every accepted connection before a byte is read.
//! The router's own middleware — the same-origin CORS variant and the write
//! guard — is left mounted rather than stripped: it costs nothing on a
//! socket where no request carries an `Origin`, and removing it would mean
//! auditing 47 routes for what it was covering.
//!
//! Module layout (split for the 500-line cap):
//! - `mod.rs` — socket path, bind, the serve loop, the liveness probe.
//! - [`wire`] — the two frame types and the method names.
//! - `dispatch` — frame in, `Router` call, frame out.
//! - [`client`] — the in-repo caller's half.
//!
//! Test: `tests` submodule — path convention, hardening modes, stale-socket
//! takeover, second-bind refusal, and a real round trip through the router.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use axum::Router;
use trusty_common::uds::server::{RpcRouter, RpcServeOptions, serve_until};

pub mod client;
mod dispatch;
pub mod wire;

#[cfg(test)]
mod tests;

pub use client::ApiSocketClient;
pub use wire::{HealthResponse, HttpRequestFrame, HttpResponseFrame, METHOD_HEALTH, METHOD_REQUEST};

/// Environment override for the API socket path.
///
/// Why: the derived path is per-project and per-home, which is right for an
/// operator and wrong for a test or a sandbox that owns neither. One variable
/// answers both without a flag that would also have to be threaded through the
/// `--api` early dispatch in `runtime::startup`.
/// What: when set and non-empty, its value is the socket path verbatim.
pub const SOCKET_PATH_ENV: &str = "TAGENT_API_SOCKET";

/// How long [`is_service_running`] waits for `agents.health` to answer.
///
/// Why 500ms: the probe runs on the REPL's startup path, before the prompt
/// appears. A local socket round-trip is sub-millisecond, so this is generous
/// for a live daemon and short enough that a wedged one does not stall startup.
/// It replaces the identical budget the retired `GET /api/health` probe used.
const HEALTH_PROBE_TIMEOUT: Duration = Duration::from_millis(500);

/// Resolve the canonical Unix-socket path for the API daemon.
///
/// Why: mirrors `search_socket_path` (#7026) and `ctrl::socket::ctrl_socket_path`,
/// so all three of this crate's sockets live in one directory under one naming
/// convention. Per-project because the daemon is: two checkouts of the same
/// repository each run their own API server over their own state.
///
/// #6433: this is the daemon's whole address. There is no `http_addr` discovery
/// file and no port — caller and daemon derive the same path from the same
/// project root, so there is nothing for a stale file to disagree with.
/// What: [`SOCKET_PATH_ENV`] when set, else
/// `~/.trusty-agents/sockets/<project_id>.api.sock`, else — with no home
/// directory — `<project_root>/.trusty-agents/state/api.sock`.
/// Test: `api_socket_path_uses_project_id`, `api_socket_path_honors_the_env_override`.
pub fn api_socket_path(project_root: &Path) -> PathBuf {
    if let Ok(raw) = std::env::var(SOCKET_PATH_ENV)
        && !raw.trim().is_empty()
    {
        return PathBuf::from(raw);
    }
    let project_id = crate::ctrl::socket::project_id_from_path(project_root);
    if let Some(home) = dirs::home_dir() {
        home.join(".trusty-agents")
            .join("sockets")
            .join(format!("{project_id}.api.sock"))
    } else {
        project_root
            .join(".trusty-agents")
            .join("state")
            .join("api.sock")
    }
}

/// The API socket path for the project this process is running in.
///
/// Why: every caller but a test resolves the same root — `detect_self_project()`
/// with the cwd as fallback, the identity `service::pid_file_path()` already
/// uses. Open-coding that at each call site is how two of them would come to
/// disagree.
/// What: [`api_socket_path`] applied to that root.
/// Test: `self_socket_path_matches_the_derived_path`.
pub fn self_socket_path() -> PathBuf {
    let root = crate::ctrl::detect_self_project()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    api_socket_path(&root)
}

/// Delete the discovery files the TCP daemon used to publish (#6433).
///
/// Why: `~/.trusty-agents/http_addr` recorded the bound `host:port` so the
/// console proxy could resolve this surface. Nothing reads it now, and leaving
/// it on disk invites a future reader to trust an address that belongs to
/// whatever else has since bound it — the hazard #6285/#6287 record for the
/// search and analyze rows of the console proxy. Removing it at every start
/// makes the retirement observable rather than assumed.
/// What: best-effort unlink. A missing file is the expected case after the
/// first start.
/// Test: `serve_removes_the_retired_discovery_file`.
pub fn remove_retired_discovery_file() {
    if let Err(e) = trusty_common::remove_daemon_addr("trusty-agents") {
        tracing::debug!(error = %e, "no retired trusty-agents http_addr file to remove");
    }
}

/// Build the RPC router that fronts `app`.
///
/// Why split from [`serve_with_shutdown`]: a test needs the method table
/// without owning a socket, and `rpc_router_registers_every_documented_method`
/// is what keeps the two method-name constants honest.
/// What: two methods — [`METHOD_HEALTH`], answered without touching `app`, and
/// [`METHOD_REQUEST`], which is `dispatch::dispatch`.
/// Test: `rpc_router_registers_every_documented_method`.
pub fn build_rpc_router(app: Router) -> RpcRouter {
    RpcRouter::new()
        .typed_liveness::<wire::NoParams, HealthResponse, _, _>(METHOD_HEALTH, move |_| async move {
            Ok(HealthResponse::current())
        })
        .typed::<HttpRequestFrame, HttpResponseFrame, _, _>(METHOD_REQUEST, move |frame| {
            let app = app.clone();
            async move { dispatch::dispatch(app, frame).await }
        })
}

/// Bind `socket` and serve `app` until SIGTERM/SIGINT, then unlink it.
///
/// # Errors
///
/// As [`serve_with_shutdown`].
///
/// Test: `serve_answers_health_over_a_real_socket`.
pub async fn serve(app: Router, socket: &Path) -> Result<()> {
    serve_with_shutdown(app, socket, trusty_common::shutdown_signal()).await
}

/// [`serve`], with the shutdown future supplied by the caller.
///
/// Why both signatures exist: `serve` waits on SIGTERM/SIGINT, which a test
/// cannot deliver to its own process without stopping the whole test binary.
///
/// Why the bind is [`trusty_common::uds::bind_singleton_hardened`] rather than
/// `bind_hardened`: a predecessor that is SIGKILLed never reaches the unlink and
/// leaves its socket file behind. `bind_hardened` refuses an occupied path, so
/// the next `--api` would fail to start with no operator-visible cause.
/// `bind_singleton_hardened` probes first and takes over only a path the kernel
/// proved nobody is serving, so a live daemon is still never clobbered. That
/// probe is this daemon's stale-socket recovery, and a second bind against a
/// LIVE daemon still fails loudly with `AlreadyServing`.
///
/// The unlink runs BEFORE the listener is dropped — the order
/// `trusty_common::webhook_relay::listener` records, because reversed there is a
/// window in which nothing answers the path but the file is still there, and a
/// successor that rebinds in that window has its fresh socket deleted by this
/// process.
///
/// # Errors
///
/// When the socket cannot be bound — including
/// `UdsSecurityError::AlreadyServing`, which means another API daemon is live on
/// this path and this process must not start.
///
/// Test: `serve_unlinks_its_socket_on_shutdown`,
/// `serve_takes_over_a_socket_left_by_a_dead_daemon`,
/// `a_second_bind_against_a_live_daemon_fails`.
pub async fn serve_with_shutdown(
    app: Router,
    socket: &Path,
    shutdown: impl std::future::Future<Output = ()> + Send,
) -> Result<()> {
    // #6433: the TCP address this daemon used to publish is retired, and the
    // file predates the migration on every existing machine.
    remove_retired_discovery_file();

    let listener = trusty_common::uds::bind_singleton_hardened(socket)
        .await
        .map_err(|e| anyhow::anyhow!("bind trusty-agents API socket at {}: {e}", socket.display()))?;

    let router = Arc::new(build_rpc_router(app));
    tracing::info!(
        socket = %socket.display(),
        methods = router.method_names().count(),
        "trusty-agents api serving"
    );

    serve_until(&listener, router, RpcServeOptions::default(), shutdown).await;

    if let Err(e) = std::fs::remove_file(socket) {
        tracing::debug!(socket = %socket.display(), error = %e, "socket already gone");
    }
    drop(listener);
    Ok(())
}

/// Returns true iff an API daemon is observably answering for this project.
///
/// Why: REPL startup uses this to decide whether to report a running service,
/// and `service::start_service` uses it for its idempotence check. A socket
/// file's existence does not prove anyone is serving it; only an answered
/// `agents.health` does.
///
/// #6433: this replaces the pid-file-plus-`GET /api/health` pair. The pid file
/// is still written — `stop_service` needs a pid to signal — but it is no
/// longer consulted to decide liveness, because a socket that answers is
/// stronger evidence than a pid that exists.
/// What: dials [`self_socket_path`] with a [`HEALTH_PROBE_TIMEOUT`] budget.
/// Test: `is_service_running_is_false_with_no_socket`,
/// `health_ok_is_true_against_a_live_daemon`.
pub async fn is_service_running() -> bool {
    health_ok(&self_socket_path()).await
}

/// Call `agents.health` on `socket` and report whether it answered.
///
/// Split from [`is_service_running`] so a test can point it at a temporary
/// socket without owning the home directory the real path resolves under.
///
/// Test: `health_ok_is_true_against_a_live_daemon`.
pub async fn health_ok(socket: &Path) -> bool {
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": METHOD_HEALTH,
        "params": {},
    });
    matches!(
        trusty_common::uds::send_framed_request::<_, trusty_common::uds::server::RpcResponse>(
            socket,
            &request,
            HEALTH_PROBE_TIMEOUT,
        )
        .await,
        Ok(response) if response.result.is_some()
    )
}
