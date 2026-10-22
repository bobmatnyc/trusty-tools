//! The `tcode` daemon's Unix-socket transport — the only door into the
//! persistent daemon (#6637, ADR-0032).
//!
//! Why: a loopback TCP port is reachable by any process on the machine,
//! including a page running in the operator's browser, which is what forced
//! the bearer-token and origin-guard stack #6491 added. A `0600` socket inside
//! a `0700` directory, with a peer-uid check on every accept, is an OS
//! boundary instead of a convention, so none of that machinery is ported here
//! — [`trusty_common::uds::server`]'s module docs state why re-adding it would
//! guard nothing. The owner ruling of 2026-09-08 makes UDS this daemon's
//! native transport; `tcode tui` dials it and `trusty-code-gui` bridges it.
//!
//! What: [`run_daemon`] is the persistent daemon's whole body. It binds the
//! socket and treats a bind failure as fatal, builds the router ONCE, and
//! serves it until SIGTERM/SIGINT. A serve loop that ends without that signal
//! is an error too ([`join_serve_task`]), so the process exits non-zero for
//! whoever restarts it rather than lingering as a daemon nobody can reach.
//!
//! The method surface is mounted, not restated. Exactly one of the 34
//! registered JSON-RPC methods (`session.attach`) uses
//! `crate::jsonrpc::ConnectionContext::notify`, and a UDS connection is one
//! frame in and one frame out, so it cannot push. The other 33 ignore the
//! context entirely, which is what lets [`JsonRpcFallback`] carry the whole
//! table through [`trusty_common::uds::server::RpcRouter::fallback`] rather
//! than re-registering each name — registering the same table twice is the
//! silent-drift risk this workspace's common-entry-point rule exists to stop.
//! `session.attach`'s live half is served instead by
//! [`crate::session::events_stream`], and the workstream fan-out by
//! [`crate::workstreams::events_stream`]; both are registered as STREAM
//! methods, which the router checks before the fallback, so the names win over
//! the pass-through.
//!
//! #6637 slice 2c retired the loopback HTTP/SSE listener that used to run
//! beside the socket; nothing in this daemon binds a TCP port.
//!
//! Test: `uds_tests`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::Value;
use tracing::info;
use trusty_common::uds::server::{RpcError, RpcFallback, RpcRouter, RpcServeOptions, serve_until};
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

/// The socket a daemon serving `project` binds when the shared socket is held
/// by a daemon serving a different project (#4600).
///
/// Why: one shared socket per user meant a second project could not run at
/// all — its TUI refused the first project's daemon (correctly, #4512) and had
/// nowhere else to start its own. A per-project sibling gives each project a
/// home while the first daemon keeps the shared path `trusty-code-gui` dials.
/// What: `<shared dir>/<shared stem>-<key>.sock`, where `key` is the first 12
/// hex digits of SHA-256 over the canonical project root, or `projectless`.
/// Deterministic, so every client and the daemon itself derive the same path
/// with nothing recorded on disk. A collision is not trusted: clients still
/// check the binding the daemon reports before attaching. 12 digits keep the
/// path inside macOS' 104-byte `sun_path` under a temp data directory.
/// Test: `uds_tests::project_socket_path_is_per_project_and_stable`.
pub fn project_socket_path(shared: &Path, project: Option<&Path>) -> PathBuf {
    use sha2::{Digest, Sha256};
    let key = match project {
        Some(root) => {
            let digest = Sha256::digest(root.as_os_str().as_encoded_bytes());
            digest[..6].iter().map(|b| format!("{b:02x}")).collect()
        }
        None => "projectless".to_string(),
    };
    let stem = shared
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| SOCKET_APP_NAME.to_string());
    shared.with_file_name(format!("{stem}-{key}.sock"))
}

/// A [`ConnectionContext`] whose notification receiver is already dropped.
///
/// Why (#6637): moved here from the retired REST bridge, its only
/// other caller. A handler that queues a notification onto it stops on its
/// next send, which is right for a connection that cannot carry one.
fn throwaway_ctx() -> ConnectionContext {
    let (notify_tx, _notify_rx) = tokio::sync::mpsc::unbounded_channel();
    ConnectionContext::new(notify_tx)
}

/// Mounts [`crate::jsonrpc::Router`] as the socket router's catch-all.
///
/// Why: see the module docs — the dispatcher already owns the method surface,
/// and registering those 33 names a second time here is the drift this avoids.
/// What: rebuilds the [`Request`] envelope the dispatcher takes and hands back
/// either half of its [`trusty_mcp::Response`]. The id is `0` on the way in
/// and discarded on the way out — [`RpcRouter`] stamps the caller's real id
/// onto the response frame, so the one supplied here would only be
/// overwritten. The context is [`throwaway_ctx`]'s, because a one-frame-in,
/// one-frame-out connection has nowhere to push a notification to; the one
/// handler that would try is served as a stream instead.
/// Test: `uds_tests::fallback_dispatches_every_jsonrpc_router_method`,
/// `uds_tests::fallback_reports_an_unknown_method`.
pub struct JsonRpcFallback {
    router: Arc<Router>,
}

