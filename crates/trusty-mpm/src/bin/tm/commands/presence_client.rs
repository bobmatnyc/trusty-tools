//! A bounded `mpm.presence.*` call to the daemon, usable from sync code
//! (#8878 PR2b-1b).
//!
//! Why: `tm presence request|devices add` and the pm-guard hook run in sync
//! code that may already sit inside a tokio runtime (the CLI's `main`), and
//! the hook must never wait on the daemon for more than a second.
//! What: [`call`] runs the exchange on its own thread with a current-thread
//! runtime and waits at most `timeout`; a slow daemon is abandoned, never
//! awaited. An error frame becomes `Err` with its message.
//! Test: `the_hook_request_never_waits_past_its_timeout_8878`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};
use trusty_common::uds::send_framed_request;
use trusty_common::uds::server::{JSONRPC_VERSION, RpcResponse};

/// The hook's budget for one request (plan-pr2b1.md §1).
pub(crate) const HOOK_TIMEOUT: Duration = Duration::from_secs(1);

/// The CLI's budget for one call.
pub(crate) const CLI_TIMEOUT: Duration = Duration::from_secs(15);

/// A daemon call: `(method, params) -> result or message`.
pub(crate) type Call<'a> = &'a dyn Fn(&str, Value) -> Result<Value, String>;

/// The live daemon socket, `None` when the data dir cannot be resolved.
pub(crate) fn socket() -> Option<PathBuf> {
    trusty_common::daemon_socket_path("trusty-mpm").ok()
}

/// Call `method` on the daemon at `socket` with `params`, within `timeout`.
pub(crate) fn call(
    socket: &Path,
    method: &str,
    params: Value,
    timeout: Duration,
) -> Result<Value, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    let socket = socket.to_path_buf();
    let frame = json!({"jsonrpc": JSONRPC_VERSION, "id": 1, "method": method, "params": params});
    std::thread::spawn(move || {
        let outcome = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())
            .and_then(|rt| {
                rt.block_on(send_framed_request::<_, RpcResponse>(
                    &socket, &frame, timeout,
                ))
                .map_err(|e| e.to_string())
            });
        let _ = tx.send(outcome);
    });
    match rx.recv_timeout(timeout + Duration::from_millis(100)) {
        Ok(Ok(RpcResponse {
            result: Some(v), ..
        })) => Ok(v),
        Ok(Ok(RpcResponse { error: Some(e), .. })) => Err(e.message),
        Ok(Ok(_)) => Err(format!("{method}: empty reply")),
        Ok(Err(e)) => Err(format!("the daemon is unreachable: {e}")),
        Err(_) => Err(format!("the daemon did not answer within {timeout:?}")),
    }
}

/// The live [`Call`] with `timeout`: the daemon socket, or an error when
/// it cannot be resolved.
pub(crate) fn live(timeout: Duration) -> impl Fn(&str, Value) -> Result<Value, String> {
    let socket = socket();
    move |method, params| match socket.as_deref() {
        Some(path) => call(path, method, params, timeout),
        None => Err("the daemon socket path is unknown".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A listener that accepts and never answers holds the call for at most
    /// the timeout.
    #[test]
    fn the_hook_request_never_waits_past_its_timeout_8878() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("s.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&path).expect("bind");
        let started = std::time::Instant::now();
        let out = call(&path, "mpm.presence.request", json!({}), HOOK_TIMEOUT);
        assert!(out.is_err(), "{out:?}");
        assert!(
            started.elapsed() < Duration::from_millis(1500),
            "{:?}",
            started.elapsed()
        );
        let gone = call(&dir.path().join("none.sock"), "m", json!({}), HOOK_TIMEOUT);
        assert!(gone.is_err());
    }
}
