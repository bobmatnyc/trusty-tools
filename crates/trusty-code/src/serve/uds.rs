//! The `tcode` daemon's only network transport — a hardened Unix socket
//! (#6637, ADR-0032).
//!
//! Why: a loopback TCP port is reachable by any process on the machine,
//! including a page running in the operator's browser, which is what forced
//! the bearer-token and origin-guard stack #6491 added. A `0600` socket inside
//! a `0700` directory, with a peer-uid check on every accept, is an OS
//! boundary instead of a convention, so none of that machinery is ported here
//! — [`trusty_common::uds::server`]'s module docs state why re-adding it would
//! guard nothing. The owner ruling of 2026-09-08 makes UDS this daemon's
//! native transport: `tcode tui` dials the socket directly, and the HTTP the
//! GUI webview needs is bound by `trusty-code-gui`, which dials this socket.
//!
//! What: [`run_daemon`] is the persistent daemon's whole body. It binds the
//! socket and treats a bind failure as fatal, then builds the router and
//! serves it until SIGTERM/SIGINT.
//!
//! The method surface is mounted, not restated. Exactly one of the 34
//! registered JSON-RPC methods (`session.attach`) uses
//! `crate::jsonrpc::ConnectionContext::notify`, and a UDS connection is one
//! frame in and one frame out, so it cannot push. The other 33 ignore the
//! context entirely, which is what lets [`JsonRpcFallback`] carry the whole
//! table through [`trusty_common::uds::server::RpcRouter::fallback`] rather
//! than re-registering each name — registering the same table twice is the
//! silent-drift risk this workspace's common-entry-point rule exists to stop.
//! `session.attach` itself is REFUSED here rather than half-served: its replay
//! would arrive but its push channel is a throwaway, so a caller that took the
//! answer would sit on a connection that never delivers another event. The
//! refusal names [`crate::session::events_stream::METHOD`], which is that
//! caller's real destination. The workstream fan-out is
//! [`crate::workstreams::events_stream`]; both streams are registered as
//! STREAM methods, which the router checks before the fallback, so the names
//! win over the pass-through.
//!
//! Test: `uds_tests`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::Value;
use tracing::info;
use trusty_common::uds::server::{
    CODE_INVALID_REQUEST, RpcError, RpcFallback, RpcRouter, RpcServeOptions, serve_until,
};
use trusty_common::uds::{bind_singleton_hardened, server::RpcStreamItems};
use trusty_mcp::Request;

use crate::binding::ProjectBinding;
use crate::jsonrpc::{ConnectionContext, Router};
use crate::session::SessionRegistry;
use crate::session::events_stream::{self, SessionEventsParams};
use crate::workstreams::SharedWorkstreamStore;
use crate::workstreams::events_stream::{self as workstream_events, WorkstreamEventsParams};

/// The application name whose data directory holds the daemon's socket.
///
/// Why named rather than inlined: [`trusty_common::daemon_addr::daemon_socket_path`]
/// is the single answer to "where does this daemon answer", and a server and a
/// client that disagree about the path produce a daemon that is up and a probe
/// that reports it down. Both ends resolve it from this constant.
pub const SOCKET_APP_NAME: &str = "trusty-code";

/// The socket `tcode serve` binds and every client dials.
///
/// # Errors
///
/// Whatever `resolve_data_dir` returns — the data directory could not be
/// resolved or created.
///
/// Test: `uds_tests::socket_path_sits_in_the_daemon_data_dir`.
pub fn socket_path() -> Result<PathBuf> {
    trusty_common::daemon_addr::daemon_socket_path(SOCKET_APP_NAME)
        .context("tcode serve: resolve the daemon socket path")
}

