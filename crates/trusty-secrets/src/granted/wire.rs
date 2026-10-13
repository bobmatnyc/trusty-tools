//! One `secrets.resolve` over the socket, blocking, std only (#9071).
//!
//! Why: the `api` feature links no tokio and no trusty-common, so the tier-3
//! client speaks the server's frame format itself: one newline-terminated
//! JSON-RPC 2.0 request per connection, a half-close, one answer frame.
//! What: [`resolve`] checks the socket as trusty-common's
//! `verify_socket_for_connect` does (parent a non-symlink 0700 directory of
//! this uid, the socket a non-symlink 0600 socket of this uid), connects,
//! writes the frame, reads the answer under the 8 MiB frame cap within the
//! whole-call wait, and decodes it. It never retries: the server may have
//! the frame. A blocking connect to a full backlog on Linux can outlast the
//! wait, since std sets timeouts after connect; recorded as a known limit.
//! Test: `granted_client_sends_one_resolve_frame`,
//! `granted_client_refuses_an_untrusted_socket`,
//! `granted_client_times_out_on_a_silent_server`,
//! `granted_client_rejects_an_unterminated_oversized_frame`,
//! `granted_client_decode_failure_is_an_error`.

use std::fs;
use std::io::{self, ErrorKind, Read, Write};
use std::net::Shutdown;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{GrantedError, Refusal};
use crate::api::methods::{ResolveRequest, ResolveResponse, method};
use crate::api::{SecretKey, SecretValue};

/// The largest answer frame read: trusty-common's `uds::MAX_FRAME_BYTES`.
///
/// Test: `granted_frame_cap_matches_the_uds_cap`.
pub(crate) const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// The request id; the answer must carry it back.
const REQUEST_ID: u64 = 1;

const DIR_MODE: u32 = 0o700;
const SOCKET_MODE: u32 = 0o600;

/// One `secrets.resolve` of `key` under `token` on `socket`, within `wait`.
pub(crate) fn resolve(
    socket: &Path,
    token: &str,
    key: &SecretKey,
    wait: Duration,
) -> Result<SecretValue, GrantedError> {
    let deadline = Instant::now() + wait;
    verify_socket(socket)?;
    let frame = request_frame(token, key)?;
    let mut stream = UnixStream::connect(socket).map_err(|source| GrantedError::Unavailable {
        path: socket.to_path_buf(),
        source,
    })?;
    stream
        .set_write_timeout(Some(remaining(deadline)?))
        .map_err(GrantedError::Transport)?;
    stream.write_all(&frame).map_err(io_failure)?;
    stream.shutdown(Shutdown::Write).map_err(io_failure)?;
    let answer = read_frame(&mut stream, deadline)?;
    decode(&answer, key)
}

/// Refuse a socket this uid does not own, or one others could reach.
fn verify_socket(path: &Path) -> Result<(), GrantedError> {
    let refuse = |reason| GrantedError::UntrustedSocket {
        path: path.to_path_buf(),
        reason,
    };
    let unavailable = |source| GrantedError::Unavailable {
        path: path.to_path_buf(),
        source,
    };
    let parent = match path.parent() {
        Some(parent) if parent.as_os_str().is_empty() => Path::new("."),
        Some(parent) => parent,
        None => return Err(refuse("the socket path has no parent directory")),
    };
    // SAFETY: `getuid` takes no arguments and always succeeds.
    let own = unsafe { libc::getuid() };
    let dir = fs::symlink_metadata(parent).map_err(unavailable)?;
    if dir.file_type().is_symlink() {
        return Err(refuse("the socket directory is a symlink"));
    }
    if !dir.is_dir() {
        return Err(refuse("the socket directory is not a directory"));
    }
    if dir.uid() != own {
        return Err(refuse("the socket directory belongs to another user"));
    }
    if dir.mode() & 0o777 != DIR_MODE {
        return Err(refuse("the socket directory is not mode 0700"));
    }
    let meta = fs::symlink_metadata(path).map_err(unavailable)?;
    let kind = meta.file_type();
    if kind.is_symlink() {
        return Err(refuse("the socket path is a symlink"));
    }
    if !kind.is_socket() {
        return Err(refuse("the path is not a socket"));
    }
    if meta.uid() != own {
        return Err(refuse("the socket belongs to another user"));
    }
    if meta.mode() & 0o777 != SOCKET_MODE {
        return Err(refuse("the socket is not mode 0600"));
    }
    Ok(())
}

