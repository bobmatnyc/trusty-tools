//! Unix-domain-socket accept loop and per-connection JSON-RPC dispatch.
//!
//! Why: the daemon exposes its embed service over a UDS so the trusty-memory
//! daemon (and other in-host clients) can talk to it without a TCP port. UDS
//! also gives us a natural file-system credential check (operators can chmod
//! the socket file if they need to restrict access).
//!
//! What: `run` binds the listener, then loops accepting connections; each
//! accepted stream is moved into a per-connection task that reads
//! newline-delimited JSON-RPC frames, dispatches `embed` to the batch queue,
//! and writes a response frame.
//!
//! Test: integration coverage in `tests/embed_daemon.rs` (spins up a real
//! daemon, sends a request, asserts the response).
//!
//! The accept loop and dispatch helpers are intentionally generic over the
//! batch-queue interface — the only embedder-touching code is inside
//! `BatchQueue` itself.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

use crate::batch_queue::BatchQueue;
use crate::protocol::{
    EmbedParams, EmbedResult, RpcRequest, RpcResponse, ERR_INTERNAL, ERR_INVALID_REQUEST,
    ERR_METHOD_NOT_FOUND, ERR_PARSE, JSONRPC_VERSION, METHOD_EMBED,
};

/// Bind the UDS listener at `path`.
///
/// Why: extracted so the binary's startup sequence (cleanup → bind) is
/// readable and so tests can exercise the same bind path against a tempfile
/// socket.
/// What: returns the `UnixListener` ready to accept connections. The caller
/// must have already removed any stale socket file at `path`.
/// Test: covered by the integration test.
pub fn bind_listener(path: &Path) -> Result<UnixListener> {
    UnixListener::bind(path)
        .with_context(|| format!("bind unix domain socket at {}", path.display()))
}

/// Accept connections in a loop, spawning a per-connection handler task.
///
/// Why: a single-threaded accept loop with detached per-connection tasks
/// scales well for the daemon's expected load (dozens of concurrent clients,
/// short-lived requests) without the complexity of a thread pool.
/// What: loops `listener.accept().await`; on each accept, clones the
/// `BatchQueue` handle and spawns [`handle_connection`].
/// Test: covered by the integration test.
pub async fn run_accept_loop(listener: UnixListener, queue: Arc<BatchQueue>) {
    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let queue = Arc::clone(&queue);
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(stream, queue).await {
                        tracing::warn!("connection handler exited with error: {e:#}");
                    }
                });
            }
            Err(e) => {
                tracing::error!("accept failed: {e}");
                // Brief pause to avoid a hot error loop if accept is broken.
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    }
}

/// Per-connection read/dispatch/write loop.
///
/// Why: each connection may carry multiple requests (clients can pipeline)
/// so we read newline-delimited frames until EOF.
/// What: wraps the stream's read half in a `BufReader`, calls `read_line`
/// in a loop, dispatches the parsed request, writes the response as a
/// single newline-terminated JSON blob. On parse error replies with the
/// JSON-RPC `-32700` envelope and keeps reading.
/// Test: integration test exercises the happy path; parse-error path is
/// covered by `dispatch_request_handles_unknown_method`.
async fn handle_connection(stream: UnixStream, queue: Arc<BatchQueue>) -> Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();

    loop {
        line.clear();
        let n = reader
            .read_line(&mut line)
            .await
            .context("read JSON-RPC frame")?;
        if n == 0 {
            // EOF — peer closed the connection.
            return Ok(());
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            // Keepalive / blank line — ignore.
            continue;
        }

        let response = dispatch_request(trimmed, &queue).await;
        let mut payload = serde_json::to_vec(&response).context("serialise JSON-RPC response")?;
        payload.push(b'\n');
        write_half
            .write_all(&payload)
            .await
            .context("write JSON-RPC response")?;
    }
}