/// Build the one-shot [`ConnectionContext`] every socket call dispatches
/// against.
///
/// Why: [`crate::jsonrpc::Router::dispatch`] requires a context, but a socket
/// call is one frame in and one frame out — there is no live connection for a
/// handler to push notifications back to. The handler that would try
/// (`session.attach`) is refused before it reaches the dispatcher.
/// What: a fresh `mpsc::unbounded_channel()`, receiver immediately dropped —
/// a queued notification simply stops on its next send.
/// Test: exercised by every `uds_tests` call (a method only answers at all if
/// its context is constructible).
fn throwaway_ctx() -> ConnectionContext {
    let (notify_tx, _notify_rx) = tokio::sync::mpsc::unbounded_channel();
    ConnectionContext::new(notify_tx)
}

/// What `session.attach` is answered with over the socket.
///
/// Why a constant: [`crate::session::events_stream::METHOD`] is the caller's
/// real destination, and a test asserts on this exact wording — a client that
/// reads the message must be told where to go, not merely told no.
const ATTACH_REFUSAL: &str = "session.attach is not served over the daemon socket: its replay would arrive but its push channel would deliver nothing. Open the session.events stream method instead.";

/// Mounts [`crate::jsonrpc::Router`] as the socket router's catch-all.
///
/// Why: see the module docs — the dispatcher already owns the method surface,
/// and registering those 33 names a second time here is the drift this avoids.
/// What: rebuilds the [`Request`] envelope the dispatcher takes and hands back
/// either half of its [`trusty_mcp::Response`]. The id is `0` on the way in
/// and discarded on the way out — [`RpcRouter`] stamps the caller's real id
/// onto the response frame, so the one supplied here would only be
/// overwritten. `session.attach` never reaches the dispatcher: it is refused
/// with [`ATTACH_REFUSAL`] naming `session.events`.
/// Test: `uds_tests::fallback_dispatches_every_jsonrpc_router_method`,
/// `uds_tests::fallback_reports_an_unknown_method`,
/// `uds_tests::session_attach_over_the_socket_points_at_session_events`.
pub struct JsonRpcFallback {
    router: Arc<Router>,
}

#[async_trait]
impl RpcFallback for JsonRpcFallback {
    async fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        // #6637: a throwaway context cannot carry `session.attach`'s push half,
        // so the caller is sent to the stream method that can.
        if method == crate::session::protocol::ATTACH_METHOD {
            return Err(RpcError::new(CODE_INVALID_REQUEST, ATTACH_REFUSAL));
        }
        let request = Request {
            jsonrpc: Some("2.0".to_string()),
            id: Some(serde_json::json!(0)),
            method: method.to_string(),
            params: Some(params),
        };
        let response = self
            .router
            .dispatch(request, &throwaway_ctx())
            .await;
        match (response.result, response.error) {
            (Some(result), _) => Ok(result),
            (None, Some(error)) => Err(RpcError::new(i64::from(error.code), error.message)),
            // The dispatcher's own contract is that exactly one of the two is
            // present. Saying so beats answering `null` as a success.
            (None, None) => Err(RpcError::internal(format!(
                "dispatcher answered {method} with neither result nor error"
            ))),
        }
    }
}

/// Assemble the socket router: two stream methods plus the whole JSON-RPC
/// table behind them.
///
/// Why: kept separate from [`run_daemon_on`] so a test can dispatch against it
/// without binding a socket.
/// What: registers [`crate::session::events_stream::METHOD`] and
/// [`crate::workstreams::events_stream::METHOD`] with
/// [`RpcRouter::typed_stream`], then mounts [`JsonRpcFallback`]. Neither
/// stream is marked with `RpcRouter::mark_liveness`: an open tail is real work
/// and must hold the connection, not be discounted as a monitor's poll.
/// Test: `uds_tests::stream_names_win_over_the_fallback`.
pub fn build_rpc_router(
    router: Arc<Router>,
    sessions: Arc<SessionRegistry>,
    workstreams: SharedWorkstreamStore,
) -> RpcRouter {
    RpcRouter::new()
        .typed_stream(events_stream::METHOD, move |params: SessionEventsParams| {
            let sessions = sessions.clone();
            async move { events_stream::open(sessions, params).await }
        })
        .typed_stream(
            workstream_events::METHOD,
            move |params: WorkstreamEventsParams| {
                let workstreams = workstreams.clone();
                async move { open_workstream(workstreams, params).await }
            },
        )
        .fallback(JsonRpcFallback { router })
}

