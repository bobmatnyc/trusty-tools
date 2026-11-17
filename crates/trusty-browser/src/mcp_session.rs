//! The MCP server's `agent.sock` client, in both modes.
//!
//! Why: every tool call must reach the broker as the bytes Python 0.7.0
//! sends (`json.dumps` with its default `", "`/`": "` separators) and every
//! socket failure must surface as Python's refusal (`agent_mcp.py`
//! `CodeSession`, and `run`'s `call` for workflow mode).
//! What: [`CodeSession`] holds at most one attached connection, opened only
//! by `browser_connect`; any socket failure drops it and answers
//! `ServiceUnavailable`, and `GrantEnded` drops it and remembers the reason
//! for the next `NotConnected`. [`WorkflowWire`] is the one connection
//! workflow mode opens at start; a failure closes it for good.
//! Test: `connect_exchange_replays_byte_identically` and
//! `workflow_exchange_replays_byte_identically` in `tests/parity_mcp.rs`.

use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::frame::{HEADER, MAX_FRAME};
use crate::mcp_args::repr;
use crate::mcp_upload::local;
use crate::pyjson::Json;

/// Socket timeout (`TIMEOUT`).
pub const TIMEOUT: Duration = Duration::from_secs(30);
/// `ServiceUnavailable` message.
pub const UNAVAILABLE: &str = "The Trusty Browser service is not reachable on agent.sock; start it, then call browser_connect again";
/// `NotConnected` message.
pub const NOT_CONNECTED: &str = "No session; call browser_connect first";
/// `AlreadyConnected` message.
pub const ALREADY_CONNECTED: &str = "A session is already held; call browser_disconnect first";

/// Why a reply could not be read; the text is Python's exception message.
#[derive(Debug)]
enum ReplyError {
    Io(io::Error),
    Msg(&'static str),
}

impl ReplyError {
    fn text(&self) -> String {
        match self {
            ReplyError::Msg(m) => (*m).to_owned(),
            ReplyError::Io(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                "Trusty Browser closed the connection".to_owned()
            }
            ReplyError::Io(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                "timed out".to_owned()
            }
            ReplyError::Io(e) => match e.raw_os_error() {
                Some(n) => format!("[Errno {n}] {}", strerror(n)),
                None => e.to_string(),
            },
        }
    }
}

fn strerror(n: i32) -> String {
    let s = io::Error::from_raw_os_error(n).to_string();
    // Rust appends " (os error N)"; Python prints only the C text.
    s.split(" (os error").next().unwrap_or(&s).to_owned()
}

fn open(path: &Path) -> io::Result<UnixStream> {
    let wire = UnixStream::connect(path)?;
    wire.set_read_timeout(Some(TIMEOUT))?;
    wire.set_write_timeout(Some(TIMEOUT))?;
    Ok(wire)
}

/// Request body bytes: `json.dumps(frame, allow_nan=False)`; `None` when over `MAX_FRAME`.
fn request_body(frame: &Json) -> Option<Vec<u8>> {
    let body = frame.dumps_spaced().ok()?;
    (body.len() <= MAX_FRAME).then(|| body.into_bytes())
}

/// Send one request and read one reply (Python `read_reply`).
fn exchange(wire: &mut UnixStream, body: &[u8]) -> Result<Json, ReplyError> {
    let size = u32::try_from(body.len())
        .map_err(|_| ReplyError::Msg("Trusty Browser request is too large"))?;
    let mut out = size.to_ne_bytes().to_vec();
    out.extend_from_slice(body);
    wire.write_all(&out).map_err(ReplyError::Io)?;
    let mut header = [0u8; HEADER];
    read_full(wire, &mut header, "Trusty Browser closed the connection")?;
    let size = u32::from_ne_bytes(header) as usize;
    if size == 0 || size > MAX_FRAME {
        return Err(ReplyError::Msg("Invalid Trusty Browser frame size"));
    }
    let mut payload = vec![0u8; size];
    read_full(wire, &mut payload, "Trusty Browser reply was interrupted")?;
    let reply =
        Json::parse(&payload).map_err(|_| ReplyError::Msg("Invalid Trusty Browser reply"))?;
    match reply.get("ok") {
        Some(Json::Bool(_)) => Ok(reply),
        _ => Err(ReplyError::Msg("Invalid Trusty Browser reply")),
    }
}

/// `stream.read(n)` on a buffered socket file: short only at EOF.
fn read_full(wire: &mut UnixStream, buf: &mut [u8], short: &'static str) -> Result<(), ReplyError> {
    let mut got = 0;
    while got < buf.len() {
        match wire.read(&mut buf[got..]) {
            Ok(0) => return Err(ReplyError::Msg(short)),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(ReplyError::Io(e)),
        }
    }
    Ok(())
}

/// Connect mode's session (Python `CodeSession`).
#[derive(Debug)]
pub struct CodeSession {
    path: PathBuf,
    wire: Option<UnixStream>,
    /// #11239: the last `GrantEnded` reason, so `NotConnected` says why.
    ended: Option<String>,
}

impl CodeSession {
    /// A session for the `agent.sock` at `path`; nothing is opened yet.
    pub fn new(path: PathBuf) -> CodeSession {
        CodeSession {
            path,
            wire: None,
            ended: None,
        }
    }

