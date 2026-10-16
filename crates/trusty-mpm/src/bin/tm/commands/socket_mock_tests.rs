//! One-connection daemon-socket mocks for the `tm` binary's unit tests
//! (#6288 client cutover).
//!
//! Why: the hook and guard tests drove canned HTTP answers from loopback TCP
//! listeners. Every client now dials the daemon socket only, so the canned
//! answers have to arrive as JSON-RPC frames on a hardened unix socket.
//! What: [`MockSocket`] owns the temp directory and derefs to the `unix:`
//! locator a client takes in its `url` slot. [`answering`] serves one frame,
//! [`silent`] accepts and never answers, [`unaccepted`] binds and never
//! accepts (so [`was_dialled`] can read whether anything connected), and
//! [`absent`] is a locator with no socket behind it.
//! Test: used by `pm_guard_dispatch`'s transport tests.

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;

/// A mock daemon socket's locator, valid while this value lives.
pub(crate) struct MockSocket {
    url: String,
    _dir: tempfile::TempDir,
}

impl std::ops::Deref for MockSocket {
    type Target = str;
    fn deref(&self) -> &str {
        &self.url
    }
}

impl std::fmt::Display for MockSocket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.url)
    }
}

/// A listener the hardened client dial trusts: `0600` in a `0700` directory.
fn hardened_listener() -> (UnixListener, PathBuf, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let sock_dir = dir.path().join("s");
    std::fs::create_dir(&sock_dir).expect("socket dir");
    std::fs::set_permissions(&sock_dir, std::fs::Permissions::from_mode(0o700)).expect("0700");
    let path = sock_dir.join("mpm.sock");
    let listener = UnixListener::bind(&path).expect("bind socket");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("0600");
    (listener, path, dir)
}

/// A result frame whose `result` is `raw` verbatim — invalid JSON stays
/// invalid, which is how a test sends an unparseable answer.
pub(crate) fn result_frame(raw: &str) -> String {
    format!("{{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{raw}}}\n")
}

/// An error frame with `code` and `message`.
pub(crate) fn error_frame(code: i64, message: &str) -> String {
    let frame = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "error": {"code": code, "message": message},
    });
    format!("{frame}\n")
}

/// A socket that reads one request frame and answers `frame`.
pub(crate) fn answering(frame: String) -> MockSocket {
    let (listener, path, dir) = hardened_listener();
    std::thread::spawn(move || {
        if let Ok((socket, _)) = listener.accept() {
            let mut reader = BufReader::new(socket);
            let mut line = String::new();
            let _ = reader.read_line(&mut line);
            let _ = reader.get_mut().write_all(frame.as_bytes());
        }
    });
    MockSocket {
        url: trusty_mpm::core::socket_locator(&path),
        _dir: dir,
    }
}

/// A socket that accepts, reads the request, and never answers.
pub(crate) fn silent() -> MockSocket {
    let (listener, path, dir) = hardened_listener();
    std::thread::spawn(move || {
        if let Ok((socket, _)) = listener.accept() {
            let mut reader = BufReader::new(socket);
            let mut line = String::new();
            let _ = reader.read_line(&mut line);
            std::thread::sleep(std::time::Duration::from_secs(10));
        }
    });
    MockSocket {
        url: trusty_mpm::core::socket_locator(&path),
        _dir: dir,
    }
}

/// A bound socket that never accepts, and its locator.
pub(crate) fn unaccepted() -> (UnixListener, MockSocket) {
    let (listener, path, dir) = hardened_listener();
    let mock = MockSocket {
        url: trusty_mpm::core::socket_locator(&path),
        _dir: dir,
    };
    (listener, mock)
}

/// Did anything connect to `listener`? A queued connection accepts at once.
pub(crate) fn was_dialled(listener: &UnixListener) -> bool {
    listener.set_nonblocking(true).expect("nonblocking");
    listener.accept().is_ok()
}

/// A locator naming a socket that does not exist: a daemon that is down.
pub(crate) fn absent() -> MockSocket {
    let dir = tempfile::tempdir().expect("tempdir");
    let url = trusty_mpm::core::socket_locator(&dir.path().join("absent.sock"));
    MockSocket { url, _dir: dir }
}
