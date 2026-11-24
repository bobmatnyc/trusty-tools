//! The console's ask client for one instance's ask daemon (#11748).
//!
//! Why: `POST /api/console/assistants/{id}/ask` answers through the instance's
//! running ask daemon. The console must never start one (ruling: no cold
//! start), must never hang a request on it, and must keep its write half open
//! until the reply arrives — the daemon reads EOF as a hang-up and cancels the
//! ask (`trusty-assistants/src/ask/daemon/mod.rs`, "Wire protocol").
//! What: [`ask_socket`] dials a hardened socket (owner, `0600`, `0700`
//! directory — the same check `ta`'s client runs), writes one [`AskRequest`]
//! line, reads one [`AskReply`] line of at most [`AskLimits::max_reply_bytes`],
//! and reports every failure as an [`AskFailure`]. Connect and the whole ask
//! are both bounded; on a timeout the stream is dropped, which the daemon
//! reads as a hang-up and cancels that ask.
//! Test: `ask_tests.rs` beside this file (a fake daemon on a temp socket).

use std::io::ErrorKind;
use std::path::Path;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use trusty_common::uds::{UdsSecurityError, connect_hardened};

use super::{AskReply, AskRequest, PROTOCOL_VERSION, ReplyErrorKind, ReplyOutcome};

/// Bound on dialling the socket.
///
/// Why: a Unix-socket connect is local and answers at once unless the
/// daemon's accept backlog is full, so two seconds is generous and still
/// fails a wedged daemon quickly.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// Bound on one whole ask: connect, send, model run, reply.
///
/// Why: a real-model answer takes tens of seconds, a tool-using one longer,
/// and the daemon answers one ask at a time, so a question can also queue
/// behind another. Three minutes covers that and still frees the request; the
/// dropped stream cancels the ask on the daemon side.
pub const ASK_TIMEOUT: Duration = Duration::from_secs(180);

/// Largest reply line read, newline included. Mirrors `MAX_LINE_BYTES` in
/// `trusty-assistants/src/ask/daemon/mod.rs`, the daemon's own line cap.
/// Test: `tests::the_mirrored_constants_match_the_source`.
pub const MAX_REPLY_BYTES: usize = 1024 * 1024;

/// The bounds one ask runs under.
///
/// Why: the defaults are production values; tests shrink them so a slow or
/// oversize daemon is proven without waiting minutes or sending megabytes.
/// Test: `ask_to_a_slow_daemon_is_504`, `ask_with_an_oversize_reply_is_502`
/// (`tests/assistants_ask.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AskLimits {
    /// Bound on the connect.
    pub connect_timeout: Duration,
    /// Bound on the whole ask, connect included.
    pub ask_timeout: Duration,
    /// Largest reply line accepted, newline included.
    pub max_reply_bytes: usize,
}

impl Default for AskLimits {
    fn default() -> Self {
        Self {
            connect_timeout: CONNECT_TIMEOUT,
            ask_timeout: ASK_TIMEOUT,
            max_reply_bytes: MAX_REPLY_BYTES,
        }
    }
}

