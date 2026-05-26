//! Unix domain socket listener for `trusty-embedderd` (issue #164 Step 3).
//!
//! Why: the UDS transport lets on-host callers avoid TCP stack overhead.
//! The wire format is the same newline-framed JSON-RPC 2.0 protocol used by
//! the retired `trusty-embed-daemon`, keeping `UdsEmbedderClient` (in
//! `trusty_common::embedder_client`) backward-compatible without any client changes.
//!
//! What: `bind_uds_listener` creates the UDS socket at `path`;
//! `run_uds_accept_loop` accepts connections and spawns a per-connection task
//! that reads frames, dispatches to the shared `BatchQueue`, and writes
//! responses. `dispatch_request` is public so it can be unit-tested without
//! a live socket.
//!
//! Test: `dispatch_request_*` unit tests in this module; end-to-end in
//! `tests/uds_integration.rs` (marked `#[ignore]`).

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

use crate::batch_queue::BatchQueue;

// ── Wire protocol constants ─────────────────────────────────────────────────

/// JSON-RPC 2.0 method name the daemon accepts.
///
/// Why: shared constant prevents the dispatch arm and any client from drifting.
pub const METHOD_EMBED: &str = "embed";

/// JSON-RPC protocol version string.
pub const JSONRPC_VERSION: &str = "2.0";

// Standard JSON-RPC 2.0 error codes.

/// Malformed request envelope (missing required fields, wrong version, etc.).
pub const ERR_INVALID_REQUEST: i32 = -32600;

/// Requested method does not exist.
pub const ERR_METHOD_NOT_FOUND: i32 = -32601;

/// Payload could not be parsed as JSON.
pub const ERR_PARSE: i32 = -32700;

/// Server-side failure while executing the method.
pub const ERR_INTERNAL: i32 = -32603;

// ── Wire protocol types ─────────────────────────────────────────────────────

/// Inbound JSON-RPC 2.0 request envelope.
///
/// Why: a typed struct lets serde reject malformed messages precisely instead
/// of manually post-validating a free-form `Value`.
///
/// What: standard JSON-RPC 2.0 fields. `id` is `serde_json::Value` so the
/// daemon echoes the caller's id verbatim (clients may use strings, numbers,
/// or `null`).
///
/// Test: `embed_request_round_trips` below.
#[derive(Debug, serde::Deserialize, serde::Serialize, Clone)]
pub struct RpcRequest {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default)]
    pub params: Option<serde_json::Value>,
    #[serde(default)]
    pub id: Option<serde_json::Value>,
}

/// Parameters for the `embed` method.
///
/// Why: typed params make decoding explicit — a missing or non-array `texts`
/// field is rejected at parse time rather than at the call boundary.
///
/// What: a single field — `texts: Vec<String>`.
///
/// Test: round-trip via `RpcRequest` deserialization in tests.
#[derive(Debug, serde::Deserialize)]
pub struct EmbedParams {
    pub texts: Vec<String>,
}

/// Successful result for the `embed` method.
///
/// Why: named field gives forward-compat room for future additions.
///
/// What: `embeddings[i]` is the vector for `params.texts[i]`, all 384-dim.
///
/// Test: serialised inside `RpcResponse::ok`.
#[derive(Debug, serde::Serialize)]
pub struct EmbedResult {
    pub embeddings: Vec<Vec<f32>>,
}

/// JSON-RPC 2.0 error object.
///
/// Why: structured errors let clients distinguish failure modes without
/// string-matching on `message`.
///
/// What: `code` follows the JSON-RPC 2.0 numeric scheme (see `ERR_*`).
///
/// Test: serialised inside `RpcResponse::err`.
#[derive(Debug, serde::Serialize, serde::Deserialize, Clone)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
}

/// Outbound JSON-RPC 2.0 response envelope.
///
/// Why: exactly one of `result` / `error` is populated per the JSON-RPC spec.
/// `skip_serializing_if` keeps the wire output clean.
///
/// What: `id` echoes the request id verbatim. On parse failure (no recoverable
/// id) we emit `id = null`.
///
/// Test: `ok_response_serialises_without_error_field` below.
#[derive(Debug, serde::Serialize, serde::Deserialize, Clone)]
pub struct RpcResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
    pub id: serde_json::Value,
}

