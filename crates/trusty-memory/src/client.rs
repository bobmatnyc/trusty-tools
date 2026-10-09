//! The one way anything in this crate calls the running daemon (#6286).
//!
//! Why: six call sites inside trusty-memory dialled the daemon over HTTP —
//! the `UserPromptSubmit` hook, the `SessionStart` inbox check, the
//! `send-message` CLI, `doctor`'s tier-S probe, the daemon guard, and the MCP
//! stdio bridge. Each one read `read_daemon_addr("trusty-memory")`, normalised
//! a bare `host:port` into a URL, built its own `reqwest::Client` with its own
//! timeout, and appended its own path. Six copies of address resolution is what
//! the workspace's common-entry-point rule exists to prevent, and ADR-0032
//! required touching all six anyway.
//!
//! What: [`call`] dials the derived socket, writes one JSON-RPC frame, reads
//! one back, and returns the `result` — or an error carrying the daemon's own
//! message. There is no address to resolve and no client to build: the socket
//! path is derived by `trusty_common::daemon_socket_path`, so caller and daemon
//! compute the same one and nothing publishes it.
//!
//! **This is the request/response half only.** `memory.chat` answers in many
//! frames; a caller that wants it uses
//! `trusty_common::uds::send_framed_stream_request_capped` directly, because a
//! stream is not something this signature can return.
//!
//! Test: `client_reports_a_dead_socket_rather_than_hanging`.

use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, Context as _, Result};
use serde_json::{json, Value};
use trusty_common::uds::send_framed_request_capped;
use trusty_common::uds::server::RpcResponse;

use crate::transport::uds::MAX_FRAME_BYTES;

/// Default budget for one call.
///
/// Why 60 seconds: it is the figure the stdio bridge's `reqwest` client used,
/// and it is sized for a cold-start embedding rather than for a local
/// round-trip. A hook that wants to fail fast passes its own (the
/// prompt-context hook's fail-open deadline is under a second).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// Call one method on the running daemon and return its `result`.
///
/// # Errors
///
/// When the socket cannot be resolved or dialled — which is what "the daemon is
/// not running" looks like — or when the daemon answers with a JSON-RPC error,
/// whose message and code are carried through verbatim so the caller reports
/// the reason it was given rather than a generic failure.
///
/// Test: `client_reports_a_dead_socket_rather_than_hanging`.
pub async fn call(method: &str, params: Value) -> Result<Value> {
    call_with_timeout(method, params, DEFAULT_TIMEOUT).await
}

/// [`call`] with an explicit budget, for a caller that must fail fast.
///
/// # Errors
///
/// As [`call`].
pub async fn call_with_timeout(method: &str, params: Value, timeout: Duration) -> Result<Value> {
    let socket =
        crate::transport::uds::socket_path().context("resolve the trusty-memory socket path")?;
    call_at(&socket, method, params, timeout).await
}

/// [`call_with_timeout`] against an explicit socket.
///
/// Why it is public: a test drives a daemon on a temp socket, and a hook fixture
/// that had to mutate the real data directory to be testable would not be a
/// test of the hook.
///
/// #9288: runs the shared protocol check first, so a daemon from another
/// release is refused with `MemoryProtocolError` instead of misparsed. A daemon
/// that predates the handshake is called, per
/// `trusty_common::memory_rpc::check_memory_protocol_at`.
///
/// # Errors
///
/// As [`call`], plus a `MemoryProtocolError` for a daemon this build cannot
/// talk to or whose protocol check failed.
///
/// Test: `client_refuses_an_unsupported_daemon_before_calling_it`,
/// `client_calls_a_daemon_that_predates_the_handshake`.
pub async fn call_at(
    socket: &Path,
    method: &str,
    params: Value,
    timeout: Duration,
) -> Result<Value> {
    // #9288: bounded by the shared default so a slow handshake cannot eat a
    // long call's whole budget.
    trusty_common::memory_rpc::ensure_memory_protocol_at(
        socket,
        timeout.min(trusty_common::memory_rpc::DEFAULT_TIMEOUT),
    )
    .await?;

    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    });

    let response: RpcResponse =
        send_framed_request_capped(socket, &request, timeout, MAX_FRAME_BYTES)
            .await
            .with_context(|| {
                format!(
                    "call {method} on the trusty-memory daemon at {}",
                    socket.display()
                )
            })?;

    match (response.result, response.error) {
        (Some(result), _) => Ok(result),
        (None, Some(e)) => Err(anyhow!("{method} failed: {} ({})", e.message, e.code)),
        // The daemon's own contract is that exactly one of the two is present.
        (None, None) => Err(anyhow!(
            "{method} answered with neither a result nor an error"
        )),
    }
}