    fn unavailable() -> Json {
        local("ServiceUnavailable", UNAVAILABLE)
    }

    /// Peek without blocking: a closed or failed socket is dead.
    fn alive(wire: &UnixStream) -> bool {
        let mut byte = 0u8;
        // SAFETY: one writable byte, a live socket fd.
        let n = unsafe {
            libc::recv(
                wire.as_raw_fd(),
                (&raw mut byte).cast(),
                1,
                libc::MSG_PEEK | libc::MSG_DONTWAIT,
            )
        };
        match n {
            0 => false,
            n if n > 0 => true,
            _ => io::Error::last_os_error().kind() == io::ErrorKind::WouldBlock,
        }
    }

    /// One frame on the held connection; a failure drops it.
    fn send(&mut self, frame: &Json) -> Result<Json, Json> {
        let body = request_body(frame)
            .ok_or_else(|| local("InvalidRequest", "The request is too large"))?;
        let Some(wire) = self.wire.as_mut() else {
            return Err(Self::unavailable());
        };
        exchange(wire, &body).map_err(|_| {
            self.wire = None;
            Self::unavailable()
        })
    }

    /// `browser_profiles`: a one-shot connection; the session is untouched.
    pub fn profiles(&mut self) -> Result<Json, Json> {
        let held = self.wire.take();
        let result = match open(&self.path) {
            Ok(w) => {
                self.wire = Some(w);
                self.send(&Json::object([("op", Json::from("profiles"))]))
            }
            Err(_) => Err(Self::unavailable()),
        };
        self.wire = held;
        let reply = result?;
        refused_unless_ok(reply)
    }

    /// `browser_connect`: attach to a connected profile (no code).
    pub fn connect(&mut self, profile: Option<&str>) -> Result<Json, Json> {
        if self.wire.as_ref().is_some_and(Self::alive) {
            return Err(local("AlreadyConnected", ALREADY_CONNECTED));
        }
        self.wire = None;
        self.ended = None; // a new attempt never reports the old grant's end
        self.wire = Some(open(&self.path).map_err(|_| Self::unavailable())?);
        let mut hello = Json::object([("op", Json::from("attach"))]);
        if let Some(p) = profile {
            hello.insert("profile", Json::from(p));
        }
        let reply = self.send(&hello)?;
        if reply.get("ok") != Some(&Json::Bool(true)) {
            self.wire = None; // The broker closes a refused attach; so do we.
            return Err(reply);
        }
        Ok(reply)
    }

    /// Any other op on the attached connection.
    pub fn call(&mut self, op: &str, fields: Vec<(&str, Json)>) -> Result<Json, Json> {
        if self.wire.is_none() {
            let mut frame = local("NotConnected", NOT_CONNECTED);
            if let Some(reason) = &self.ended {
                frame.insert("reason", Json::from(reason.as_str()));
            }
            return Err(frame);
        }
        let mut frame = Json::object([("op", Json::from(op))]);
        for (k, v) in fields {
            frame.insert(k, v);
        }
        let reply = self.send(&frame)?;
        if reply.get("ok") != Some(&Json::Bool(true)) {
            if reply.get("error").and_then(Json::as_str) == Some("GrantEnded") {
                self.wire = None; // The broker closes the socket after this frame.
                let reason = reply
                    .get("reason")
                    .and_then(Json::as_str)
                    .unwrap_or("unknown");
                self.ended = Some(reason.to_owned());
            }
            return Err(reply);
        }
        Ok(reply)
    }

    /// `browser_disconnect`.
    pub fn disconnect(&mut self) -> Json {
        let held = self.wire.take().is_some();
        self.ended = None;
        Json::object([("ok", Json::Bool(true)), ("disconnected", Json::Bool(held))])
    }
}

fn refused_unless_ok(reply: Json) -> Result<Json, Json> {
    if reply.get("ok") == Some(&Json::Bool(true)) {
        Ok(reply)
    } else {
        Err(reply)
    }
}

/// Workflow mode's one connection (Python `run`'s `call`).
#[derive(Debug)]
pub struct WorkflowWire {
    wire: Option<UnixStream>,
}

impl WorkflowWire {
    /// Connect to `agent.sock`.
    pub fn connect(path: &Path) -> io::Result<WorkflowWire> {
        Ok(WorkflowWire {
            wire: Some(open(path)?),
        })
    }

    /// Send `{"op": op, ...fields}`; an `ok:false` reply is its `error` text.
    pub fn call(&mut self, op: &str, fields: Vec<(&str, Json)>) -> Result<Json, String> {
        let mut frame = Json::object([("op", Json::from(op))]);
        for (k, v) in fields {
            frame.insert(k, v);
        }
        let body = request_body(&frame).ok_or("Trusty Browser request is too large")?;
        let Some(wire) = self.wire.as_mut() else {
            return Err(format!("[Errno {}] {}", libc::EBADF, strerror(libc::EBADF)));
        };
        let reply = exchange(wire, &body).map_err(|e| {
            self.wire = None;
            e.text()
        })?;
        if reply.get("ok") == Some(&Json::Bool(true)) {
            return Ok(reply);
        }
        Err(match reply.get("error") {
            None => "Request refused".to_owned(),
            Some(Json::Str(s)) => s.clone(),
            Some(other) => repr(other),
        })
    }
}
