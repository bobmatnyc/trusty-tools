//! Client for the cua-driver daemon's line-JSON socket protocol.
//!
//! Why: the broker reaches the bounded daemon only through its private Unix
//! socket, and a tool error or bounded refusal must never read as success
//! (plan section 3; spike `driver.py`).
//! What: [`DriverClient::request`] writes one `{"method", "name", "args"}`
//! line and reads one reply line, under a read/write timeout. A reply with
//! `ok: false` is [`DriverError::Refused`], or [`DriverError::SessionEnded`]
//! when its text is the daemon's session-ended refusal. [`DriverClient::call`]
//! also maps a tool result carrying `isError: true` to [`DriverError::ToolError`].
//! Test: `tests/driver_client.rs`.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Map, Value, json};

/// Default per-request timeout. A bounded click costs about 2.2 s (spike latency.txt).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Why the daemon could not answer, or refused.
#[derive(Debug, thiserror::Error)]
pub enum DriverError {
    /// Connect, write or read failed, or timed out.
    #[error("daemon socket {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The reply was not one JSON object line.
    #[error("bad daemon reply: {0}")]
    BadReply(String),
    /// The daemon answered `ok: false`.
    #[error("daemon refused: {message}")]
    Refused { message: String, exit_code: i64 },
    /// The daemon refused because the named session ended (see [`parse_session_ended`]).
    #[error("session '{session}' has ended; '{tool}' was rejected")]
    SessionEnded { session: String, tool: String },
    /// The tool ran and answered `isError: true` (a tool error or a bounded refusal).
    #[error("tool error: {message}")]
    ToolError { message: String, result: Value },
    /// The installed or running driver is not the pinned version.
    #[error("{0}")]
    Version(String),
    /// Lifecycle failure: launch, stop or ownership check.
    #[error("{0}")]
    Lifecycle(String),
    /// A mutating call hit an ended session; a new session started and the call was NOT re-run.
    #[error("session restarted; '{tool}' was not re-run and its outcome is unknown")]
    SessionRestarted { tool: String },
}

/// Parse the daemon's session-ended refusal into `(session, tool)`.
///
/// Why: the daemon refuses every call on an ended session until `start_session`
/// (GO-STOP.md:129); the supervisor must tell that apart from other refusals.
/// What: matches the exact cua-driver 0.23.2 text, captured live in
/// `tests/fixtures/evidence/session-ended-0.23.2.txt`:
/// `session '<id>' has ended; tool call '<tool>' was rejected. Call start_session ...`.
/// Test: `session_ended_text_is_parsed_and_others_are_not`.
pub fn parse_session_ended(message: &str) -> Option<(String, String)> {
    let rest = message.strip_prefix("session '")?;
    let (session, rest) = rest.split_once("' has ended; tool call '")?;
    let (tool, rest) = rest.split_once("' was rejected. ")?;
    rest.starts_with("Call start_session")
        .then(|| (session.to_owned(), tool.to_owned()))
}

/// The daemon's refusal once the manifest `idle_timeout` elapsed (cua-driver 0.23.2).
///
/// Every call, `start_session` included, gets it until the daemon restarts
/// (`tests/fixtures/evidence/session-ended-0.23.2.txt`).
pub const MANIFEST_IDLE_TEXT: &str =
    "Policy loading error: capability manifest idle timeout exceeded";

/// True when `err` is the manifest idle-timeout lockout.
///
/// Test: `idle_timeout_lockout_is_recognised_exactly`.
pub fn is_manifest_idle(err: &DriverError) -> bool {
    matches!(err, DriverError::Refused { message, .. } if message == MANIFEST_IDLE_TEXT)
}

/// One daemon endpoint.
#[derive(Debug, Clone)]
pub struct DriverClient {
    socket: PathBuf,
    timeout: Duration,
}

impl DriverClient {
    /// A client for `socket` with [`DEFAULT_TIMEOUT`].
    pub fn new(socket: &Path) -> DriverClient {
        DriverClient::with_timeout(socket, DEFAULT_TIMEOUT)
    }

    /// A client for `socket` whose every request times out after `timeout`.
    pub fn with_timeout(socket: &Path, timeout: Duration) -> DriverClient {
        DriverClient {
            socket: socket.to_owned(),
            timeout,
        }
    }

    /// The socket this client talks to.
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    fn io(&self, source: std::io::Error) -> DriverError {
        DriverError::Io {
            path: self.socket.clone(),
            source,
        }
    }

    /// Send one request; return `result` of an `ok: true` reply.
    ///
    /// Why: one place owns the wire format and the timeout.
    /// What: one line out, one line in; `ok: false` becomes `Refused` or
    /// `SessionEnded`. It does not inspect `isError` (see [`DriverClient::call`]).
    /// Test: `request_times_out_on_a_silent_daemon`, `refusal_and_bad_reply_are_errors`.
    pub fn request(
        &self,
        method: &str,
        name: Option<&str>,
        args: Option<&Value>,
    ) -> Result<Value, DriverError> {
        let mut payload = Map::new();
        payload.insert("method".into(), json!(method));
        if let Some(name) = name {
            payload.insert("name".into(), json!(name));
        }
        if let Some(args) = args {
            payload.insert("args".into(), args.clone());
        }
        let mut stream = UnixStream::connect(&self.socket).map_err(|e| self.io(e))?;
        stream
            .set_read_timeout(Some(self.timeout))
            .and_then(|()| stream.set_write_timeout(Some(self.timeout)))
            .map_err(|e| self.io(e))?;
        writeln!(stream, "{}", Value::Object(payload)).map_err(|e| self.io(e))?;
        let mut line = String::new();
        BufReader::new(stream)
            .read_line(&mut line)
            .map_err(|e| self.io(e))?;
        let reply: Value =
            serde_json::from_str(&line).map_err(|e| DriverError::BadReply(e.to_string()))?;
        if reply.get("ok") == Some(&Value::Bool(true)) {
            return Ok(reply.get("result").cloned().unwrap_or(Value::Null));
        }
        let message = reply
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("daemon refused")
            .to_owned();
        if let Some((session, tool)) = parse_session_ended(&message) {
            return Err(DriverError::SessionEnded { session, tool });
        }
        let exit_code = reply.get("exit_code").and_then(Value::as_i64).unwrap_or(0);
        Err(DriverError::Refused { message, exit_code })
    }

    /// The daemon's `metadata` object (`driver_version`, `pid`, ...).
    pub fn metadata(&self) -> Result<Map<String, Value>, DriverError> {
        match self.request("metadata", None, None)? {
            Value::Object(map) => Ok(map),
            other => Err(DriverError::BadReply(format!(
                "metadata is not an object: {other}"
            ))),
        }
    }

    /// Call `tool`; a result with `isError: true` is an error, never success.
    ///
    /// Test: `is_error_result_is_never_success`.
    pub fn call(&self, tool: &str, args: &Value) -> Result<Value, DriverError> {
        let result = self.request("call", Some(tool), Some(args))?;
        if result.get("isError") == Some(&Value::Bool(true)) {
            return Err(DriverError::ToolError {
                message: error_text(&result),
                result,
            });
        }
        Ok(result)
    }
}

/// The refusal message of an `isError` result: the structured refusal, else the first text part.
fn error_text(result: &Value) -> String {
    let structured = result
        .pointer("/structuredContent/refusal/message")
        .and_then(Value::as_str);
    let text = result.pointer("/content/0/text").and_then(Value::as_str);
    structured.or(text).unwrap_or("tool error").to_owned()
}
