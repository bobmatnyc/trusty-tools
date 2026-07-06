//! [`StdioRpcClient`]: a JSON-RPC 2.0 client speaking NDJSON over a spawned
//! `tcode serve --stdio` child's stdio pipes (#2060).
//!
//! Why: the CLI needs to drive the daemon's `session.*`/`task.*` methods
//! without reimplementing any of their logic. Spawning an ephemeral child
//! per invocation (rather than discovering/reusing a long-lived daemon) is
//! the simplest robust M1 approach the ticket calls for — no discovery file,
//! no port, no lifecycle management beyond this one process's lifetime.
//! What: [`StdioRpcClient::spawn`] launches the child; [`StdioRpcClient::call`]
//! sends a request and blocks (bounded by [`DEFAULT_CALL_TIMEOUT`]) for its
//! matching response, buffering any interleaved `session.event` notification
//! lines rather than dropping them; [`StdioRpcClient::next_notification`]
//! drains that buffer (or reads fresh lines, bounded by a caller-supplied
//! wait) for the streaming subcommands (`attach`, `run-task`).
//! [`classify_notification`]/[`build_request`] are pure helpers, unit tested
//! here without spawning a real process; the full spawn+wire round trip is
//! covered by `tests/cli_e2e.rs` against the real `tcode` binary.
//! Test: `cli_client::stdio::tests::*`.

use std::collections::VecDeque;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::time::timeout;
use trusty_common::mcp::Request;

use crate::events::SessionEventEnvelope;

/// How long [`StdioRpcClient::call`] waits for a matching response before
/// giving up.
///
/// Why: every method this client calls (`session.*`, `task.run`) is a fast,
/// synchronous handler by design — `task.run` in particular reserves its
/// execution slot and returns immediately rather than blocking on the LLM
/// run (see `task::protocol::task_run`'s docs) — so a real hang here means a
/// genuine daemon-side regression, not a slow-but-legitimate call. Generous
/// enough for a debug build under load, tight enough to fail fast.
pub const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(15);

