//! A fake trusty-memory daemon that answers the protocol handshake (#9538).
//!
//! Why: every console call to trusty-memory now opens with `memory.protocol`,
//! so a stub that answers one frame and closes would refuse its own test. One
//! stub shape keeps the console's memory tests on the same daemon.
//! What: [`spawn`] serves any number of connections; `memory.protocol` gets
//! `protocol`, every other method gets `reply`, and each method is recorded.
//! Test: this file IS test support; its users are the `call_*` tests.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

/// The handshake body of a daemon on the protocol this console supports.
pub(crate) fn supported() -> Value {
    json!({ "protocol_version": *trusty_common::memory_rpc::SUPPORTED_MEMORY_PROTOCOLS.start() })
}

/// Serve `<dir>/sockets/memory.sock`; returns the socket and every method seen.
pub(crate) fn spawn(
    dir: &Path,
    protocol: Value,
    reply: impl Into<String>,
) -> (PathBuf, Arc<Mutex<Vec<String>>>) {
    let socket = dir.join("sockets").join("memory.sock");
    let reply = reply.into();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let listener = trusty_common::uds::bind_hardened(&socket).expect("bind");
    let log = Arc::clone(&seen);
    tokio::spawn(async move {
        loop {
            let Ok((mut conn, _)) = listener.accept().await else {
                return;
            };
            use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
            let mut raw = Vec::new();
            let _ = conn.read_to_end(&mut raw).await;
            let request: Value = serde_json::from_slice(&raw).unwrap_or(Value::Null);
            let method = request["method"].as_str().unwrap_or_default().to_string();
            let line = if method == "memory.protocol" {
                json!({ "jsonrpc": "2.0", "id": 1, "result": protocol }).to_string()
            } else {
                reply.clone()
            };
            if let Ok(mut seen) = log.lock() {
                seen.push(method);
            }
            let _ = conn.write_all(line.as_bytes()).await;
            let _ = conn.write_all(b"\n").await;
            let _ = conn.flush().await;
        }
    });
    (socket, seen)
}
