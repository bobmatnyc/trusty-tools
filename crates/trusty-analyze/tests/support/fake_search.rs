//! A fake trusty-search Unix socket for trusty-analyze's integration tests (#9214).
//!
//! Why: the analyzer reaches trusty-search over its socket, and on a developer
//! machine the real socket is live — a test that reached it would pass or fail
//! on whatever that daemon holds.
//! What: binds a hardened `UnixListener` inside its own tempdir, answers each
//! newline-framed JSON-RPC request with the caller's canned reply, and records
//! every method it was sent. Modelled on trusty-review's
//! `integrations/search_socket_fixture.rs`.
//! Test: used by `search_socket_9214.rs` and `on_demand.rs`.

#![allow(dead_code)] // each test binary compiles this module and uses a subset

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

// `Value` alone: this file is formatted under two editions (trusty-analyze and
// trusty-crate-contracts), which sort a mixed-case import list differently.
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// A canned answer: a `result`, or an `error` as `(code, message)`.
pub type Reply = Result<Value, (i64, String)>;

/// One fake daemon; the accept loop stops when this is dropped.
pub struct FakeSearchSocket {
    _dir: tempfile::TempDir,
    path: PathBuf,
    calls: Arc<Mutex<Vec<(String, Value)>>>,
    task: tokio::task::JoinHandle<()>,
}

impl FakeSearchSocket {
    /// Bind in a fresh tempdir and answer every request with `reply`.
    pub fn serve(reply: impl Fn(&str, &Value) -> Reply + Send + Sync + 'static) -> Self {
        let dir = tempfile::tempdir().expect("tempdir for the fake search socket");
        let path = dir.path().join("search.sock");
        let listener =
            trusty_common::uds::bind_hardened(&path).expect("bind the fake search socket");
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
                            serde_json::json!({"jsonrpc": "2.0", "id": request["id"], "result": result})
                        }
                        Err((code, message)) => serde_json::json!({
                            "jsonrpc": "2.0", "id": request["id"],
                            "error": {"code": code, "message": message}
                        }),
                    };
                    let mut out = frame.to_string();
                    out.push('\n');
                    let _ = write.write_all(out.as_bytes()).await;
                });
            }
        });
        Self {
            _dir: dir,
            path,
            calls,
            task,
        }
    }

    /// A fake that answers `search.health` and an empty `search.indexes.list`.
    pub fn healthy() -> Self {
        Self::serve(|method, _| match method {
            "search.health" => Ok(healthy()),
            "search.indexes.list" => Ok(serde_json::json!({ "indexes": [] })),
            other => Err((-32601, format!("fake has no {other}"))),
        })
    }

    /// Where the socket is bound.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Every `(method, params)` received so far, in arrival order.
    pub fn calls(&self) -> Vec<(String, Value)> {
        self.calls.lock().expect("calls lock").clone()
    }

    /// The methods received so far, in arrival order.
    pub fn methods(&self) -> Vec<String> {
        self.calls().into_iter().map(|(m, _)| m).collect()
    }
}

impl AsRef<Path> for FakeSearchSocket {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

impl Drop for FakeSearchSocket {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// A minimal healthy `search.health` result.
pub fn healthy() -> Value {
    serde_json::json!({"status": "ok", "version": "test", "indexes": 0, "embedder": "ready"})
}