/// The JSON-RPC request envelope.
#[derive(Serialize)]
struct Request {
    jsonrpc: &'static str,
    id: u64,
    method: &'static str,
    params: ResolveRequest,
}

/// The request as one newline-terminated frame, built by serde.
fn request_frame(token: &str, key: &SecretKey) -> Result<Vec<u8>, GrantedError> {
    let request = Request {
        jsonrpc: "2.0",
        id: REQUEST_ID,
        method: method::RESOLVE,
        params: ResolveRequest::new(token, key.clone()),
    };
    let mut frame = serde_json::to_vec(&request)
        .map_err(|_| GrantedError::Transport(io::Error::other("the request did not encode")))?;
    frame.push(b'\n');
    Ok(frame)
}

/// The time left before `deadline`; none left is a timeout.
fn remaining(deadline: Instant) -> Result<Duration, GrantedError> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        Err(GrantedError::Timeout)
    } else {
        Ok(left)
    }
}

/// A socket I/O error: a timeout is `Timeout`, anything else `Transport`.
fn io_failure(error: io::Error) -> GrantedError {
    match error.kind() {
        ErrorKind::WouldBlock | ErrorKind::TimedOut => GrantedError::Timeout,
        _ => GrantedError::Transport(error),
    }
}

/// One answer frame: up to the first newline, or to EOF.
///
/// What: each read gets only the time left before `deadline`, so a server
/// that trickles bytes cannot stretch the call; more than
/// [`MAX_FRAME_BYTES`] with no newline is refused; no bytes at all is an
/// unexpected EOF.
fn read_frame(stream: &mut UnixStream, deadline: Instant) -> Result<Vec<u8>, GrantedError> {
    let mut frame = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        stream
            .set_read_timeout(Some(remaining(deadline)?))
            .map_err(GrantedError::Transport)?;
        let n = match stream.read(&mut buf) {
            Ok(n) => n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(io_failure(e)),
        };
        if n == 0 {
            break;
        }
        let chunk = &buf[..n];
        let end = chunk.iter().position(|b| *b == b'\n');
        frame.extend_from_slice(&chunk[..end.unwrap_or(n)]);
        if frame.len() > MAX_FRAME_BYTES {
            return Err(GrantedError::Transport(io::Error::new(
                ErrorKind::InvalidData,
                "the answer is larger than the frame cap",
            )));
        }
        if end.is_some() {
            break;
        }
    }
    if frame.is_empty() {
        return Err(GrantedError::Transport(io::Error::new(
            ErrorKind::UnexpectedEof,
            "the server closed the connection without an answer",
        )));
    }
    Ok(frame)
}

/// The JSON-RPC answer envelope.
#[derive(Deserialize)]
struct Answer {
    #[serde(default)]
    id: Option<Value>,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<WireError>,
}

/// A JSON-RPC error object.
#[derive(Deserialize)]
struct WireError {
    code: i64,
    #[serde(default)]
    message: String,
    #[serde(default)]
    data: Option<Value>,
}

/// The value in `frame`, only when it answers this request for `key`.
fn decode(frame: &[u8], key: &SecretKey) -> Result<SecretValue, GrantedError> {
    // A serde error can quote its input, so its text is dropped.
    let answer: Answer = serde_json::from_slice(frame).map_err(|_| GrantedError::Decode)?;
    if answer.id != Some(Value::from(REQUEST_ID)) {
        return Err(GrantedError::Decode);
    }
    if let Some(error) = answer.error {
        let kind = error
            .data
            .as_ref()
            .and_then(|data| data.get("kind"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        return Err(GrantedError::Refused(Refusal {
            code: error.code,
            message: error.message,
            kind,
        }));
    }
    let result = answer.result.ok_or(GrantedError::EmptyResponse)?;
    let resolved: ResolveResponse =
        serde_json::from_value(result).map_err(|_| GrantedError::Decode)?;
    if resolved.key != *key {
        return Err(GrantedError::KeyMismatch);
    }
    Ok(resolved.value)
}
