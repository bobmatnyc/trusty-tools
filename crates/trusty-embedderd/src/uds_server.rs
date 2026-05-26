//! Unix-domain-socket accept loop for `trusty-embedderd`.
//!
//! Why: adds a low-latency in-host transport alongside the existing HTTP
//! listener. Both transports funnel through the same `BatchQueue` instance
//! so there is exactly one ONNX session regardless of how many clients connect
//! over how many transports (issue #164).
//!
//! What: `run_uds_accept_loop` binds a `UnixListener` at the given path
//! and spawns a per-connection task for each accepted stream. Each connection
//! handler reads newline-delimited JSON-RPC 2.0 frames, dispatches `embed`
//! requests to the shared `BatchQueue`, and writes response frames. The wire
//! format is identical to the retired `trusty-embed-daemon` UDS protocol.
//!
//! Test: integration coverage in `tests/concurrent_embed.rs` (UDS-only and
//! mixed HTTP+UDS concurrent tests). Unit dispatch tests are in
//! `dispatch_request_*` functions below (shared with the HTTP integration test
//! plumbing from `trusty-embed-daemon`).

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

/// Bind a `UnixListener` at `path`, cleaning up any stale socket file first.
///
/// Why: a previous run may have left a socket file that would cause `EADDRINUSE`
/// on the next bind attempt. Removing it first is the idiomatic Unix pattern.
/// What: removes any existing file at `path` (ignoring "not found"), then
/// calls `UnixListener::bind`.
/// Test: covered by the concurrent_embed integration test.
pub fn bind_uds_listener(path: &Path) -> Result<UnixListener> {
    // Best-effort cleanup of a stale socket file.
    if let Err(e) = std::fs::remove_file(path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!("failed to remove stale UDS socket {}: {e}", path.display());
        }
    }
    UnixListener::bind(path).with_context(|| format!("bind UDS socket at {}", path.display()))
}

/// Accept connections in a loop, spawning a per-connection handler task.
///
/// Why: a single-threaded accept loop with detached per-connection tasks
/// scales well for the daemon's expected load (dozens of concurrent in-host
/// clients, short-lived requests).
/// What: loops `listener.accept().await`; on each accepted stream, clones the
/// `BatchQueue` handle and spawns [`handle_connection`].
/// Test: covered by `concurrent_embed.rs` integration test.
pub async fn run_uds_accept_loop(listener: UnixListener, queue: Arc<BatchQueue>) {
    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let q = Arc::clone(&queue);
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(stream, q).await {
                        tracing::warn!("UDS connection handler exited with error: {e:#}");
                    }
                });
            }
            Err(e) => {
                tracing::error!("UDS accept failed: {e}");
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    }
}

/// Per-connection read/dispatch/write loop.
///
/// Why: each connection may carry multiple pipelined requests so we read
/// newline-delimited frames until EOF.
/// What: wraps the read half in a `BufReader`, calls `read_line` in a loop,
/// dispatches each parsed request via `dispatch_request`, writes the response
/// as a single newline-terminated JSON blob. On parse error replies with the
/// JSON-RPC `-32700` envelope and keeps reading.
/// Test: dispatch unit tests below; end-to-end in `tests/concurrent_embed.rs`.
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
/// Why: extracted so unit tests can exercise the dispatch logic without
/// standing up a real UDS pair.
/// What: returns the `RpcResponse` to send back. Always returns a response
/// (no notifications supported); on parse failure the id is `null`.
/// Test: `dispatch_*` tests below.
pub async fn dispatch_request(frame: &str, queue: &BatchQueue) -> RpcResponse {
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

    let Some(params_value) = req.params else {
        return RpcResponse::err(id, ERR_INVALID_REQUEST, "missing params");
    };
    let params: EmbedParams = match serde_json::from_value(params_value) {
        Ok(p) => p,
        Err(e) => {
            return RpcResponse::err(id, ERR_INVALID_REQUEST, format!("invalid params: {e}"));
        }
    };

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
    async fn dispatch_unknown_method_returns_error() {
        // Why: callers that send the wrong method name must get a graceful error.
        // What: dispatch a request with method "bogus"; assert error code -32601.
        // Test: this test.
        let q = test_queue();
        let frame = r#"{"jsonrpc":"2.0","method":"bogus","params":{},"id":1}"#;
        let resp = dispatch_request(frame, &q).await;
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, ERR_METHOD_NOT_FOUND);
        assert_eq!(resp.id, serde_json::json!(1));
    }

    #[tokio::test]
    async fn dispatch_rejects_unknown_jsonrpc_version() {
        // Why: strict version check protects against future protocol changes.
        let q = test_queue();
        let frame = r#"{"jsonrpc":"1.0","method":"embed","params":{"texts":["x"]},"id":2}"#;
        let resp = dispatch_request(frame, &q).await;
        assert_eq!(resp.error.unwrap().code, ERR_INVALID_REQUEST);
    }

    #[tokio::test]
    async fn dispatch_returns_parse_error_on_garbage() {
        // Why: non-JSON frames must return a parse-error envelope with id=null.
        let q = test_queue();
        let resp = dispatch_request("not json", &q).await;
        assert_eq!(resp.error.unwrap().code, ERR_PARSE);
        assert_eq!(resp.id, serde_json::Value::Null);
    }

    #[tokio::test]
    async fn dispatch_happy_path_returns_embeddings() {
        // Why: the success path must return an embeddings array of correct length.
        let q = test_queue();
        let frame = r#"{"jsonrpc":"2.0","method":"embed","params":{"texts":["a","b"]},"id":"xyz"}"#;
        let resp = dispatch_request(frame, &q).await;
        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        let embs = result
            .get("embeddings")
            .and_then(|v| v.as_array())
            .expect("embeddings array");
        assert_eq!(embs.len(), 2);
    }

    #[tokio::test]
    async fn dispatch_rejects_missing_params() {
        // Why: the embed method requires params; a missing params field must be
        //      rejected with an invalid-request error.
        let q = test_queue();
        let frame = r#"{"jsonrpc":"2.0","method":"embed","id":3}"#;
        let resp = dispatch_request(frame, &q).await;
        assert_eq!(resp.error.unwrap().code, ERR_INVALID_REQUEST);
    }
}