#[async_trait]
impl RpcFallback for JsonRpcFallback {
    async fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let request = Request {
            jsonrpc: Some("2.0".to_string()),
            id: Some(serde_json::json!(0)),
            method: method.to_string(),
            params: Some(params),
        };
        let response = self.router.dispatch(request, &throwaway_ctx()).await;
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
/// Why/What: see module docs. #6637 slice 2c removed the `http_port`
/// parameter with the TCP listener it selected.
///
/// # Errors
///
/// The socket could not be bound or hardened (fatal), the router could not be
/// assembled, or the serve loop ended without a shutdown signal.
///
/// Test: `uds_tests::run_uds_socket_bind_failure_is_fatal`,
/// `tests/daemon_no_tcp_6637.rs` (the real binary, every command line).
pub async fn run_daemon(binding: ProjectBinding) -> Result<()> {
    let socket = socket_path()?;
    run_daemon_on(binding, &socket, None, trusty_common::shutdown_signal()).await
}

/// [`run_daemon`] on this binding's own socket, [`project_socket_path`],
/// instead of the shared one (#4600, `tcode serve --project-socket`).
///
/// Why: `tcode tui` starts a daemon here when the shared socket already serves
/// another project. The daemon derives the path from its own binding rather
/// than taking a path argument, so it can only ever land on the path a client
/// of the same project will probe, and never hardens an arbitrary directory.
///
/// # Errors
///
/// As [`run_daemon`].
///
/// Test: `uds_tests::project_socket_path_is_per_project_and_stable` (the path);
/// `tests/cli_e2e.rs::tui_starts_its_own_daemon_beside_another_projects` (the
/// real daemon binding it).
pub async fn run_project_daemon(binding: ProjectBinding) -> Result<()> {
    let socket = project_socket_path(&socket_path()?, binding.root());
    run_daemon_on(binding, &socket, None, trusty_common::shutdown_signal()).await
}

/// [`run_daemon`] with the socket path, the workstream data directory and the
/// shutdown trigger injected.
///
/// Why: the same seam [`crate::serve::build_router_at`] has, and `pub` so an
/// integration test outside this crate can stand a real daemon up on a
/// throwaway socket with its own shutdown signal, without touching the
/// operator's data directory.
/// What: `data_dir` of `None` is production — `crate::serve::build_router`,
/// which resolves the real `~/.trusty-code` and starts the log-drain
/// scheduler. `Some(dir)` routes through `build_router_at` instead, which does
/// neither. The serve loop runs in its own task so a panic in it surfaces here
/// as an error rather than vanishing with the handle.
///
/// # Errors
///
/// As [`run_daemon`].
pub async fn run_daemon_on(
    binding: ProjectBinding,
    socket: &Path,
    data_dir: Option<&Path>,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<()> {
    // Bound FIRST, and fatally: a daemon that could not take the socket has no
    // door at all. `bind_singleton_hardened` (not `bind_hardened`) because this
    // daemon is restart-prone and has to take over the socket file a killed
    // predecessor left behind; a LIVE owner is still refused, which is the
    // only duplicate guard now that no TCP port exists (#6637).
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
        // #8203: the build this process will run for its whole life, so a
        // stale daemon is visible in its own log.
        build = crate::build_info::GIT_HASH,
        "tcode serve: starting"
    );
    eprintln!("tcode serve: listening on {}", socket.display());

    let (router, sessions, workstreams) = match data_dir {
        Some(dir) => super::build_router_at(binding, dir).await?,
        None => super::build_router(binding).await?,
    };
    let rpc_router = Arc::new(build_rpc_router(
        Arc::new(router),
        sessions.clone(),
        workstreams,
    ));

    // #6637: set when the shutdown signal fires, so a loop that returns
    // without it reads as the failure it is.
    let requested = Arc::new(AtomicBool::new(false));
    let flag = requested.clone();
    let shutdown = async move {
        shutdown.await;
        flag.store(true, Ordering::SeqCst);
    };
    let owned_socket = socket.to_path_buf();
    let serve = tokio::spawn(async move {
        serve_until(&listener, rpc_router, RpcServeOptions::default(), shutdown).await;
        // Unlink BEFORE dropping the listener — `RpcServer::run` records why:
        // with the order reversed a successor that rebinds in the gap has its
        // fresh socket deleted by this process.
        if let Err(e) = std::fs::remove_file(&owned_socket) {
            tracing::debug!(socket = %owned_socket.display(), error = %e, "socket already gone");
        }
        drop(listener);
    });
    let served = join_serve_task(serve, &requested).await;

    sessions.shutdown_executions(super::SHUTDOWN_GRACE).await;
    served?;
    info!("tcode serve: stopped");
    Ok(())
}

/// Wait for the serve task, and fail when it ended without a shutdown request.
///
/// Why (#6637, the Fail-Open Check): the socket is the daemon's only door. A
/// loop that dies or returns on its own leaves a process that runs and answers
/// nobody, so it must end the daemon with an error and a non-zero exit for its
/// supervisor to see. Mirrors `trusty-mpm`'s `daemon::socket::join_serve_task`
/// (#6288 S3a).
/// What: `Ok` only when the task returned AND `requested` was set first; a
/// panic, a cancelled task, or an unrequested return is an `Err` naming why.
///
/// # Errors
///
/// As above.
///
/// Test: `uds_tests::join_serve_task_errors_when_the_loop_panics`,
/// `uds_tests::join_serve_task_errors_when_the_loop_ends_without_a_shutdown`,
/// `uds_tests::join_serve_task_is_ok_after_a_requested_shutdown`.
pub async fn join_serve_task(
    task: tokio::task::JoinHandle<()>,
    requested: &AtomicBool,
) -> Result<()> {
    match task.await {
        Err(join) => Err(anyhow::anyhow!(
            "tcode serve: the socket accept loop died: {join}"
        )),
        Ok(()) if !requested.load(Ordering::SeqCst) => Err(anyhow::anyhow!(
            "tcode serve: the socket accept loop ended without a shutdown signal"
        )),
        Ok(()) => Ok(()),
    }
}

#[cfg(test)]
#[path = "uds_tests.rs"]
mod uds_tests;
