//! STDIO JSON-RPC transport loop for `tcode serve --stdio` (#2053, #2054).
//!
//! Why: matches the ecosystem's line-delimited JSON-RPC-over-stdio framing
//! — the same convention `trusty-search` (`mcp::stdio::run`) and
//! `trusty-memory` (`commands::serve_stdio_bridge::run_stdio_bridge`) use,
//! both built on the shared `trusty_common::mcp` primitives — while adding
//! graceful shutdown per the workspace's connection-safe daemon-restart
//! convention (issue #534): the loop stops accepting new input on SIGTERM/
//! SIGINT but always finishes whatever request is already in-flight before
//! returning, and it never crashes on malformed input — a `serde_json`
//! parse failure becomes a `-32700 Parse error` JSON-RPC response instead of
//! propagating and killing the process. Shutdown detection itself is the
//! shared `trusty_common::shutdown_signal()` helper — the exact function
//! `trusty-memory`'s `run_http_on` and `trusty-search`'s HTTP daemon install
//! via `axum::serve(...).with_graceful_shutdown(...)` (see
//! `crate::serve::http::run_http`) — rather than a second, STDIO-specific
//! signal handler.
//!
//! #2054 adds a THIRD `select!` arm: STDIO is a single long-lived
//! connection for the whole process lifetime, so one
//! [`ConnectionContext`](crate::jsonrpc::ConnectionContext) is built once
//! per process and its notify receiver is polled alongside input lines and
//! the shutdown signal. Whenever `session.attach`'s background forwarder
//! (`crate::session::registry`) pushes a server-initiated notification, it
//! is written to stdout interleaved with ordinary request/response lines —
//! exactly the "stdout: JSON-RPC 2.0 responses + event notifications
//! (NDJSON)" wire model the vision spec's §4.2 describes.
//!
//! What: [`run_stdio_loop`] wires the real `tokio::io::stdin()`/`stdout()`
//! against a [`Router`]. The generic [`run_loop`] underneath accepts any
//! `AsyncRead`/`AsyncWrite` pair plus a shutdown future so tests can drive
//! it over an in-memory pipe without touching real stdio or process signals.
//! Logging goes to stderr only (via `tracing`) — stdout carries only
//! JSON-RPC response/notification lines, never log output.
//!
//! Test: `transport::tests::*` drive `run_loop` over an in-memory duplex
//! pipe (successful dispatch, malformed JSON, notification suppression,
//! EOF exit, shutdown-signal-triggered early exit, and a server-initiated
//! notification pushed via the connection's notify channel).

use std::future::Future;
use std::sync::Arc;

use anyhow::Result;
use serde::Serialize;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tracing::info;

use crate::jsonrpc::{ConnectionContext, Router};

/// Read JSON-RPC 2.0 requests line-by-line from stdin, dispatch each through
/// `router`, and write the response to stdout — interleaved with any
/// server-initiated notifications pushed via this connection's
/// [`ConnectionContext`]. Returns on stdin EOF or on receipt of SIGTERM/
/// SIGINT.
///
/// Why: the single entry point `crate::serve::run_stdio` calls once the
/// router is assembled.
/// What: thin wrapper over [`run_loop`] binding the real process stdio, a
/// fresh long-lived [`ConnectionContext`], and the shared
/// `trusty_common::shutdown_signal()`.
/// Test: exercised end-to-end (minus real signals/stdio) by `run_loop`'s
/// tests below.
pub async fn run_stdio_loop(router: Router) -> Result<()> {
    let router = Arc::new(router);
    let (notify_tx, notify_rx) = mpsc::unbounded_channel();
    let ctx = ConnectionContext::new(notify_tx);
    run_loop(
        router,
        tokio::io::stdin(),
        tokio::io::stdout(),
        trusty_common::shutdown_signal(),
        ctx,
        notify_rx,
    )
    .await
}