/// Why an ask produced no answer. Every variant is a failure; none is an
/// answer (#11748 fail-open check).
/// Test: `ask_tests.rs`, one case per variant the fake daemon can provoke.
#[derive(Debug, thiserror::Error)]
pub enum AskFailure {
    /// No socket, or a socket file nobody listens on: the daemon is not running.
    #[error("the ask daemon is not running")]
    NotRunning,
    /// The socket or its directory failed the ownership/mode check, or the
    /// connect failed for another reason.
    #[error("the ask socket could not be dialled: {0}")]
    Unreachable(String),
    /// The connect did not finish within [`AskLimits::connect_timeout`].
    #[error("connect timed out after {0:?}")]
    ConnectTimeout(Duration),
    /// No reply within [`AskLimits::ask_timeout`].
    #[error("no reply within {0:?}")]
    Timeout(Duration),
    /// Sending the request or reading the reply failed.
    #[error("{what}: {source}")]
    Io {
        /// The step that failed.
        what: &'static str,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },
    /// The daemon closed the connection without a reply line.
    #[error("the daemon closed the connection without replying")]
    Disconnected,
    /// The reply line is longer than [`AskLimits::max_reply_bytes`].
    #[error("the reply is longer than {0} bytes")]
    ReplyTooLarge(usize),
    /// The reply line is not an [`AskReply`].
    #[error("malformed reply: {0}")]
    BadReply(String),
    /// The daemon speaks another protocol version.
    #[error("the daemon speaks protocol {theirs}, the console {PROTOCOL_VERSION}")]
    ProtocolMismatch {
        /// The daemon's version, `0` when absent.
        theirs: u64,
    },
    /// The daemon replied with an error outcome.
    #[error("the daemon refused the ask ({kind:?})")]
    Daemon {
        /// The daemon's error kind.
        kind: ReplyErrorKind,
        /// The daemon's message; logged by kind only, never returned.
        message: String,
    },
}

/// Ask the daemon serving `socket` one question; never starts a daemon.
///
/// Why: the one dial the ask route makes (#11748).
/// What: the connect is bounded by `limits.connect_timeout`, and the connect
/// plus the exchange by `limits.ask_timeout`. The write half stays open until
/// the reply line is read.
///
/// # Errors
///
/// An [`AskFailure`] naming what went wrong; see each variant.
/// Test: `ask_tests.rs`.
pub async fn ask_socket(
    socket: &Path,
    question: &str,
    limits: AskLimits,
) -> Result<String, AskFailure> {
    let run = async {
        let stream = match tokio::time::timeout(limits.connect_timeout, connect(socket)).await {
            Ok(connected) => connected?,
            Err(_) => return Err(AskFailure::ConnectTimeout(limits.connect_timeout)),
        };
        exchange(stream, question, limits.max_reply_bytes).await
    };
    // #11748: dropping the stream on timeout is the daemon's cancel signal.
    match tokio::time::timeout(limits.ask_timeout, run).await {
        Ok(outcome) => outcome,
        Err(_) => Err(AskFailure::Timeout(limits.ask_timeout)),
    }
}

/// Dial `socket` with `ta`'s hardened check; absent or refused is
/// [`AskFailure::NotRunning`].
async fn connect(socket: &Path) -> Result<UnixStream, AskFailure> {
    // Same order as `ta`'s client: no socket means no daemon, before the
    // directory check could refuse a directory the daemon never hardened.
    if matches!(std::fs::symlink_metadata(socket), Err(e) if e.kind() == ErrorKind::NotFound) {
        return Err(AskFailure::NotRunning);
    }
    match connect_hardened(socket).await {
        Ok(stream) => Ok(stream),
        Err(UdsSecurityError::StatForConnect { source, .. })
            if source.kind() == ErrorKind::NotFound =>
        {
            Err(AskFailure::NotRunning)
        }
        Err(UdsSecurityError::Connect { source, .. })
            if matches!(
                source.kind(),
                ErrorKind::ConnectionRefused | ErrorKind::NotFound
            ) =>
        {
            Err(AskFailure::NotRunning)
        }
        Err(e) => Err(AskFailure::Unreachable(e.to_string())),
    }
}

/// Send one request line on `stream` and read one reply line.
///
/// What: the write half is held until the reply is read (#11748: EOF earlier
/// cancels the ask). The read takes at most `max_reply + 1` bytes, so an
/// endless reply is cut off instead of buffered.
async fn exchange(
    stream: UnixStream,
    question: &str,
    max_reply: usize,
) -> Result<String, AskFailure> {
    let (read, mut write) = stream.into_split();
    let request = AskRequest {
        protocol: PROTOCOL_VERSION,
        question: question.to_owned(),
    };
    let mut line = serde_json::to_vec(&request).map_err(|e| AskFailure::BadReply(e.to_string()))?;
    line.push(b'\n');
    write
        .write_all(&line)
        .await
        .map_err(|source| AskFailure::Io {
            what: "send the question",
            source,
        })?;
    let cap = u64::try_from(max_reply)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let mut reply = Vec::new();
    let read_result = BufReader::new(read)
        .take(cap)
        .read_until(b'\n', &mut reply)
        .await;
    // #11748: only now may the daemon see EOF.
    drop(write);
    let n = read_result.map_err(|source| AskFailure::Io {
        what: "read the reply",
        source,
    })?;
    if n == 0 {
        return Err(AskFailure::Disconnected);
    }
    if reply.last() != Some(&b'\n') {
        return Err(if reply.len() > max_reply {
            AskFailure::ReplyTooLarge(max_reply)
        } else {
            AskFailure::Disconnected
        });
    }
    parse_reply(&reply)
}

/// The answer in one reply line, or the failure it reports.
/// Test: `ask_tests::a_daemon_error_is_a_failure`, `ask_tests::a_protocol_mismatch_is_a_failure`.
pub(crate) fn parse_reply(line: &[u8]) -> Result<String, AskFailure> {
    let value: serde_json::Value =
        serde_json::from_slice(line).map_err(|e| AskFailure::BadReply(e.to_string()))?;
    let theirs = value.get("protocol").and_then(serde_json::Value::as_u64);
    if theirs != Some(u64::from(PROTOCOL_VERSION)) {
        return Err(AskFailure::ProtocolMismatch {
            theirs: theirs.unwrap_or(0),
        });
    }
    let reply: AskReply =
        serde_json::from_value(value).map_err(|e| AskFailure::BadReply(e.to_string()))?;
    match reply.outcome {
        ReplyOutcome::Answer { answer } => Ok(answer),
        ReplyOutcome::Error { kind, message } => Err(AskFailure::Daemon { kind, message }),
    }
}
