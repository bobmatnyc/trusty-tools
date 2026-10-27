//! The CLI's reader for `mpm.managed.prune_worktrees_stream` (#7619 PR 2).
//!
//! Why: the unary prune request printed nothing until the daemon answered,
//! minutes later, and a killed CLI left the pass deleting. The stream shows
//! each worktree as the pass reaches it, and hanging up stops the pass.
//! What: [`stream_prune`] sends one request body over the daemon socket and
//! prints each progress and keepalive item through a caller-supplied sink. It
//! ends with the unary reply body, a detach acknowledgement, or
//! [`StreamReply::Unsupported`] when the daemon predates the method, so the
//! caller falls back to the unary request. An interrupt drops the socket,
//! which the daemon reads as cancel.
//! Test: `worktree_7619_the_plain_prune_prints_a_line_per_worktree`,
//! `worktree_7619_a_daemon_without_the_stream_falls_back_to_unary`,
//! `worktree_7619_a_stream_without_its_result_is_an_error`,
//! `worktree_7619_an_interrupt_drops_the_stream_and_says_so`,
//! `worktree_7619_detach_returns_on_the_acknowledgement`.

use std::future::Future;
use std::time::Duration;

use serde_json::{Value, json};
use trusty_common::uds::UdsRpcError;
use trusty_common::uds::server::{CODE_METHOD_NOT_FOUND, CODE_STREAM_UNSUPPORTED, JSONRPC_VERSION};
use trusty_mpm::daemon::managed_routes::prune_stream::PRUNE_STREAM_METHOD;

/// The longest the CLI waits for any one frame (#7619, plan §3.2). The daemon
/// beats every
/// [`KEEPALIVE`](trusty_mpm::daemon::managed_routes::prune_stream::KEEPALIVE),
/// so this many seconds of silence is a stall.
pub(crate) const IDLE_BUDGET: Duration = Duration::from_secs(120);

/// The bound on the dial and the request write.
const OPEN_TIMEOUT: Duration = Duration::from_secs(10);

/// How one streamed prune request ended.
#[derive(Debug)]
pub(crate) enum StreamReply {
    /// The pass finished; the unary method's reply body.
    Body(Value),
    /// The daemon accepted `detach` and runs the pass without this client.
    Detached,
    /// The daemon or client cannot stream this call; send the unary request.
    Unsupported(String),
}

/// Stream one prune-worktrees request, printing progress through `line`.
///
/// Why: Fail-Open Check (#7619 d2). A stream that ends without its result is
/// an error, never an empty success, and only a refusal the daemon sends
/// before any item reads as "does not stream".
/// What: `body` is the unary request body; `detach` asks the daemon to keep
/// going without this client and returns once it says so. `interrupt` firing
/// drops the socket and returns an error saying what the daemon does next.
/// Test: see the module header.
pub(crate) async fn stream_prune(
    daemon: &trusty_mpm::client::DaemonClient,
    mut body: Value,
    detach: bool,
    interrupt: impl Future<Output = ()>,
    mut line: impl FnMut(String),
) -> anyhow::Result<StreamReply> {
    let Some(socket) = daemon.socket_path() else {
        return Ok(StreamReply::Unsupported(
            "the daemon client is not on the socket".into(),
        ));
    };
    body["detach"] = json!(detach);
    let frame = json!({
        "jsonrpc": JSONRPC_VERSION,
        "id": 1,
        "stream": true,
        "method": PRUNE_STREAM_METHOD,
        "params": body,
    });
    let mut stream = trusty_common::uds::send_framed_stream_request_with::<_, Value>(
        socket,
        &frame,
        OPEN_TIMEOUT,
        IDLE_BUDGET,
    )
    .await
    .map_err(|e| anyhow::anyhow!("prune-worktrees: could not open the daemon stream: {e}"))?;
    tokio::pin!(interrupt);
    let mut seen_item = false;
    loop {
        let next = tokio::select! {
            next = stream.next_frame() => next,
            () = &mut interrupt => {
                drop(stream);
                anyhow::bail!(
                    "prune-worktrees interrupted (#7619): the daemon stops before its next \
                     worktree, and a removal in flight finishes. Re-run without `--force` to \
                     see what remains."
                );
            }
        };
        let item = match next {
            Some(Ok(item)) => item,
            Some(Err(e)) if !seen_item => {
                if let Some(why) = unsupported(&e) {
                    return Ok(StreamReply::Unsupported(why));
                }
                return Err(stream_error(e));
            }
            Some(Err(e)) => return Err(stream_error(e)),
            None => anyhow::bail!(
                "prune-worktrees: the daemon's stream ended without its result (#7619). \
                 Nothing is reported as removed; re-run without `--force` to see what remains."
            ),
        };
        seen_item = true;
        match item.get("kind").and_then(Value::as_str) {
            Some("progress") => {
                if let Some(text) = item.get("line").and_then(Value::as_str) {
                    line(text.to_string());
                }
            }
            Some("keepalive") => {
                let elapsed = item.get("elapsed_s").and_then(Value::as_u64).unwrap_or(0);
                line(format!(
                    "prune-worktrees: still working — {elapsed}s elapsed (#7619)"
                ));
            }
            Some("detached") if detach => return Ok(StreamReply::Detached),
            Some("result") => {
                return Ok(StreamReply::Body(
                    item.get("body").cloned().unwrap_or(Value::Null),
                ));
            }
            // A newer daemon's item kind: nothing to show, keep reading.
            _ => {}
        }
    }
}