/// Generic version of [`run_stdio_loop`] parameterised over the reader,
/// writer, shutdown future, and connection context.
///
/// Why: lets unit tests substitute an in-memory duplex pipe, a
/// deterministic shutdown future, and drive the notify channel directly
/// instead of real stdio/signals/a spawned forwarder.
/// What: reads newline-delimited JSON one line at a time; blank lines are
/// skipped; malformed JSON produces a `-32700 Parse error` response;
/// suppressed responses (notifications received AS a request, e.g. a
/// client-sent notification) are never written. A third `select!` arm
/// drains `notify_rx` and writes whatever arrives (already a complete
/// JSON-RPC object — a response or a server-initiated notification)
/// verbatim. `select!` is `biased` so shutdown always wins a simultaneous
/// race against input or a queued notification, guaranteeing prompt
/// shutdown.
/// Test: `run_loop_dispatches_and_writes_response`,
/// `run_loop_malformed_json_returns_parse_error`,
/// `run_loop_notification_writes_nothing`,
/// `run_loop_exits_on_eof_when_shutdown_never_fires`,
/// `run_loop_shutdown_signal_takes_priority_over_pending_input`,
/// `run_loop_forwards_queued_notifications_to_the_wire`.
#[allow(clippy::too_many_arguments)]
async fn run_loop<R, W, Sh>(
    router: Arc<Router>,
    reader: R,
    mut writer: W,
    shutdown: Sh,
    ctx: ConnectionContext,
    mut notify_rx: mpsc::UnboundedReceiver<serde_json::Value>,
) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
    Sh: Future<Output = ()>,
{
    tokio::pin!(shutdown);
    let mut lines = BufReader::new(reader).lines();

    loop {
        tokio::select! {
            biased;
            () = &mut shutdown => {
                info!("tcode serve: shutdown signal received, stopping");
                break;
            }
            notification = notify_rx.recv() => {
                let Some(notification) = notification else {
                    // All senders dropped (should not happen while `ctx` is
                    // alive, but treat it the same as "nothing more to send"
                    // rather than erroring).
                    continue;
                };
                write_json_line(&mut writer, &notification).await?;
            }
            line = lines.next_line() => {
                let Some(line) = line? else {
                    info!("tcode serve: stdin EOF, stopping");
                    break;
                };
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let response = router.dispatch_json(trimmed.as_bytes(), &ctx).await;
                if response.suppress {
                    continue;
                }
                write_json_line(&mut writer, &response).await?;
            }
        }
    }
    Ok(())
}