/// Named rather than inlined so the closure above stays a one-liner and the
/// return type is written once.
async fn open_workstream(
    workstreams: SharedWorkstreamStore,
    params: WorkstreamEventsParams,
) -> Result<RpcStreamItems, RpcError> {
    workstream_events::open(workstreams, params).await
}

/// Run the persistent `tcode` daemon: bind the socket and serve it until
/// SIGTERM/SIGINT.
///
/// Why/What: see module docs. This is the whole of `tcode serve` — #6637
/// PR 2c retired the TCP listener that used to be bound alongside it, so
/// there is no transport to select and no port to name.
///
/// # Errors
///
/// The socket could not be bound or hardened, or the router could not be
/// assembled.
///
/// Test: `uds_tests::run_uds_socket_bind_failure_is_fatal`,
/// `uds_tests::session_events_stream_stays_open_under_idle_window`.
pub async fn run_daemon(binding: ProjectBinding) -> Result<()> {
    let socket = socket_path()?;
    run_daemon_on(binding, &socket, None, trusty_common::shutdown_signal()).await
}

/// [`run_daemon`] with the socket path, the workstream data directory and the
/// shutdown trigger injected.
///
/// Why: the same seam [`crate::serve::build_router_at`] has, and `pub`
/// because an integration test outside this crate needs it to stand a real
/// daemon up on a throwaway socket — driving the real bind/serve/unlink
/// machinery against a tempdir socket and its own shutdown signal, without
/// touching the operator's data directory or waiting on a real one.
/// What: `data_dir` of `None` is production — `crate::serve::build_router`,
/// which resolves the real `~/.trusty-code` and starts the log-drain
/// scheduler. `Some(dir)` routes through `build_router_at` instead, which does
/// neither.
pub async fn run_daemon_on(
    binding: ProjectBinding,
    socket: &Path,
    data_dir: Option<&Path>,
    shutdown: impl std::future::Future<Output = ()> + Send,
) -> Result<()> {
    // Fatal on failure: a daemon that could not take the socket has no other
    // way to answer, so coming up at all would leave every client reporting
    // "no daemon" against a process that is plainly running.
    // `bind_singleton_hardened` (not `bind_hardened`) because this daemon is
    // restart-prone and has to take over the socket file a killed predecessor
    // left behind; a LIVE owner is still refused.
    let listener = bind_singleton_hardened(socket).await.with_context(|| {
        format!(
            "tcode serve: bind the daemon socket at {}",
            socket.display()
        )
    })?;

    info!(
        binding = binding.state(),
        project = binding.label().unwrap_or_else(|| "<projectless>".into()),
        socket = %socket.display(),
        "tcode serve: starting"
    );
    eprintln!("tcode serve: listening on {}", socket.display());

    let (router, sessions, workstreams) = match data_dir {
        Some(dir) => super::build_router_at(binding, dir).await?,
        None => super::build_router(binding).await?,
    };
    let router = Arc::new(router);
    let rpc_router = Arc::new(build_rpc_router(
        router.clone(),
        sessions.clone(),
        workstreams.clone(),
    ));

    serve_until(&listener, rpc_router, RpcServeOptions::default(), shutdown).await;
    // Unlink BEFORE dropping the listener — `RpcServer::run` records why: with
    // the order reversed a successor that rebinds in the gap has its fresh
    // socket deleted by this process.
    if let Err(e) = std::fs::remove_file(socket) {
        tracing::debug!(socket = %socket.display(), error = %e, "socket already gone");
    }
    drop(listener);

    sessions.shutdown_executions(super::SHUTDOWN_GRACE).await;
    info!("tcode serve: stopped");
    Ok(())
}

#[cfg(test)]
#[path = "uds_tests.rs"]
mod uds_tests;
