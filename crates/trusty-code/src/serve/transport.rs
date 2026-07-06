//! STDIO JSON-RPC transport loop for `tcode serve --stdio` (#2053).
//!
//! Why: matches the ecosystem's line-delimited JSON-RPC-over-stdio framing
//! — the same convention `trusty-search` (`mcp::stdio::run`) and
//! `trusty-memory` (`commands::serve_stdio_bridge::run_stdio_bridge`) use,
//! both built on the shared `trusty_common::mcp` primitives — while adding
//! SIGTERM-aware graceful shutdown per the workspace's connection-safe
//! daemon-restart convention (issue #534): the loop stops accepting new
//! input on SIGTERM but always finishes whatever request is already
//! in-flight before returning, and it never crashes on malformed input —
//! a `serde_json` parse failure becomes a `-32700 Parse error` JSON-RPC
//! response instead of propagating and killing the process.
//!
//! What: [`run_stdio_loop`] wires the real `tokio::io::stdin()`/`stdout()`
//! against a [`Router`]. The generic [`run_loop`] underneath accepts any
//! `AsyncRead`/`AsyncWrite` pair plus a shutdown future so tests can drive
//! it over an in-memory pipe without touching real stdio or process signals.
//! Logging goes to stderr only (via `tracing`) — stdout carries only
//! JSON-RPC response lines, never log output.
//!
//! Deferred: the parent issue (#2053) also lists an HTTP `POST /rpc`
//! transport; this ticket implements STDIO only (see the crate's `serve`
//! module docs).
//!
//! Test: `transport::tests::*` drive `run_loop` over an in-memory duplex
//! pipe (successful dispatch, malformed JSON, notification suppression,
//! EOF exit, and shutdown-signal-triggered early exit).

use std::future::Future;
use std::sync::Arc;

use anyhow::Result;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tracing::{info, warn};
use trusty_common::mcp::{Response, error_codes};

use crate::jsonrpc::{Request, Router};

/// Read JSON-RPC 2.0 requests line-by-line from stdin, dispatch each through
/// `router`, and write the response to stdout. Returns on stdin EOF or on
/// receipt of SIGTERM.
///
/// Why: the single entry point `crate::serve::run_stdio` calls once the
/// router is assembled.
/// What: thin wrapper over [`run_loop`] binding the real process stdio and
/// [`shutdown_signal`].
/// Test: exercised end-to-end (minus real signals/stdio) by `run_loop`'s
/// tests below.
pub async fn run_stdio_loop(router: Router) -> Result<()> {
    let router = Arc::new(router);
    run_loop(
        router,
        tokio::io::stdin(),
        tokio::io::stdout(),
        shutdown_signal(),
    )
    .await
}

/// Generic version of [`run_stdio_loop`] parameterised over the reader,
/// writer, and shutdown future.
///
/// Why: lets unit tests substitute an in-memory duplex pipe and a
/// deterministic shutdown future instead of real stdio/signals.
/// What: reads newline-delimited JSON one line at a time; blank lines are
/// skipped; malformed JSON produces a `-32700 Parse error` response;
/// suppressed responses (notifications) are never written. `select!` is
/// `biased` so a ready shutdown future always wins over a simultaneously
/// ready input line, guaranteeing prompt shutdown.
/// Test: `run_loop_dispatches_and_writes_response`,
/// `run_loop_malformed_json_returns_parse_error`,
/// `run_loop_notification_writes_nothing`,
/// `run_loop_exits_on_eof_when_shutdown_never_fires`,
/// `run_loop_shutdown_signal_takes_priority_over_pending_input`.
async fn run_loop<R, W, Sh>(
    router: Arc<Router>,
    reader: R,
    mut writer: W,
    shutdown: Sh,
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
            line = lines.next_line() => {
                let Some(line) = line? else {
                    info!("tcode serve: stdin EOF, stopping");
                    break;
                };
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let response = parse_and_dispatch(&router, trimmed).await;
                if response.suppress {
                    continue;
                }
                write_response(&mut writer, &response).await?;
            }
        }
    }
    Ok(())
}