/// Is anything serving the daemon's socket?
///
/// Why a bare connect rather than a `memory.health` call: the question is
/// whether the endpoint is live. A daemon that is up but degraded must not be
/// reported absent and then spawned on top of itself.
pub async fn is_running() -> bool {
    let Ok(socket) = crate::transport::uds::socket_path() else {
        return false;
    };
    trusty_common::uds::socket_is_serving(&socket, Duration::from_millis(500)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: every hook and CLI path above degrades on "the daemon is not
    /// running", and it has to learn that promptly rather than by waiting out a
    /// timeout. A dial against an absent socket is refused by the kernel, so
    /// the error must arrive well inside the budget.
    /// Test: itself.
    #[tokio::test]
    async fn client_reports_a_dead_socket_rather_than_hanging() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let started = std::time::Instant::now();
        let result = call_at(
            &tmp.path().join("absent.sock"),
            "memory.status",
            json!({}),
            Duration::from_secs(30),
        )
        .await;
        assert!(result.is_err(), "an absent socket cannot answer");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "a refused dial must not wait out the budget: {:?}",
            started.elapsed()
        );
    }

    /// Serve `router` on a temp socket until the returned sender drops.
    fn serve_fake(
        router: trusty_common::uds::server::RpcRouter,
    ) -> (
        tempfile::TempDir,
        std::path::PathBuf,
        tokio::sync::oneshot::Sender<()>,
    ) {
        use trusty_common::uds::server::{serve_until, RpcServeOptions};
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("fake-memory.sock");
        let listener = trusty_common::uds::bind_hardened(&socket).expect("bind");
        let (stop, shutdown) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            serve_until(
                &listener,
                std::sync::Arc::new(router),
                RpcServeOptions::default(),
                async {
                    let _ = shutdown.await;
                },
            )
            .await;
        });
        (dir, socket, stop)
    }

    /// Why (#9288): the CLI and hooks must refuse a daemon from another release
    /// by name, before the call whose reply they would misparse is sent.
    /// What: a fake daemon reports a protocol past this build's range and
    /// counts `memory.status` calls; the client must refuse and send none.
    /// Test: itself.
    #[tokio::test]
    async fn client_refuses_an_unsupported_daemon_before_calling_it() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        use trusty_common::memory_rpc::{
            MemoryProtocolError, METHOD_PROTOCOL, SUPPORTED_MEMORY_PROTOCOLS,
        };

        let calls = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&calls);
        let newer = SUPPORTED_MEMORY_PROTOCOLS.end() + 1;
        let router = trusty_common::uds::server::RpcRouter::new()
            .typed::<Value, Value, _, _>(METHOD_PROTOCOL, move |_| async move {
                Ok(json!({ "protocol_version": newer }))
            })
            .typed::<Value, Value, _, _>("memory.status", move |_| {
                counted.fetch_add(1, Ordering::SeqCst);
                async { Ok(json!({ "ok": true })) }
            });
        let (_dir, socket, _stop) = serve_fake(router);

        let err = call_at(&socket, "memory.status", json!({}), Duration::from_secs(10))
            .await
            .expect_err("an unsupported daemon is refused");

        assert!(
            matches!(
                err.downcast_ref::<MemoryProtocolError>(),
                Some(MemoryProtocolError::Unsupported { daemon, .. }) if *daemon == newer
            ),
            "expected the named protocol error, got {err:#}"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0, "the call must not be sent");
    }

    /// Why (#9288): during a rolling upgrade the installed client meets a
    /// daemon that predates the handshake; every call must not fail.
    /// Test: itself.
    #[tokio::test]
    async fn client_calls_a_daemon_that_predates_the_handshake() {
        let router = trusty_common::uds::server::RpcRouter::new()
            .typed::<Value, Value, _, _>("memory.status", |_| async { Ok(json!({ "ok": true })) });
        let (_dir, socket, _stop) = serve_fake(router);

        let answer = call_at(&socket, "memory.status", json!({}), Duration::from_secs(10))
            .await
            .expect("a pre-handshake daemon is still called");

        assert_eq!(answer, json!({ "ok": true }));
    }
}