/// Parse one JSON-RPC frame and dispatch it.
///
/// Why: pulled out so we can unit-test the dispatch shape without standing
/// up a UDS pair.
/// What: returns the `RpcResponse` to send back. Always returns a response
/// (no notifications supported); on parse failure the id is `null`.
/// Test: `dispatch_request_handles_unknown_method` and the integration
/// happy-path test.
pub async fn dispatch_request(frame: &str, queue: &BatchQueue) -> RpcResponse {
    // Step 1: parse the envelope. If this fails we cannot recover the id.
    let req: RpcRequest = match serde_json::from_str(frame) {
        Ok(r) => r,
        Err(e) => {
            return RpcResponse::err(
                serde_json::Value::Null,
                ERR_PARSE,
                format!("parse error: {e}"),
            );
        }
    };

    let id = req.id.clone().unwrap_or(serde_json::Value::Null);

    if req.jsonrpc != JSONRPC_VERSION {
        return RpcResponse::err(
            id,
            ERR_INVALID_REQUEST,
            format!("unsupported jsonrpc version: {}", req.jsonrpc),
        );
    }

    if req.method != METHOD_EMBED {
        return RpcResponse::err(
            id,
            ERR_METHOD_NOT_FOUND,
            format!("unknown method: {}", req.method),
        );
    }

    // Step 2: decode params.
    let Some(params_value) = req.params else {
        return RpcResponse::err(id, ERR_INVALID_REQUEST, "missing params");
    };
    let params: EmbedParams = match serde_json::from_value(params_value) {
        Ok(p) => p,
        Err(e) => {
            return RpcResponse::err(id, ERR_INVALID_REQUEST, format!("invalid params: {e}"));
        }
    };

    // Step 3: dispatch.
    match queue.embed_many(params.texts).await {
        Ok(embeddings) => {
            let result = match serde_json::to_value(EmbedResult { embeddings }) {
                Ok(v) => v,
                Err(e) => {
                    return RpcResponse::err(id, ERR_INTERNAL, format!("serialise result: {e}"));
                }
            };
            RpcResponse::ok(id, result)
        }
        Err(e) => RpcResponse::err(id, ERR_INTERNAL, format!("embed failed: {e:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::batch_queue::{BatchConfig, BatchQueue};
    use std::sync::Arc;
    use trusty_common::embedder::{Embedder, MockEmbedder, EMBED_DIM};

    fn test_queue() -> BatchQueue {
        let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(EMBED_DIM));
        BatchQueue::new(embedder, BatchConfig::default())
    }

    #[tokio::test]
    async fn dispatch_request_handles_unknown_method() {
        let q = test_queue();
        let frame = r#"{"jsonrpc":"2.0","method":"bogus","params":{},"id":1}"#;
        let resp = dispatch_request(frame, &q).await;
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, ERR_METHOD_NOT_FOUND);
        assert_eq!(resp.id, serde_json::json!(1));
    }

    #[tokio::test]
    async fn dispatch_request_rejects_unknown_jsonrpc_version() {
        let q = test_queue();
        let frame = r#"{"jsonrpc":"1.0","method":"embed","params":{"texts":["x"]},"id":2}"#;
        let resp = dispatch_request(frame, &q).await;
        assert_eq!(resp.error.unwrap().code, ERR_INVALID_REQUEST);
    }

    #[tokio::test]
    async fn dispatch_request_returns_parse_error_on_garbage() {
        let q = test_queue();
        let frame = "not json at all";
        let resp = dispatch_request(frame, &q).await;
        assert_eq!(resp.error.unwrap().code, ERR_PARSE);
        assert_eq!(resp.id, serde_json::Value::Null);
    }

    #[tokio::test]
    async fn dispatch_request_handles_happy_path() {
        let q = test_queue();
        let frame = r#"{"jsonrpc":"2.0","method":"embed","params":{"texts":["a","b"]},"id":"abc"}"#;
        let resp = dispatch_request(frame, &q).await;
        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        let embeddings = result
            .get("embeddings")
            .and_then(|v| v.as_array())
            .expect("embeddings array");
        assert_eq!(embeddings.len(), 2);
    }

    #[tokio::test]
    async fn dispatch_request_rejects_missing_params() {
        let q = test_queue();
        let frame = r#"{"jsonrpc":"2.0","method":"embed","id":3}"#;
        let resp = dispatch_request(frame, &q).await;
        assert_eq!(resp.error.unwrap().code, ERR_INVALID_REQUEST);
    }
}