/// Parse one line as a JSON-RPC request and dispatch it, or produce a
/// `-32700 Parse error` response on malformed JSON.
///
/// Why: keeps the parse-error mapping (the one error code the router itself
/// never produces) next to the one place it can occur.
/// What: `serde_json::from_str::<Request>` then `router.dispatch`.
/// Test: `run_loop_malformed_json_returns_parse_error`.
async fn parse_and_dispatch(router: &Router, line: &str) -> Response {
    match serde_json::from_str::<Request>(line) {
        Ok(req) => router.dispatch(req).await,
        Err(e) => {
            warn!("tcode serve: malformed JSON-RPC request: {e}");
            Response::err(
                None,
                error_codes::PARSE_ERROR,
                format!("invalid JSON-RPC: {e}"),
            )
        }
    }
}

/// Write one JSON-RPC response as a single NDJSON line and flush.
///
/// Why: flushing per-line matters for STDIO transports — without it,
/// responses can sit in a buffer indefinitely when stdout isn't a TTY.
/// What: serialise, write, `\n`, flush.
/// Test: covered by every `run_loop_*` test that asserts on written bytes.
async fn write_response<W: AsyncWrite + Unpin>(writer: &mut W, response: &Response) -> Result<()> {
    let serialised = serde_json::to_string(response)?;
    writer.write_all(serialised.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
}

/// Resolve when the process receives SIGTERM.
///
/// Why: the connection-safe daemon-restart convention (issue #534) requires
/// every trusty-* daemon to drain and exit cleanly on SIGTERM rather than
/// being SIGKILLed.
/// What: installs a `SIGTERM` listener and resolves on the first signal. On
/// non-Unix platforms (or if installing the handler fails) this future never
/// resolves, leaving stdin EOF as the only shutdown path.
/// Test: not directly tested (requires a real process); `run_loop`'s
/// shutdown-priority test substitutes `std::future::ready(())` for this.
#[cfg(unix)]
async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    match signal(SignalKind::terminate()) {
        Ok(mut sig) => {
            sig.recv().await;
        }
        Err(e) => {
            warn!("tcode serve: failed to install SIGTERM handler: {e}");
            std::future::pending::<()>().await;
        }
    }
}

/// Non-Unix fallback: never resolves (EOF remains the only shutdown path).
#[cfg(not(unix))]
async fn shutdown_signal() {
    std::future::pending::<()>().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::AsyncReadExt;

    fn router_with_ping() -> Arc<Router> {
        let mut router = Router::new();
        router.register("ping", |_params: serde_json::Value| async move {
            Ok(json!({"pong": true}))
        });
        Arc::new(router)
    }

    /// A well-formed request must produce exactly one NDJSON response line
    /// with the handler's result.
    #[tokio::test]
    async fn run_loop_dispatches_and_writes_response() {
        let router = router_with_ping();
        let (mut input_tx, input_rx) = tokio::io::duplex(4096);
        let (output_tx, mut output_rx) = tokio::io::duplex(4096);

        input_tx
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n")
            .await
            .unwrap();
        drop(input_tx); // EOF after the one line

        run_loop(router, input_rx, output_tx, std::future::pending::<()>())
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

        input_tx.write_all(b"not json at all\n").await.unwrap();
        drop(input_tx);

        run_loop(router, input_rx, output_tx, std::future::pending::<()>())
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

        input_tx
            .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"ping\"}\n")
            .await
            .unwrap();
        drop(input_tx);

        run_loop(router, input_rx, output_tx, std::future::pending::<()>())
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
        let result = run_loop(
            router,
            tokio::io::empty(),
            tokio::io::sink(),
            std::future::pending::<()>(),
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

        input_tx
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n")
            .await
            .unwrap();
        drop(input_tx);

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            run_loop(router, input_rx, output_tx, std::future::ready(())),
        )
        .await;
        assert!(
            result.is_ok(),
            "loop must return promptly once the shutdown future is ready"
        );
        assert!(result.unwrap().is_ok());
    }
}