/// Errors a CLI subcommand handler surfaces to the user (mapped onto a
/// nonzero exit code by the caller — this type carries no exit-code opinion
/// itself).
#[derive(Debug, thiserror::Error)]
pub enum RpcClientError {
    /// The child process could not be spawned at all (binary missing, not
    /// executable, etc.).
    #[error("failed to spawn `{0}`: {1}")]
    Spawn(String, #[source] std::io::Error),
    /// The child's stdout closed (process exited) before a full response
    /// arrived.
    #[error("daemon connection closed unexpectedly")]
    ConnectionClosed,
    /// A read/write against the child's stdio pipes failed.
    #[error("failed to read from daemon: {0}")]
    Io(#[source] std::io::Error),
    /// A line on the wire was not valid JSON, or lacked fields a response
    /// must have.
    #[error("malformed response from daemon: {0}")]
    MalformedResponse(String),
    /// The daemon's JSON-RPC error envelope for this call (verbatim: code +
    /// message + optional `data`).
    #[error("daemon returned an error ({code}): {message}")]
    Rpc {
        code: i32,
        message: String,
        data: Option<Value>,
    },
    /// No matching response arrived within [`DEFAULT_CALL_TIMEOUT`].
    #[error("timed out waiting for the daemon to respond")]
    Timeout,
}

/// Build the outgoing JSON-RPC 2.0 request envelope for one call.
///
/// Why: split out so `next_id` bookkeeping and wire-envelope construction
/// are each independently, purely testable — no process I/O involved.
/// What: `jsonrpc: "2.0"`, `id` as a JSON number, `method`/`params` verbatim.
/// Test: `tests::build_request_uses_2_0_and_given_id`.
fn build_request(id: i64, method: &str, params: Value) -> Request {
    Request {
        jsonrpc: Some("2.0".to_string()),
        id: Some(serde_json::json!(id)),
        method: method.to_string(),
        params: Some(params),
    }
}

/// Classify one already-JSON-parsed wire line as a `session.event`
/// notification, if it is one.
///
/// Why: the daemon interleaves request/response traffic with
/// server-initiated `session.event` notifications on the SAME connection
/// (see `session::registry`'s attach-forwarding docs); every line this
/// client reads must be checked against both shapes.
/// What: `Some(envelope)` when `value.method == "session.event"` and
/// `value.params` deserialises as a [`SessionEventEnvelope`]; `None`
/// otherwise (including a shape that claims to be a notification but fails
/// to parse — degrade silently rather than error, matching `Event`'s own
/// "unknown variants are ignored" forward-compatibility stance).
/// Test: `tests::classify_notification_recognises_session_event`,
/// `tests::classify_notification_ignores_other_shapes`.
fn classify_notification(value: &Value) -> Option<SessionEventEnvelope> {
    if value.get("method").and_then(|m| m.as_str()) != Some("session.event") {
        return None;
    }
    serde_json::from_value(value.get("params")?.clone()).ok()
}

/// A JSON-RPC 2.0 client over a spawned `tcode serve --stdio` child.
///
/// Why: see module docs.
/// What: owns the child + its stdin/stdout handles and a monotonic request
/// id counter; buffers notification lines encountered while a `call` is
/// awaiting its own response so nothing observed on the wire is ever
/// silently dropped.
/// Test: `tests::*` for the pure helpers; `tests/cli_e2e.rs` for the real
/// spawn + wire round trip.
pub struct StdioRpcClient {
    child: Child,
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
    next_id: i64,
    pending_notifications: VecDeque<SessionEventEnvelope>,
}

impl StdioRpcClient {
    /// Spawn `<tcode_exe> serve --project <project> --stdio` and return a
    /// client wired to its stdio pipes.
    ///
    /// Why: `tcode_exe` is normally `std::env::current_exe()` — the CLI
    /// re-spawns ITSELF in daemon mode, per the ticket's "in-process spawn"
    /// requirement; passing it explicitly (rather than hardcoding
    /// `current_exe()` inside this function) keeps this module testable
    /// with an arbitrary binary path and avoids a hidden `std::env` read
    /// inside library code.
    /// What: `kill_on_drop(true)` so a CLI process that exits early (panic,
    /// early return) never leaks the child; the child's stderr is
    /// inherited so daemon-side log lines are visible to the operator
    /// without corrupting the stdout NDJSON framing this client reads.
    /// Test: `tests/cli_e2e.rs` (spawns the real binary).
    pub fn spawn(tcode_exe: &Path, project: &Path) -> Result<Self, RpcClientError> {
        let mut child = tokio::process::Command::new(tcode_exe)
            .arg("serve")
            .arg("--project")
            .arg(project)
            .arg("--stdio")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| RpcClientError::Spawn(tcode_exe.display().to_string(), e))?;
        let stdin = child.stdin.take().ok_or(RpcClientError::ConnectionClosed)?;
        let stdout = child
            .stdout
            .take()
            .ok_or(RpcClientError::ConnectionClosed)?;
        Ok(Self {
            child,
            stdin,
            lines: BufReader::new(stdout).lines(),
            next_id: 1,
            pending_notifications: VecDeque::new(),
        })
    }

    /// Call `method` with `params`, waiting for its matching response.
    ///
    /// Why: the request/response half of the protocol every non-streaming
    /// subcommand (`session.list`/`status`/`cancel`/`get_transcript`,
    /// `task.run`'s initial call) uses.
    /// What: writes one NDJSON request line, then reads lines until one
    /// carries the SAME `id` — any `session.event` notification observed
    /// along the way is buffered (not dropped) for a later
    /// [`Self::next_notification`] call. A JSON-RPC error envelope becomes
    /// `Err(RpcClientError::Rpc)`; the whole call is bounded by
    /// [`DEFAULT_CALL_TIMEOUT`].
    /// Test: covered end-to-end by `tests/cli_e2e.rs`.
    pub async fn call(&mut self, method: &str, params: Value) -> Result<Value, RpcClientError> {
        timeout(DEFAULT_CALL_TIMEOUT, self.call_inner(method, params))
            .await
            .map_err(|_| RpcClientError::Timeout)?
    }

    async fn call_inner(&mut self, method: &str, params: Value) -> Result<Value, RpcClientError> {
        let id = self.next_id;
        self.next_id += 1;
        let req = build_request(id, method, params);
        let mut line = serde_json::to_string(&req)
            .map_err(|e| RpcClientError::MalformedResponse(e.to_string()))?;
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .await
            .map_err(RpcClientError::Io)?;
        self.stdin.flush().await.map_err(RpcClientError::Io)?;

        loop {
            let raw = self.read_raw_line().await?;
            let value: Value = serde_json::from_str(&raw)
                .map_err(|e| RpcClientError::MalformedResponse(format!("{e}: {raw}")))?;

            if value.get("id").and_then(Value::as_i64) == Some(id) {
                if let Some(err) = value.get("error").filter(|e| !e.is_null()) {
                    return Err(RpcClientError::Rpc {
                        code: err.get("code").and_then(Value::as_i64).unwrap_or(0) as i32,
                        message: err
                            .get("message")
                            .and_then(|m| m.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        data: err.get("data").cloned(),
                    });
                }
                return Ok(value.get("result").cloned().unwrap_or(Value::Null));
            }

            if let Some(envelope) = classify_notification(&value) {
                self.pending_notifications.push_back(envelope);
            }
            // Any other stray line (e.g. a response to an id we no longer
            // care about) is silently ignored — this client only ever has
            // one call in flight at a time.
        }
    }

    /// Return the next observed `session.event` notification, waiting up to
    /// `wait` for a fresh one if none is already buffered.
    ///
    /// Why: the streaming subcommands (`attach`, `run-task`) poll this in a
    /// loop, printing each envelope as it arrives, until a terminal event or
    /// the user interrupts.
    /// What: drains [`Self::pending_notifications`] first (events observed
    /// while a previous `call` was still awaiting its own response); only
    /// reads fresh lines from the child when the buffer is empty.
    /// `Ok(None)` means "nothing arrived within `wait`" — NOT connection
    /// loss (that is `Err(ConnectionClosed)`) — callers should keep polling.
    /// Test: covered end-to-end by `tests/cli_e2e.rs`.
    pub async fn next_notification(
        &mut self,
        wait: Duration,
    ) -> Result<Option<SessionEventEnvelope>, RpcClientError> {
        if let Some(envelope) = self.pending_notifications.pop_front() {
            return Ok(Some(envelope));
        }
        match timeout(wait, self.read_raw_line()).await {
            Ok(Ok(raw)) => {
                let value: Value = serde_json::from_str(&raw)
                    .map_err(|e| RpcClientError::MalformedResponse(format!("{e}: {raw}")))?;
                Ok(classify_notification(&value))
            }
            Ok(Err(e)) => Err(e),
            Err(_) => Ok(None),
        }
    }

    async fn read_raw_line(&mut self) -> Result<String, RpcClientError> {
        self.lines
            .next_line()
            .await
            .map_err(RpcClientError::Io)?
            .ok_or(RpcClientError::ConnectionClosed)
    }

    /// Close stdin (EOF) and wait, bounded by `grace`, for the child to
    /// exit cleanly; force-kills it if it doesn't.
    ///
    /// Why: mirrors the daemon's own connection-safe-shutdown convention
    /// (issue #534) from the client side — the CLI should give its spawned
    /// child a real chance to drain before the process ends.
    /// What: dropping `stdin` signals EOF (the same trigger
    /// `serve::run_stdio` reacts to); force-kill is best-effort, matching
    /// every other bounded-shutdown path in this crate.
    /// Test: covered end-to-end by `tests/cli_e2e.rs`.
    pub async fn shutdown(mut self, grace: Duration) -> Result<(), RpcClientError> {
        drop(self.stdin);
        match timeout(grace, self.child.wait()).await {
            Ok(Ok(_status)) => Ok(()),
            Ok(Err(e)) => Err(RpcClientError::Io(e)),
            Err(_) => {
                let _ = self.child.start_kill();
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// `build_request` must stamp `jsonrpc: "2.0"` and the exact given id.
    #[test]
    fn build_request_uses_2_0_and_given_id() {
        let req = build_request(7, "session.list", json!({}));
        assert_eq!(req.jsonrpc.as_deref(), Some("2.0"));
        assert_eq!(req.id, Some(json!(7)));
        assert_eq!(req.method, "session.list");
    }

    /// A well-formed `session.event` notification must parse into its
    /// envelope.
    #[test]
    fn classify_notification_recognises_session_event() {
        let line = json!({
            "method": "session.event",
            "params": {
                "session_id": "s-1",
                "seq": 1,
                "at": "2024-01-01T00:00:00Z",
                "kind": "session_started",
                "event": {"type": "session_started", "session_id": "s-1", "project": "p"},
            }
        });
        let envelope = classify_notification(&line).expect("must classify as a notification");
        assert_eq!(envelope.session_id, "s-1");
        assert_eq!(envelope.kind, "session_started");
    }

    /// A JSON-RPC response (has an `id`, no `method`) must NOT be classified
    /// as a notification.
    #[test]
    fn classify_notification_ignores_other_shapes() {
        let response = json!({"jsonrpc": "2.0", "id": 1, "result": {}});
        assert!(classify_notification(&response).is_none());

        let other_method = json!({"method": "ping", "params": {}});
        assert!(classify_notification(&other_method).is_none());

        let malformed_params = json!({"method": "session.event", "params": {"not": "an envelope"}});
        assert!(classify_notification(&malformed_params).is_none());
    }
}