impl RpcResponse {
    /// Build a success response.
    ///
    /// Why: keeps the boilerplate of "jsonrpc=2.0, error=None" off every
    /// call site so dispatch code reads as a straight value translation.
    ///
    /// What: wraps `result` as `Some(serde_json::Value)` and the supplied id.
    ///
    /// Test: `ok_response_serialises_without_error_field`.
    pub fn ok(id: serde_json::Value, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            result: Some(result),
            error: None,
            id,
        }
    }

    /// Build an error response.
    ///
    /// Why: same DRY motivation as [`Self::ok`].
    ///
    /// What: wraps `error` as `Some(RpcError)` with the supplied code/message
    /// and the caller's id (or `null` when no id could be recovered).
    ///
    /// Test: `err_response_serialises_without_result_field`.
    pub fn err(id: serde_json::Value, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            result: None,
            error: Some(RpcError {
                code,
                message: message.into(),
            }),
            id,
        }
    }
}

// ── Listener / accept loop ──────────────────────────────────────────────────

/// Best-effort remove a (possibly stale) socket file before bind.
///
/// Why: a crashed daemon leaves its socket file behind; `bind()` on an
/// existing path returns `EADDRINUSE`. Unconditionally unlinking is the
/// idiomatic Unix pattern; we swallow "not found" to keep first-run silent.
///
/// What: calls `std::fs::remove_file`. Logs any non-`NotFound` error at
/// `warn!` so operators see permission problems but startup is not aborted.
///
/// Test: `cleanup_stale_socket_idempotent` below.
pub fn cleanup_stale_socket(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(_) => tracing::debug!("removed stale UDS socket at {}", path.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!("failed to remove stale UDS socket {}: {e}", path.display()),
    }
}

/// Bind the UDS listener at `path`.
///
/// Why: extracted so the main startup sequence (cleanup → mkdir → bind) is
/// readable and tests can exercise the bind path against a tempfile socket.
///
/// What: returns the `UnixListener` ready to accept connections. The caller
/// must have already cleaned any stale socket file at `path`.
///
/// Test: covered by the UDS integration test.
pub fn bind_uds_listener(path: &Path) -> Result<UnixListener> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create UDS socket directory {}", parent.display()))?;
    }
    UnixListener::bind(path).with_context(|| format!("bind UDS listener at {}", path.display()))
}

/// Accept connections in a loop, spawning a per-connection handler task.
///
/// Why: a single-threaded accept loop with detached per-connection tasks
/// scales well for the expected load (dozens of concurrent clients,
/// short-lived requests) without the complexity of a thread pool.
///
/// What: loops `listener.accept().await`; on each accept, clones the
/// `Arc<BatchQueue>` handle and spawns `handle_connection`.
///
/// Test: end-to-end in `tests/uds_integration.rs`.
pub async fn run_uds_accept_loop(listener: UnixListener, queue: Arc<BatchQueue>) {
    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let queue = Arc::clone(&queue);
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(stream, queue).await {
                        tracing::warn!("UDS connection handler exited with error: {e:#}");
                    }
                });
            }
            Err(e) => {
                tracing::error!("UDS accept failed: {e}");
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
///
/// What: wraps the stream's read half in a `BufReader`, calls `read_line`
/// in a loop, dispatches the parsed request, writes the response as a
/// single newline-terminated JSON blob. On parse error replies with the
/// JSON-RPC `-32700` envelope and keeps reading.
///
/// Test: dispatch-level tests in this module; connection-level in
/// `tests/uds_integration.rs`.
async fn handle_connection(stream: UnixStream, queue: Arc<BatchQueue>) -> Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();

    loop {
        line.clear();
        let n = reader
            .read_line(&mut line)
            .await
            .context("read JSON-RPC frame from UDS connection")?;
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
        let mut payload =
            serde_json::to_vec(&response).context("serialise JSON-RPC UDS response")?;
        payload.push(b'\n');
        write_half
            .write_all(&payload)
            .await
            .context("write JSON-RPC UDS response")?;
    }
}

