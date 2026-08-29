#![allow(dead_code)]
//! A stand-in trusty-* daemon on a temp Unix socket, for tests (#6286, #6285).
//!
//! Why: this crate's memory rigs each bound an ephemeral TCP port and served
//! `POST /rpc` (or the REST routes) with axum, because that is how they reached
//! trusty-memory. ADR-0032 retired that listener, so every one of them has to
//! dial a socket instead — and a copy of the accept loop per rig is the
//! duplication the workspace's common-entry-point rule exists to prevent.
//! #6285 moved the trusty-search rigs across the same way, which is why nothing
//! here names a service: the handler decides which daemon it is pretending to
//! be.
//!
//! What: [`spawn`] binds a socket under a `TempDir`, mounts `handler` as the
//! router's catch-all through the same [`trusty_common::uds::server`] pieces the
//! real daemon uses, and serves until the returned [`MockUdsDaemon`] drops.
//! The handler answers a `result` value directly — a rig that needs the
//! `tools/call` envelope wraps it itself, the same way it did over HTTP.
//!
//! This is a `#[cfg(test)]` module, so it never ships.
//!
//! Test: every caller — `core::memory_import::tests`,
//! `tui::coordinator::tests`, `provisioner::identity_seed::tests`,
//! `daemon::doctor_tests`, `daemon::doctor_search_pin_tests`.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tempfile::TempDir;
pub use trusty_common::uds::server::RpcError;
use trusty_common::uds::server::{RpcFallback, RpcRouter, RpcServeOptions, serve_until};

/// What one mock call answers, as a boxed future.
///
/// Boxed rather than generic because one rig's handler awaits a `watch`
/// channel: it cannot be a plain function of its arguments. `Err` is how a rig
/// makes the daemon refuse — the palace-missing case `ensure_palace` branches
/// on used a JSON-RPC error body over HTTP too.
pub type MockFuture = Pin<Box<dyn Future<Output = Result<Value, RpcError>> + Send>>;

/// A running mock daemon. Dropping it stops the accept loop and removes the
/// socket with its temp directory.
pub struct MockUdsDaemon {
    socket: PathBuf,
    _dir: TempDir,
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

/// Start a mock daemon answering every method through `handler`.
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
    let listener = trusty_common::uds::bind_hardened(&socket).expect("bind the mock socket");

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
        _dir: dir,
        shutdown: Some(tx),
    }
}

/// Wrap `inner` the way the daemon's `tools/call` arm answers.
///
/// Why: the real dispatcher stringifies a tool's result into
/// `result.content[0].text`, and the rigs assert the unwrap as well as the
/// request. Keeping the shape here means one place gets it wrong or right.
pub fn tools_call_envelope(inner: &Value) -> Value {
    serde_json::json!({ "content": [{ "type": "text", "text": inner.to_string() }] })
}

/// Sugar for a handler that answers the same value to every call.
pub fn always(result: Value) -> impl Fn(&str, Value) -> MockFuture + Send + Sync + 'static {
    move |_method, _params| {
        let result = result.clone();
        Box::pin(async move { Ok(result) })
    }
}
