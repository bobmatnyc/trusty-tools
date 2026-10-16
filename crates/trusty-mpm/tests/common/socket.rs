//! Daemon-socket fixtures for tests that spawn the built `tm` (#6288).
//!
//! Why: since the #6288 client cutover every `tm` command and hook reaches the
//! daemon over its unix socket only. A test that stood up a loopback HTTP mock
//! and passed `--url` now has to stand up a socket and pass
//! `TRUSTY_MPM_SOCKET` instead, or the child never reaches the mock.
//! What: [`serve_state`] serves a real `DaemonState` on a socket;
//! [`spawn_mock`] serves canned JSON-RPC frames from a std thread (usable from
//! a plain `#[test]`) and hands back every request frame it read;
//! [`frame_for_http`] maps an HTTP status line and body onto the frame the
//! daemon socket answers with. Each fixture returns the `unix:` locator a
//! client takes, so a helper that used to take a URL can take it unchanged.
//! The socket directories are kept, as [`super::scratch_home`]'s is: a mock
//! outlives the call that made it.
//! Test: used by the hook and CLI targets that spawn `tm`.

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use serde_json::Value;

/// A fresh hardened socket path: `0700` directory, not yet bound.
pub fn socket_path() -> PathBuf {
    let dir = tempfile::tempdir().expect("tempdir").keep();
    let sock_dir = dir.join("s");
    std::fs::create_dir(&sock_dir).expect("socket dir");
    std::fs::set_permissions(&sock_dir, std::fs::Permissions::from_mode(0o700)).expect("0700");
    sock_dir.join("mpm.sock")
}

/// The `unix:` locator for `path` — the form `core::discovery::socket_locator`
/// writes, spelled here so these fixtures also build against a pre-#6288
/// library (the red-first runs).
pub fn locator(path: &Path) -> String {
    format!("unix:{}", path.display())
}

/// The socket path a locator names, if it is one.
pub fn path_of(url: &str) -> Option<PathBuf> {
    url.strip_prefix("unix:")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
}

/// Point `cmd` at the daemon `url` names: `TRUSTY_MPM_SOCKET` for a socket
/// locator, else an absent socket, so the child reaches no daemon at all.
pub fn aim<'a>(cmd: &'a mut std::process::Command, url: &str) -> &'a mut std::process::Command {
    let path = path_of(url).unwrap_or_else(|| socket_path().with_file_name("absent.sock"));
    cmd.env("TRUSTY_MPM_SOCKET", path)
}

/// How a mock answers one request frame.
pub enum Reply {
    /// Write this text (normally one newline-terminated frame) and close.
    Frame(String),
    /// Read the request and hold the connection open without answering.
    Silent,
}

/// A result frame whose `result` is `raw` verbatim; invalid JSON stays invalid.
pub fn result_frame(raw: &str) -> String {
    format!("{{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{raw}}}\n")
}

/// An error frame with `code` and `message`.
pub fn error_frame(code: i64, message: &str) -> String {
    let frame = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "error": {"code": code, "message": message},
    });
    format!("{frame}\n")
}

/// The frame the daemon socket answers where HTTP answered `status_line` and
/// `body`: a result for `2xx`, the daemon's error code for a refusal, and
/// "method not found" for a `404` (the socket form of a route an older daemon
/// lacks).
pub fn frame_for_http(status_line: &str, body: &str) -> String {
    let code = match status_line.split(' ').next() {
        Some(s) if s.starts_with('2') => return result_frame(body),
        Some("404") => -32601,
        Some("400") => -32602,
        Some("403") => -32003,
        Some("409") => -32009,
        Some("422") => -32006,
        Some("503") => -32002,
        _ => -32603,
    };
    error_frame(code, body)
}

/// A mock daemon socket answering every request with `reply(&frame)`.
///
/// What: binds a hardened socket, then a detached std thread accepts in a
/// loop; each request frame is parsed, sent on the returned channel, and
/// answered per [`Reply`]. Returns the locator and the request channel.
pub fn spawn_mock(
    reply: impl Fn(&Value) -> Reply + Send + 'static,
) -> (String, mpsc::Receiver<Value>) {
    let path = socket_path();
    let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind mock socket");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("0600");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        while let Ok((stream, _)) = listener.accept() {
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                continue;
            }
            let frame: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
            let answer = reply(&frame);
            let _ = tx.send(frame);
            match answer {
                Reply::Frame(text) => {
                    let _ = reader.get_mut().write_all(text.as_bytes());
                }
                Reply::Silent => std::thread::sleep(Duration::from_secs(10)),
            }
        }
    });
    (locator(&path), rx)
}

/// A real daemon state served on its socket until the value drops.
pub struct DaemonSocket {
    /// The socket path.
    pub path: PathBuf,
    /// Its `unix:` locator.
    pub url: String,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Drop for DaemonSocket {
    fn drop(&mut self) {
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
    }
}

/// Serve `state` on a fresh socket and wait until it answers.
pub async fn serve_state(state: Arc<trusty_mpm::daemon::state::DaemonState>) -> DaemonSocket {
    let path = socket_path();
    let bound = trusty_mpm::daemon::socket::bind(&path)
        .await
        .expect("bind daemon socket");
    let (stop, shutdown) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(trusty_mpm::daemon::socket::serve_until_shutdown(
        bound,
        state,
        async {
            let _ = shutdown.await;
        },
    ));
    for _ in 0..200 {
        if trusty_common::uds::socket_is_serving(&path, Duration::from_millis(50)).await {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    DaemonSocket {
        url: locator(&path),
        path,
        stop: Some(stop),
    }
}