/// Parse one JSON-RPC frame and dispatch it to the batch queue.
///
/// Why: extracted so dispatch logic can be unit-tested without a live UDS pair.
///
/// What: returns the `RpcResponse` to send back. Always returns a response
/// (no notifications supported); on parse failure the id is `null`.
///
/// Test: `dispatch_request_handles_unknown_method`,
/// `dispatch_request_returns_parse_error_on_garbage`,
/// `dispatch_request_handles_happy_path`, etc. in this module.
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

    // Step 3: dispatch to the shared batch queue.
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

// ── Tests ───────────────────────────────────────────────────────────────────

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

    #[test]
    fn ok_response_serialises_without_error_field() {
        // Why: ensures no stray "error":null in the wire output.
        // What: serialise ok response and check for absence of error field.
        // Test: this test.
        let resp = RpcResponse::ok(
            serde_json::json!("abc"),
            serde_json::json!({"embeddings":[[0.1_f32, 0.2_f32]]}),
        );
        let s = serde_json::to_string(&resp).unwrap();
        assert!(s.contains("\"result\""));
        assert!(!s.contains("\"error\""));
        assert!(s.contains("\"id\":\"abc\""));
    }

    #[test]
    fn err_response_serialises_without_result_field() {
        // Why: ensures no stray "result":null in the wire output.
        // What: serialise error response and check for absence of result field.
        // Test: this test.
        let resp = RpcResponse::err(serde_json::Value::Null, ERR_INTERNAL, "boom");
        let s = serde_json::to_string(&resp).unwrap();
        assert!(s.contains("\"error\""));
        assert!(!s.contains("\"result\""));
        assert!(s.contains("\"code\":-32603"));
    }

    #[tokio::test]
    async fn dispatch_request_handles_unknown_method() {
        // Why: unknown methods must get ERR_METHOD_NOT_FOUND, not a panic.
        // What: dispatch a frame with method "bogus".
        // Test: this test.
        let q = test_queue();
        let frame = r#"{"jsonrpc":"2.0","method":"bogus","params":{},"id":1}"#;
        let resp = dispatch_request(frame, &q).await;
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, ERR_METHOD_NOT_FOUND);
        assert_eq!(resp.id, serde_json::json!(1));
    }

    #[tokio::test]
    async fn dispatch_request_rejects_unknown_jsonrpc_version() {
        // Why: unknown protocol versions must be rejected cleanly.
        // What: dispatch a frame with jsonrpc "1.0".
        // Test: this test.
        let q = test_queue();
        let frame = r#"{"jsonrpc":"1.0","method":"embed","params":{"texts":["x"]},"id":2}"#;
        let resp = dispatch_request(frame, &q).await;
        assert_eq!(resp.error.unwrap().code, ERR_INVALID_REQUEST);
    }

    #[tokio::test]
    async fn dispatch_request_returns_parse_error_on_garbage() {
        // Why: malformed JSON must produce ERR_PARSE with id=null.
        // What: dispatch a non-JSON frame.
        // Test: this test.
        let q = test_queue();
        let frame = "not json at all";
        let resp = dispatch_request(frame, &q).await;
        assert_eq!(resp.error.unwrap().code, ERR_PARSE);
        assert_eq!(resp.id, serde_json::Value::Null);
    }

    #[tokio::test]
    async fn dispatch_request_handles_happy_path() {
        // Why: the basic success path must return embeddings[].len() == texts.len().
        // What: dispatch a valid embed frame with 2 texts via MockEmbedder.
        // Test: this test.
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
        // Why: the `embed` method requires params; absence must be caught.
        // What: dispatch a frame with no params field.
        // Test: this test.
        let q = test_queue();
        let frame = r#"{"jsonrpc":"2.0","method":"embed","id":3}"#;
        let resp = dispatch_request(frame, &q).await;
        assert_eq!(resp.error.unwrap().code, ERR_INVALID_REQUEST);
    }

    #[test]
    fn cleanup_stale_socket_idempotent() {
        // Why: cleanup_stale_socket must be safe to call whether or not the
        // file exists — first-run must not panic.
        // What: call once with a non-existent path, then create the file and
        // call again; assert the file is gone.
        // Test: this test.
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "trusty-embedderd-test-cleanup-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        cleanup_stale_socket(&path);
        assert!(!path.exists());
        std::fs::write(&path, b"").unwrap();
        assert!(path.exists());
        cleanup_stale_socket(&path);
        assert!(!path.exists());
    }
}