/// Write one JSON-serialisable value as a single NDJSON line and flush.
///
/// Why: both ordinary `Response`s and server-initiated notifications
/// (plain `serde_json::Value`) go out over the same stdout stream — this is
/// the one place that owns "serialise, newline, flush" for either shape.
/// Flushing per-line matters for STDIO transports — without it, lines can
/// sit in a buffer indefinitely when stdout isn't a TTY.
/// What: serialise, write, `\n`, flush.
/// Test: covered by every `run_loop_*` test that asserts on written bytes.
async fn write_json_line<W: AsyncWrite + Unpin, T: Serialize>(
    writer: &mut W,
    value: &T,
) -> Result<()> {
    let serialised = serde_json::to_string(value)?;
    writer.write_all(serialised.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::AsyncReadExt;
    use trusty_common::mcp::error_codes;

    fn router_with_ping() -> Arc<Router> {
        let mut router = Router::new();
        router.register(
            "ping",
            |_params: serde_json::Value, _ctx: ConnectionContext| async move {
                Ok(json!({"pong": true}))
            },
        );
        Arc::new(router)
    }

    /// Build a throwaway context + a pre-drained notify receiver, for tests
    /// that don't exercise the notification path.
    fn test_ctx() -> (
        ConnectionContext,
        mpsc::UnboundedReceiver<serde_json::Value>,
    ) {
        let (tx, rx) = mpsc::unbounded_channel();
        (ConnectionContext::new(tx), rx)
    }

    /// A well-formed request must produce exactly one NDJSON response line
    /// with the handler's result.
    #[tokio::test]
    async fn run_loop_dispatches_and_writes_response() {
        let router = router_with_ping();
        let (mut input_tx, input_rx) = tokio::io::duplex(4096);
        let (output_tx, mut output_rx) = tokio::io::duplex(4096);
        let (ctx, notify_rx) = test_ctx();

        input_tx
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n")
            .await
            .unwrap();
        drop(input_tx); // EOF after the one line

        run_loop(
            router,
            input_rx,
            output_tx,
            std::future::pending::<()>(),
            ctx,
            notify_rx,
        )
        .await
        .expect("loop must return Ok on EOF");

        let mut out = Vec::new();
        output_rx.read_to_end(&mut out).await.unwrap();
        let line = String::from_utf8(out).unwrap();
        let resp: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(resp["result"], json!({"pong": true}));
        assert_eq!(resp["id"], 1);
    }

    /// Malformed JSON must produce a `-32700 Parse error` response, not a
    /// crash.
    #[tokio::test]
    async fn run_loop_malformed_json_returns_parse_error() {
        let router = router_with_ping();
        let (mut input_tx, input_rx) = tokio::io::duplex(4096);
        let (output_tx, mut output_rx) = tokio::io::duplex(4096);
        let (ctx, notify_rx) = test_ctx();

        input_tx.write_all(b"not json at all\n").await.unwrap();
        drop(input_tx);

        run_loop(
            router,
            input_rx,
            output_tx,
            std::future::pending::<()>(),
            ctx,
            notify_rx,
        )
        .await
        .expect("loop must return Ok on EOF even after a parse error");

        let mut out = Vec::new();
        output_rx.read_to_end(&mut out).await.unwrap();
        let resp: serde_json::Value =
            serde_json::from_str(String::from_utf8(out).unwrap().trim()).unwrap();
        assert_eq!(resp["error"]["code"], error_codes::PARSE_ERROR);
    }

    /// A notification (no `id`) must produce no output line at all.
    #[tokio::test]
    async fn run_loop_notification_writes_nothing() {
        let router = router_with_ping();
        let (mut input_tx, input_rx) = tokio::io::duplex(4096);
        let (output_tx, mut output_rx) = tokio::io::duplex(4096);
        let (ctx, notify_rx) = test_ctx();

        input_tx
            .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"ping\"}\n")
            .await
            .unwrap();
        drop(input_tx);

        run_loop(
            router,
            input_rx,
            output_tx,
            std::future::pending::<()>(),
            ctx,
            notify_rx,
        )
        .await
        .expect("loop must return Ok on EOF");

        let mut out = Vec::new();
        output_rx.read_to_end(&mut out).await.unwrap();
        assert!(
            out.is_empty(),
            "a notification must not produce a response line"
        );
    }

    /// EOF with a shutdown future that never fires must still return `Ok`.
    #[tokio::test]
    async fn run_loop_exits_on_eof_when_shutdown_never_fires() {
        let router = router_with_ping();
        let (ctx, notify_rx) = test_ctx();
        let result = run_loop(
            router,
            tokio::io::empty(),
            tokio::io::sink(),
            std::future::pending::<()>(),
            ctx,
            notify_rx,
        )
        .await;
        assert!(result.is_ok(), "loop must return Ok on EOF: {result:?}");
    }

    /// When the shutdown future is already ready, it must take priority
    /// over a simultaneously-ready input line (biased `select!`), so the
    /// loop returns promptly instead of draining all remaining input.
    #[tokio::test]
    async fn run_loop_shutdown_signal_takes_priority_over_pending_input() {
        let router = router_with_ping();
        let (mut input_tx, input_rx) = tokio::io::duplex(4096);
        let (output_tx, _output_rx) = tokio::io::duplex(4096);
        let (ctx, notify_rx) = test_ctx();

        input_tx
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n")
            .await
            .unwrap();
        drop(input_tx);

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            run_loop(
                router,
                input_rx,
                output_tx,
                std::future::ready(()),
                ctx,
                notify_rx,
            ),
        )
        .await;
        assert!(
            result.is_ok(),
            "loop must return promptly once the shutdown future is ready"
        );
        assert!(result.unwrap().is_ok());
    }

    /// A value pushed onto the connection's notify channel (as
    /// `session.attach`'s forwarder does) must be written to the wire
    /// verbatim, interleaved with ordinary request/response traffic.
    #[tokio::test]
    async fn run_loop_forwards_queued_notifications_to_the_wire() {
        let router = router_with_ping();
        let (_input_tx, input_rx) = tokio::io::duplex(4096);
        let (output_tx, mut output_rx) = tokio::io::duplex(4096);
        let (tx, rx) = mpsc::unbounded_channel();
        let ctx = ConnectionContext::new(tx);

        let notification = json!({
            "jsonrpc": "2.0",
            "method": "session.event",
            "params": {"session_id": "s-1", "event": {"type": "session_input", "input": "hi"}},
        });
        ctx.notify.send(notification.clone()).unwrap();

        // Run the loop with a shutdown that fires shortly after — enough
        // time for the notify arm to win a `select!` iteration and write
        // the queued line, but bounded so the test can't hang if it doesn't.
        let shutdown = async { tokio::time::sleep(std::time::Duration::from_millis(100)).await };
        run_loop(router, input_rx, output_tx, shutdown, ctx, rx)
            .await
            .expect("loop must return Ok once shutdown fires");

        let mut out = Vec::new();
        output_rx.read_to_end(&mut out).await.unwrap();
        let line: serde_json::Value =
            serde_json::from_str(String::from_utf8(out).unwrap().trim()).unwrap();
        assert_eq!(line, notification);
    }
}