/// Why a stream refusal means "this daemon does not stream the method".
///
/// A daemon serving other streams answers [`CODE_STREAM_UNSUPPORTED`]; one
/// serving none answers a plain response, usually [`CODE_METHOD_NOT_FOUND`].
fn unsupported(e: &UdsRpcError) -> Option<String> {
    match e {
        UdsRpcError::Stream { error, .. }
            if error.code == CODE_STREAM_UNSUPPORTED || error.code == CODE_METHOD_NOT_FOUND =>
        {
            Some(error.message.clone())
        }
        UdsRpcError::NotAStream { response, .. } => Some(response.error.as_ref().map_or_else(
            || "a non-streamed answer".to_string(),
            |e| e.message.clone(),
        )),
        _ => None,
    }
}

/// A stream failure as the operator's error: the daemon's own refusal text,
/// or the transport fault with what it means for removals.
fn stream_error(e: UdsRpcError) -> anyhow::Error {
    match e {
        UdsRpcError::Stream { error, .. } => anyhow::anyhow!("{}", error.message),
        UdsRpcError::Timeout { timeout, .. } => anyhow::anyhow!(
            "prune-worktrees: the daemon sent nothing for {}s (#7619). Nothing is reported as \
             removed; re-run without `--force` to see what remains.",
            timeout.as_secs()
        ),
        other => anyhow::anyhow!("prune-worktrees: the daemon stream failed: {other}"),
    }
}

/// Send one prune request, streamed while the daemon supports it (#7619).
///
/// Why: one place decides between the stream and the unary fallback, so the
/// preview and the `--force` request cannot pick different transports.
/// What: while `*streaming` holds, streams `payload` and prints each line to
/// stderr. A daemon that cannot stream clears `*streaming`, says so on
/// stderr, and `unary` sends the request instead. `Ok(None)` is a detached
/// pass. A detach request without the stream is refused before `unary` runs,
/// because the unary request cannot report that the daemon kept going.
/// Test: `worktree_7619_a_daemon_without_the_stream_falls_back_to_unary`,
/// `worktree_7619_detach_without_the_stream_sends_nothing`.
pub(crate) async fn send_prune<F, Fut>(
    daemon: &trusty_mpm::client::DaemonClient,
    payload: Value,
    detach: bool,
    streaming: &mut bool,
    unary: F,
) -> anyhow::Result<Option<Value>>
where
    F: FnOnce(Value) -> Fut,
    Fut: Future<Output = anyhow::Result<Value>>,
{
    if *streaming {
        match stream_prune(daemon, payload.clone(), detach, ctrl_c(), |l| {
            eprintln!("{l}")
        })
        .await?
        {
            StreamReply::Body(body) => return Ok(Some(body)),
            StreamReply::Detached => return Ok(None),
            StreamReply::Unsupported(why) => {
                *streaming = false;
                eprintln!(
                    "prune-worktrees: the daemon does not stream this call ({why}); sending the \
                     unary request — no per-worktree progress, and Ctrl-C ends this command, \
                     not the daemon's pass (#7619)"
                );
            }
        }
    }
    if detach {
        anyhow::bail!(
            "--detach needs a daemon serving `{PRUNE_STREAM_METHOD}` (#7619); no removal \
             request was sent. Re-run without `--detach`."
        );
    }
    unary(payload).await.map(Some)
}

/// The interrupt the CLI listens for: Ctrl-C.
///
/// A handler that cannot be installed never fires, so the default SIGINT
/// still ends the process — which closes the socket, the same cancel.
pub(crate) async fn ctrl_c() {
    if tokio::signal::ctrl_c().await.is_err() {
        std::future::pending::<()>().await;
    }
}

#[cfg(test)]
#[path = "prune_stream_client_tests.rs"]
mod prune_stream_client_tests;
