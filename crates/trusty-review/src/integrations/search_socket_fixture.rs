//! A fake trusty-search Unix socket for the #9214 transport tests.
//!
//! Why: the socket leg must be proven without a live daemon, and on a developer
//! machine the real socket is live — a test that reached it would pass or fail
//! on whatever that daemon holds.
//! What: binds a `UnixListener` at a caller-chosen temp path, answers each
//! newline-framed JSON-RPC request with the caller's canned reply, and records
//! every `(method, params)` it was sent.
//! Test: used by `search_transport_tests.rs` and the subprocess client tests.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

/// A canned answer: a `result`, or an `error` as `(code, message, data)`.
pub(crate) type Reply = Result<Value, (i64, String, Option<Value>)>;

/// One fake daemon; the accept loop stops when this is dropped.
pub(crate) struct FakeSearchSocket {
    /// Where the socket is bound.
    pub(crate) path: PathBuf,
    calls: Arc<Mutex<Vec<(String, Value)>>>,
    task: tokio::task::JoinHandle<()>,
}

impl FakeSearchSocket {
    /// Bind at `path` (creating its parent) and answer with `reply`.
    pub(crate) fn serve(
        path: &Path,
        reply: impl Fn(&str, &Value) -> Reply + Send + Sync + 'static,
    ) -> Self {
        if let Some(parent) = path.parent() {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::create_dir_all(parent).expect("create the socket's parent");
            // The client refuses a socket whose directory is not 0700, as the
            // daemon's hardened bind leaves it.
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
                .expect("narrow the socket's parent to 0700");
        }
        let listener = UnixListener::bind(path).expect("bind the fake search socket");
        let calls = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&calls);
        let reply = Arc::new(reply);
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let seen = Arc::clone(&seen);
                let reply = Arc::clone(&reply);
                tokio::spawn(async move {
                    let (read, mut write) = stream.into_split();
                    let mut line = String::new();
                    if BufReader::new(read).read_line(&mut line).await.is_err() {
                        return;
                    }
                    let Ok(request) = serde_json::from_str::<Value>(&line) else {
                        return;
                    };
                    let method = request["method"].as_str().unwrap_or_default().to_string();
                    let params = request["params"].clone();
                    seen.lock()
                        .expect("calls lock")
                        .push((method.clone(), params.clone()));
                    let frame = match reply(&method, &params) {
                        Ok(result) => {
                            json!({"jsonrpc": "2.0", "id": request["id"], "result": result})
                        }
                        Err((code, message, data)) => {
                            let mut error = json!({"code": code, "message": message});
                            if let Some(data) = data {
                                error["data"] = data;
                            }
                            json!({"jsonrpc": "2.0", "id": request["id"], "error": error})
                        }
                    };
                    let mut out = frame.to_string();
                    out.push('\n');
                    let _ = write.write_all(out.as_bytes()).await;
                });
            }
        });
        Self {
            path: path.to_path_buf(),
            calls,
            task,
        }
    }

    /// Every `(method, params)` received so far, in arrival order.
    pub(crate) fn calls(&self) -> Vec<(String, Value)> {
        self.calls.lock().expect("calls lock").clone()
    }

    /// The methods received so far.
    pub(crate) fn methods(&self) -> Vec<String> {
        self.calls().into_iter().map(|(m, _)| m).collect()
    }
}

impl Drop for FakeSearchSocket {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// A minimal healthy `search.health` result.
pub(crate) fn healthy() -> Value {
    json!({"status": "ok", "version": "test", "indexes": 1, "embedder": "ready"})
}

/// Set or clear one env var for the life of the guard, restoring it on drop.
///
/// Only for `#[serial_test::serial]` tests: the environment is process-global.
pub(crate) struct EnvGuard(&'static str, Option<String>);

impl EnvGuard {
    /// Set `key` to `value`.
    pub(crate) fn set(key: &'static str, value: &str) -> Self {
        let old = std::env::var(key).ok();
        // SAFETY: callers hold the crate's serial env lock.
        unsafe { std::env::set_var(key, value) };
        Self(key, old)
    }

    /// Remove `key`.
    pub(crate) fn unset(key: &'static str) -> Self {
        let old = std::env::var(key).ok();
        // SAFETY: callers hold the crate's serial env lock.
        unsafe { std::env::remove_var(key) };
        Self(key, old)
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: the guard lives inside a serial test.
        unsafe {
            match &self.1 {
                Some(v) => std::env::set_var(self.0, v),
                None => std::env::remove_var(self.0),
            }
        }
    }
}
