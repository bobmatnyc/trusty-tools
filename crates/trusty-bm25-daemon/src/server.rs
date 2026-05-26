//! Unix-domain-socket accept loop and per-connection JSON-RPC dispatch.
//!
//! Why: the daemon exposes its BM25 service over a UDS so trusty-memory
//! (and other in-host clients) can talk to it without a TCP port. UDS also
//! gives us a natural file-system credential check (operators can chmod
//! the socket file if they need to restrict access).
//!
//! What: `bind_listener` opens the listener, `run_accept_loop` runs the
//! accept loop, and each accepted stream is moved into a per-connection
//! task that reads newline-delimited JSON-RPC frames, dispatches to the
//! `BatchQueue`, and writes a response frame.
//!
//! Test: unit coverage for `dispatch_request` lives in this module;
//! end-to-end coverage in `tests/bm25_daemon.rs`.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

use crate::batch_queue::BatchQueue;
use crate::protocol::{
    DeleteParams, DeleteResult, IndexParams, IndexResult, RebuildResult, RpcRequest, RpcResponse,
    SearchParams, SearchResult, ERR_INTERNAL, ERR_INVALID_REQUEST, ERR_METHOD_NOT_FOUND, ERR_PARSE,
    JSONRPC_VERSION, METHOD_DELETE, METHOD_INDEX, METHOD_REBUILD, METHOD_SEARCH,
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
/// scales well for the daemon's expected load (a handful of in-host
/// clients, short-lived requests) without the complexity of a thread pool.
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
/// Test: integration test exercises the happy path; parse-error and
/// unknown-method paths are covered by `dispatch_request_*` tests.
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
/// Test: `dispatch_request_handles_*`.
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

    // Step 2: dispatch on method name.
    match req.method.as_str() {
        METHOD_INDEX => dispatch_index(id, req.params, queue).await,
        METHOD_SEARCH => dispatch_search(id, req.params, queue).await,
        METHOD_DELETE => dispatch_delete(id, req.params, queue).await,
        METHOD_REBUILD => dispatch_rebuild(id, queue).await,
        other => RpcResponse::err(id, ERR_METHOD_NOT_FOUND, format!("unknown method: {other}")),
    }
}

async fn dispatch_index(
    id: serde_json::Value,
    params: Option<serde_json::Value>,
    queue: &BatchQueue,
) -> RpcResponse {
    let Some(params_value) = params else {
        return RpcResponse::err(id, ERR_INVALID_REQUEST, "missing params");
    };
    let params: IndexParams = match serde_json::from_value(params_value) {
        Ok(p) => p,
        Err(e) => {
            return RpcResponse::err(id, ERR_INVALID_REQUEST, format!("invalid params: {e}"));
        }
    };
    match queue.index_doc(params.doc_id, params.text).await {
        Ok(_indexed) => {
            let result = match serde_json::to_value(IndexResult { indexed: true }) {
                Ok(v) => v,
                Err(e) => {
                    return RpcResponse::err(id, ERR_INTERNAL, format!("serialise ack: {e}"));
                }
            };
            RpcResponse::ok(id, result)
        }
        Err(e) => RpcResponse::err(id, ERR_INTERNAL, format!("index failed: {e:#}")),
    }
}

async fn dispatch_search(
    id: serde_json::Value,
    params: Option<serde_json::Value>,
    queue: &BatchQueue,
) -> RpcResponse {
    let Some(params_value) = params else {
        return RpcResponse::err(id, ERR_INVALID_REQUEST, "missing params");
    };
    let params: SearchParams = match serde_json::from_value(params_value) {
        Ok(p) => p,
        Err(e) => {
            return RpcResponse::err(id, ERR_INVALID_REQUEST, format!("invalid params: {e}"));
        }
    };
    match queue.search(params.query, params.top_k).await {
        Ok(hits) => {
            let result = match serde_json::to_value(SearchResult { hits }) {
                Ok(v) => v,
                Err(e) => {
                    return RpcResponse::err(id, ERR_INTERNAL, format!("serialise hits: {e}"));
                }
            };
            RpcResponse::ok(id, result)
        }
        Err(e) => RpcResponse::err(id, ERR_INTERNAL, format!("search failed: {e:#}")),
    }
}

async fn dispatch_delete(
    id: serde_json::Value,
    params: Option<serde_json::Value>,
    queue: &BatchQueue,
) -> RpcResponse {
    // NOTE: Access discipline — `delete` is reserved for the dream
    // subprocess. The hot request path (`Bm25Client`) deliberately does
    // not expose this method so a runaway extractor can't wipe the index.
    // The daemon honours the call regardless; operators are expected to
    // gate access at the socket-permission layer (chmod the .sock file).
    let Some(params_value) = params else {
        return RpcResponse::err(id, ERR_INVALID_REQUEST, "missing params");
    };
    let params: DeleteParams = match serde_json::from_value(params_value) {
        Ok(p) => p,
        Err(e) => {
            return RpcResponse::err(id, ERR_INVALID_REQUEST, format!("invalid params: {e}"));
        }
    };
    match queue.delete(params.doc_id).await {
        Ok(deleted) => {
            let result_val = match serde_json::to_value(DeleteResult { deleted }) {
                Ok(v) => v,
                Err(e) => {
                    return RpcResponse::err(id, ERR_INTERNAL, format!("serialise ack: {e}"));
                }
            };
            RpcResponse::ok(id, result_val)
        }
        Err(e) => RpcResponse::err(id, ERR_INTERNAL, format!("delete failed: {e:#}")),
    }
}

