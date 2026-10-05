//! Serve a real `SearchAppState` on a scratch Unix socket (#9168).
//!
//! Why: the MCP bridge reaches the daemon only over its socket, so an
//! end-to-end test needs the daemon's REAL socket router — the same
//! `service::socket::serve_until_shutdown` `trusty-search start` runs — on a
//! socket nothing else can see. A loopback axum listener would test a route
//! the bridge no longer dials.
//! What: [`serve_state_at`] binds `socket` and serves `state` until the
//! returned [`SocketDaemon`] drops; [`serve_state`] picks a socket under its
//! own `TempDir`. Nothing here resolves or dials the live daemon's socket.
//! Test: `mcp_structured_503_5350.rs`, `mcp_reindex_quarantine_8105.rs`,
//! `mcp_stdio_e2e_5264.rs`.

#![allow(dead_code)] // Each test binary uses a different subset.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use trusty_search::service::daemon_client::DaemonClient;
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
    let bound = bind(socket).await.expect("bind the scratch socket");
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(serve_until_shutdown(bound, Arc::new(state), async {
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
