//! Serve a real `SearchAppState` on a scratch Unix socket (#9168).
//!
//! Why: the MCP bridge reaches the daemon only over its socket, so an
//! end-to-end test needs the daemon's REAL socket router — the same
//! `service::socket::serve_until_shutdown` `trusty-search start` runs — on a
//! socket nothing else can see. A loopback axum listener would test a route
//! the bridge no longer dials.
//! What: [`serve_state_at`] binds `socket` and serves `state` until the
//! returned [`SocketDaemon`] drops; [`serve_state`] picks a socket under its
//! own `TempDir`. [`serve_logged`] (#9214) puts a proxy in front of the real
//! router that records every `(method, params)` and lets a test answer a call
//! itself — the socket twin of the axum request-log and chaos layers the CLI
//! tests used. Nothing here resolves or dials the live daemon's socket.
//! Test: `mcp_structured_503_5350.rs`, `mcp_reindex_quarantine_8105.rs`,
//! `mcp_stdio_e2e_5264.rs`, `index_remove_env_conflict_8175.rs`,
//! `reindex_quantize_env_conflict_8737.rs`, `index_remove_residency_8687.rs`.

#![allow(dead_code)] // Each test binary uses a different subset.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::Value;
use trusty_common::uds::server::{serve_until, RpcError, RpcFallback, RpcRouter, RpcServeOptions};
use trusty_search::service::daemon_client::{DaemonCallError, DaemonClient};
use trusty_search::service::server::SearchAppState;
use trusty_search::service::socket::{bind, serve_until_shutdown};

/// A daemon serving its real socket router. Dropping it stops the server.
pub struct SocketDaemon {
    /// A client on the served socket.
    pub client: DaemonClient,
    /// The socket path the daemon bound.
    pub socket: PathBuf,
    _stop: tokio::sync::oneshot::Sender<()>,
    _dir: Option<tempfile::TempDir>,
}

/// Serve `state` on `socket` until the returned handle drops.
pub async fn serve_state_at(state: SearchAppState, socket: &Path) -> SocketDaemon {
    serve_shared_state_at(Arc::new(state), socket).await
}

/// [`serve_state_at`] for a state the test keeps a handle to.
pub async fn serve_shared_state_at(state: Arc<SearchAppState>, socket: &Path) -> SocketDaemon {
    let bound = bind(socket).await.expect("bind the scratch socket");
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(serve_until_shutdown(bound, state, async {
        let _ = stopped.await;
    }));
    SocketDaemon {
        client: DaemonClient::at(socket),
        socket: socket.to_path_buf(),
        _stop: stop,
        _dir: None,
    }
}

/// Serve `state` on a socket under a fresh `TempDir`.
pub async fn serve_state(state: SearchAppState) -> SocketDaemon {
    let dir = tempfile::tempdir().expect("scratch socket dir");
    let socket = dir.path().join("ts.sock");
    let mut daemon = serve_state_at(state, &socket).await;
    daemon._dir = Some(dir);
    daemon
}

/// Every `(method, params)` a [`LoggedDaemon`] received, in arrival order.
pub type CallLog = Arc<Mutex<Vec<(String, Value)>>>;

/// A test's chance to answer a call before it reaches the real router.
pub type Intercept = Box<dyn Fn(&str, &Value) -> Option<Result<Value, RpcError>> + Send + Sync>;

/// A real router behind a recording proxy. Dropping it stops both.
pub struct LoggedDaemon {
    /// The proxy socket a CLI under test is pointed at.
    pub socket: PathBuf,
    /// Every call the proxy received, intercepted or forwarded.
    pub calls: CallLog,
    _upstream: SocketDaemon,
    _stop: tokio::sync::oneshot::Sender<()>,
    _dir: tempfile::TempDir,
}

impl LoggedDaemon {
    /// A client on the real router, past the proxy: reads through it are not
    /// recorded.
    pub fn upstream(&self) -> &DaemonClient {
        &self._upstream.client
    }

    /// The recorded calls, cloned.
    pub fn snapshot(&self) -> Vec<(String, Value)> {
        self.calls.lock().expect("call log").clone()
    }

    /// Every recorded call that is not a read: anything that could create,
    /// delete, rebuild, re-quantize or relocate an index.
    pub fn mutations(&self) -> Vec<(String, Value)> {
        const READS: &[&str] = &[
            "search.health",
            "search.indexes.list",
            "search.index.status",
        ];
        self.snapshot()
            .into_iter()
            .filter(|(m, _)| !READS.contains(&m.as_str()))
            .collect()
    }
}

struct Proxy {
    upstream: DaemonClient,
    calls: CallLog,
    intercept: Intercept,
}

#[async_trait]
impl RpcFallback for Proxy {
    async fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        self.calls
            .lock()
            .expect("call log")
            .push((method.to_string(), params.clone()));
        if let Some(answer) = (self.intercept)(method, &params) {
            return answer;
        }
        self.upstream
            .call(method, params)
            .await
            .map_err(|e| match e {
                DaemonCallError::Refused {
                    code,
                    message,
                    data,
                    ..
                } => {
                    let refusal = RpcError::new(code, message);
                    match data {
                        Some(data) => refusal.with_data(data),
                        None => refusal,
                    }
                }
                other => RpcError::internal(other.to_string()),
            })
    }
}

/// Serve `state` behind a recording proxy on a fresh scratch socket.
///
/// What: the real socket router on `<dir>/up.sock`; the proxy on
/// `<dir>/ts.sock` logs each call, answers it from `intercept` when that
/// returns `Some`, and forwards it otherwise. Streams are not proxied.
pub async fn serve_logged(
    state: Arc<SearchAppState>,
    intercept: impl Fn(&str, &Value) -> Option<Result<Value, RpcError>> + Send + Sync + 'static,
) -> LoggedDaemon {
    let dir = tempfile::tempdir().expect("scratch socket dir");
    let upstream = serve_shared_state_at(state, &dir.path().join("up.sock")).await;
    let socket = dir.path().join("ts.sock");
    let calls: CallLog = Arc::default();
    let listener = trusty_common::uds::bind_hardened(&socket).expect("bind the proxy socket");
    let router = Arc::new(RpcRouter::new().fallback(Proxy {
        upstream: upstream.client.clone(),
        calls: Arc::clone(&calls),
        intercept: Box::new(intercept),
    }));
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        serve_until(&listener, router, RpcServeOptions::default(), async {
            let _ = stopped.await;
        })
        .await;
    });
    LoggedDaemon {
        socket,
        calls,
        _upstream: upstream,
        _stop: stop,
        _dir: dir,
    }
}