async fn dispatch_rebuild(id: serde_json::Value, queue: &BatchQueue) -> RpcResponse {
    // NOTE: Same access-discipline note as `dispatch_delete` — rebuild is
    // a privileged operation reserved for the dream subprocess.
    match queue.rebuild().await {
        Ok(doc_count) => {
            let result_val = match serde_json::to_value(RebuildResult { doc_count }) {
                Ok(v) => v,
                Err(e) => {
                    return RpcResponse::err(id, ERR_INTERNAL, format!("serialise ack: {e}"));
                }
            };
            RpcResponse::ok(id, result_val)
        }
        Err(e) => RpcResponse::err(id, ERR_INTERNAL, format!("rebuild failed: {e:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::batch_queue::{BatchConfig, BatchQueue};
    use crate::index::PalaceBm25Index;

    fn test_queue() -> (BatchQueue, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let index = PalaceBm25Index::load_or_create(dir.path()).expect("create test index");
        // Keep the TempDir alive — `rebuild` and write batches flush the
        // on-disk snapshot, so the data dir must outlive the queue.
        (BatchQueue::new(index, BatchConfig::default()), dir)
    }

    #[tokio::test]
    async fn dispatch_request_handles_unknown_method() {
        let (q, _dir) = test_queue();
        let frame = r#"{"jsonrpc":"2.0","method":"bogus","params":{},"id":1}"#;
        let resp = dispatch_request(frame, &q).await;
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, ERR_METHOD_NOT_FOUND);
        assert_eq!(resp.id, serde_json::json!(1));
    }

    #[tokio::test]
    async fn dispatch_request_rejects_unknown_jsonrpc_version() {
        let (q, _dir) = test_queue();
        let frame =
            r#"{"jsonrpc":"1.0","method":"index","params":{"doc_id":"d","text":"t"},"id":2}"#;
        let resp = dispatch_request(frame, &q).await;
        assert_eq!(resp.error.unwrap().code, ERR_INVALID_REQUEST);
    }

    #[tokio::test]
    async fn dispatch_request_returns_parse_error_on_garbage() {
        let (q, _dir) = test_queue();
        let frame = "not json at all";
        let resp = dispatch_request(frame, &q).await;
        assert_eq!(resp.error.unwrap().code, ERR_PARSE);
        assert_eq!(resp.id, serde_json::Value::Null);
    }

    #[tokio::test]
    async fn dispatch_request_handles_index_then_search() {
        let (q, _dir) = test_queue();
        // Index a document, then search for it.
        let index_frame = r#"{"jsonrpc":"2.0","method":"index","params":{"doc_id":"d1","text":"cargo test runs all tests"},"id":"a1"}"#;
        let resp = dispatch_request(index_frame, &q).await;
        assert!(resp.error.is_none(), "index must succeed: {resp:?}");

        // Search
        let search_frame = r#"{"jsonrpc":"2.0","method":"search","params":{"query":"cargo test","top_k":10},"id":"s1"}"#;
        let resp = dispatch_request(search_frame, &q).await;
        assert!(resp.error.is_none(), "search must succeed: {resp:?}");
        let hits = resp
            .result
            .unwrap()
            .get("hits")
            .and_then(|h| h.as_array())
            .cloned()
            .unwrap();
        assert!(!hits.is_empty(), "expected at least one hit");
        assert_eq!(hits[0]["doc_id"].as_str().unwrap(), "d1");
    }

    #[tokio::test]
    async fn dispatch_request_rejects_missing_params() {
        let (q, _dir) = test_queue();
        let frame = r#"{"jsonrpc":"2.0","method":"index","id":3}"#;
        let resp = dispatch_request(frame, &q).await;
        assert_eq!(resp.error.unwrap().code, ERR_INVALID_REQUEST);
    }

    #[tokio::test]
    async fn dispatch_request_handles_delete() {
        let (q, _dir) = test_queue();
        // Index then delete.
        let index =
            r#"{"jsonrpc":"2.0","method":"index","params":{"doc_id":"d1","text":"alpha"},"id":1}"#;
        dispatch_request(index, &q).await;
        let del = r#"{"jsonrpc":"2.0","method":"delete","params":{"doc_id":"d1"},"id":2}"#;
        let resp = dispatch_request(del, &q).await;
        assert!(resp.error.is_none(), "delete must succeed: {resp:?}");
        // delete returns `{"deleted": <bool>}`.
        let result = resp.result.unwrap();
        assert!(result.get("deleted").is_some());
    }

    #[tokio::test]
    async fn dispatch_request_handles_rebuild() {
        let (q, _dir) = test_queue();
        let index =
            r#"{"jsonrpc":"2.0","method":"index","params":{"doc_id":"d1","text":"alpha"},"id":1}"#;
        dispatch_request(index, &q).await;
        let rebuild = r#"{"jsonrpc":"2.0","method":"rebuild","params":{},"id":2}"#;
        let resp = dispatch_request(rebuild, &q).await;
        assert!(resp.error.is_none(), "rebuild must succeed: {resp:?}");
        // rebuild returns `{"doc_count": 0}`.
        let doc_count = resp.result.unwrap()["doc_count"].as_u64().unwrap();
        assert_eq!(doc_count, 0);
    }
}
