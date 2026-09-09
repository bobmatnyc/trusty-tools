//! A stand-in trusty-* daemon on a Unix socket, for this crate's tests (#7237).
//!
//! Why: `search_index`'s find-or-create used to be exercised against a
//! `TcpListener` speaking hand-rolled HTTP, because that is how it reached
//! trusty-search. #7237 moved it onto the socket, so every one of those rigs has
//! to serve a framed JSON-RPC envelope instead — and a copy of the accept loop
//! per rig is the duplication the workspace's common-entry-point rule exists to
//! prevent.
//!
//! What: [`spawn_at`] binds `socket` through the same [`crate::uds::server`]
//! pieces the real daemon uses, mounts `handler` as the router's catch-all, and
//! serves until the returned [`MockUdsDaemon`] drops. [`spawn`] does the same at
//! a path under a `TempDir` it mints, for a rig that dials the socket directly
//! rather than through production discovery. The handler answers a `result`
//! value directly, or an [`RpcError`] to make the daemon refuse.
//!
//! This is a `#[cfg(test)]` module, so it never ships.
//!
//! Test: every caller — `search_rpc::tests` and `search_index::tests`.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tempfile::TempDir;

pub use crate::uds::server::RpcError;
use crate::uds::server::{RpcFallback, RpcRouter, RpcServeOptions, serve_until};

/// What one mock call answers, as a boxed future.
///
/// Boxed rather than generic because a rig's handler may capture shared state
/// it mutates per call: it cannot be a plain function of its arguments.
pub type MockFuture = Pin<Box<dyn Future<Output = Result<Value, RpcError>> + Send>>;

/// A running mock daemon. Dropping it stops the accept loop.
pub struct MockUdsDaemon {
    socket: PathBuf,
    /// Held only when [`spawn`] minted the directory. [`spawn_at`] binds inside
    /// a directory the caller owns and keeps alive.
    _dir: Option<TempDir>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl MockUdsDaemon {
    /// The path a client under test should dial.
    pub fn socket(&self) -> &Path {
        &self.socket
    }
}

impl Drop for MockUdsDaemon {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
    }
}

/// The catch-all that hands every method to the test's closure.
struct MockFallback<F> {
    handler: F,
}

#[async_trait]
impl<F> RpcFallback for MockFallback<F>
where
    F: Fn(&str, Value) -> MockFuture + Send + Sync + 'static,
{
    async fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        (self.handler)(method, params).await
    }
}

/// Start a mock daemon answering every method through `handler`, on a socket
/// under a temp directory this function owns.
///
/// # Panics
///
/// When the socket cannot be bound — a test-only failure with no recovery.
pub async fn spawn<F>(handler: F) -> MockUdsDaemon
where
    F: Fn(&str, Value) -> MockFuture + Send + Sync + 'static,
{
    let dir = TempDir::new().expect("tempdir for the mock socket");
    let socket = dir.path().join("daemon.sock");
    let mut daemon = spawn_at(socket, handler).await;
    daemon._dir = Some(dir);
    daemon
}

/// Start a mock daemon at a socket path the CALLER chose.
///
/// Why: a rig that exercises production socket DISCOVERY cannot take whatever
/// path [`spawn`] minted — it has to bind where `daemon_socket_path` will look,
/// under a `TRUSTY_DATA_DIR_OVERRIDE` the test controls.
///
/// # Panics
///
/// When the socket cannot be bound — a test-only failure with no recovery.
pub async fn spawn_at<F>(socket: PathBuf, handler: F) -> MockUdsDaemon
where
    F: Fn(&str, Value) -> MockFuture + Send + Sync + 'static,
{
    let listener = crate::uds::bind_hardened(&socket).expect("bind the mock socket");

    let router = Arc::new(RpcRouter::new().fallback(MockFallback { handler }));
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        serve_until(&listener, router, RpcServeOptions::default(), async {
            let _ = rx.await;
        })
        .await;
    });

    MockUdsDaemon {
        socket,
        _dir: None,
        shutdown: Some(tx),
    }
}

/// A mock daemon owning its own runtime on its own OS thread.
///
/// Why: `search_index`'s rigs are SYNCHRONOUS — they hold
/// `crate::data_dir::ENV_LOCK` across the whole arrangement, and the code they
/// drive is a blocking function. An async rig would have to hold that guard
/// across an `.await`, which is both a clippy refusal and a lock this crate's
/// other env-mutating tests rely on being held for a bounded, non-yielding
/// stretch. Giving the daemon its own thread keeps the rig exactly the shape the
/// retired `TcpListener` fixtures had.
/// What: dropping it stops the accept loop and joins the thread.
pub struct BlockingMockDaemon {
    socket: PathBuf,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl BlockingMockDaemon {
    /// The path a client under test should dial.
    pub fn socket(&self) -> &Path {
        &self.socket
    }
}

impl Drop for BlockingMockDaemon {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Start a [`BlockingMockDaemon`] at `socket`, returning only once it is bound.
///
/// Why the readiness handshake: a rig that returned before `bind_hardened` ran
/// would race the client's dial against the bind and flake.
///
/// # Panics
///
/// When the runtime cannot be built or the socket cannot be bound — test-only
/// failures with no recovery.
pub fn spawn_blocking_at<F>(socket: PathBuf, handler: F) -> BlockingMockDaemon
where
    F: Fn(&str, Value) -> MockFuture + Send + Sync + 'static,
{
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let bind_at = socket.clone();

    let thread = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime for the mock daemon");
        runtime.block_on(async move {
            let listener = crate::uds::bind_hardened(&bind_at).expect("bind the mock socket");
            let router = Arc::new(RpcRouter::new().fallback(MockFallback { handler }));
            let _ = ready_tx.send(());
            serve_until(&listener, router, RpcServeOptions::default(), async {
                let _ = stop_rx.await;
            })
            .await;
        });
    });

    ready_rx
        .recv()
        .expect("the mock daemon must bind before the rig proceeds");
    BlockingMockDaemon {
        socket,
        shutdown: Some(stop_tx),
        thread: Some(thread),
    }
}
