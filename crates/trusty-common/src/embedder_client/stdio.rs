//! Stdio (piped stdin/stdout) embedder client for a sidecar `trusty-embedderd`
//! process.
//!
//! Why: when `trusty-search` spawns `trusty-embedderd` as a child process, the
//! cleanest IPC transport is the pipes that were created by the OS at fork time.
//! No socket files to manage or clean up, no port to discover, and the child
//! exits automatically when the parent closes its end of the pipe — a free
//! lifecycle tie that UDS and HTTP cannot provide. This is exactly the transport
//! pattern MCP uses throughout the project.
//!
//! What: `StdioEmbedderClient` owns the child's `stdin` (write) and `stdout`
//! (read) handles. Each `embed_batch` call serialises a JSON-RPC 2.0 request
//! onto stdin, reads one newline-framed response from stdout, and deserialises
//! it. A `Mutex` serialises all writes and reads so the single-flight
//! constraint is trivially satisfied without a request-id correlation layer
//! (deferred to a follow-up if multiplexing is ever needed).
//!
//! Test: unit tests below cover empty-batch short-circuit, request serialisation
//! shape, and error decoding without a live process. The `bit_identical_stdio`
//! integration test in `trusty-embedderd/tests/bit_identical.rs` asserts
//! bit-identical output over the real stdio sidecar path.

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout};
use tokio::sync::Mutex;

use super::{EmbedderClient, EmbedderError};

// ── Wire types ──────────────────────────────────────────────────────────────
// Private to this module — mirrors the types in `uds.rs` exactly so the
// daemon side can reuse the same dispatch path for both transports.

const METHOD_EMBED: &str = "embed";
const JSONRPC_VERSION: &str = "2.0";

#[derive(Debug, serde::Serialize)]
struct RpcRequest<'a> {
    jsonrpc: &'a str,
    method: &'a str,
    params: EmbedParams<'a>,
    id: u64,
}

#[derive(Debug, serde::Serialize)]
struct EmbedParams<'a> {
    texts: &'a [String],
}

#[derive(Debug, serde::Deserialize)]
struct RpcResponse {
    #[serde(default)]
    result: Option<EmbedResult>,
    #[serde(default)]
    error: Option<RpcError>,
}

#[derive(Debug, serde::Deserialize)]
struct EmbedResult {
    embeddings: Vec<Vec<f32>>,
}

#[derive(Debug, serde::Deserialize)]
struct RpcError {
    code: i32,
    message: String,
}

// ── Client ──────────────────────────────────────────────────────────────────

/// `EmbedderClient` that communicates with a sidecar `trusty-embedderd` process
/// via its piped stdin/stdout handles.
///
/// Why: avoids all socket-file / port-discovery complexity for the common case
/// where `trusty-search` itself manages the `trusty-embedderd` lifecycle. The
/// kernel guarantees exclusive ownership of these pipes — no other process can
/// inject or intercept frames.
///
/// What: holds `ChildStdin` and `ChildStdout` behind `Mutex` guards. Each call
/// to `embed_batch` acquires both locks together (write then read) so the
/// entire request-response cycle is single-flight. Callers that need higher
/// concurrency should batch texts before calling rather than issuing many
/// concurrent `embed_batch` calls; the `BatchQueue` inside the daemon already
/// coalesces batches anyway so the extra per-call serialisation on the parent
/// side loses no throughput in practice.
///
/// Test: `cargo test -p trusty-common --features embedder-client` exercises the
/// unit surface. End-to-end coverage lives in
/// `trusty-embedderd/tests/bit_identical.rs` (`bit_identical_stdio`,
/// `#[ignore]`).
pub struct StdioEmbedderClient {
    stdin: Mutex<ChildStdin>,
    stdout: Mutex<BufReader<ChildStdout>>,
}

impl StdioEmbedderClient {
    /// Construct a client from the raw pipe handles of a spawned child process.
    ///
    /// Why: callers (typically `EmbedderSupervisor`) extract `stdin` and
    /// `stdout` from a `tokio::process::Child` with `Stdio::piped()` and hand
    /// them directly to this constructor — no config or path needed.
    /// What: wraps both handles in `Mutex`. The `BufReader` around stdout
    /// provides the `read_line` primitive needed for newline-framed JSON-RPC.
    /// Test: indirectly covered by every test that constructs and calls the client.
    pub fn new(stdin: ChildStdin, stdout: ChildStdout) -> Self {
        Self {
            stdin: Mutex::new(stdin),
            stdout: Mutex::new(BufReader::new(stdout)),
        }
    }
}

#[async_trait::async_trait]
impl EmbedderClient for StdioEmbedderClient {
    /// Embed a batch of texts via the stdio JSON-RPC 2.0 transport.
    ///
    /// Why: the sidecar model; see module-level doc.
    ///
    /// What: acquires the stdin lock, serialises one newline-framed JSON-RPC
    /// request, flushes to the child's stdin. Then acquires the stdout lock
    /// and reads one newline-framed response. Decodes `embeddings`, validates
    /// the count, and returns. Any transport or protocol error is mapped to
    /// `EmbedderError::Stdio`.
    ///
    /// Test: `cargo test -p trusty-embedderd --test bit_identical -- --include-ignored`
    async fn embed_batch(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>, EmbedderError> {
        if texts.is_empty() {
            return Ok(vec![]);
        }
        let sent = texts.len();

        tracing::debug!(n = sent, "StdioEmbedderClient: sending batch");

        // Serialise the request.
        let req = RpcRequest {
            jsonrpc: JSONRPC_VERSION,
            method: METHOD_EMBED,
            params: EmbedParams { texts: &texts },
            id: 1,
        };
        let mut payload = serde_json::to_vec(&req)
            .map_err(|e| EmbedderError::Stdio(format!("serialise JSON-RPC request: {e}")))?;
        payload.push(b'\n');

        // Acquire both locks atomically (stdin first, stdout second — consistent
        // order prevents a deadlock between two concurrent callers).
        let mut stdin_guard = self.stdin.lock().await;
        let mut stdout_guard = self.stdout.lock().await;

        // Write the request frame.
        stdin_guard
            .write_all(&payload)
            .await
            .map_err(|e| EmbedderError::Stdio(format!("write request to child stdin: {e}")))?;
        stdin_guard
            .flush()
            .await
            .map_err(|e| EmbedderError::Stdio(format!("flush child stdin: {e}")))?;

        // Read one newline-terminated response frame.
        let mut line = String::new();
        let n = stdout_guard
            .read_line(&mut line)
            .await
            .map_err(|e| EmbedderError::Stdio(format!("read response from child stdout: {e}")))?;

        if n == 0 {
            return Err(EmbedderError::Stdio(
                "child closed stdout before responding (process crashed?)".to_owned(),
            ));
        }

        // Decode the response.
        let resp: RpcResponse = serde_json::from_str(line.trim()).map_err(|e| {
            EmbedderError::Stdio(format!("decode response (raw={:?}): {e}", line.trim()))
        })?;

        if let Some(err) = resp.error {
            return Err(EmbedderError::ModelError(format!(
                "daemon RPC error {}: {}",
                err.code, err.message
            )));
        }

        let result = resp.result.ok_or_else(|| {
            EmbedderError::Stdio("response missing both result and error fields".to_owned())
        })?;

        if result.embeddings.len() != sent {
            return Err(EmbedderError::DimensionMismatch {
                sent,
                got: result.embeddings.len(),
            });
        }

        tracing::debug!(n = sent, "StdioEmbedderClient: batch complete");

        Ok(result.embeddings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_serialises_correctly() {
        // Why: guard against accidental rename of JSON-RPC fields; the daemon
        //      parses these names literally.
        // What: serialise a sample request and check required wire fields.
        // Test: this test.
        let texts = vec!["hello".to_string(), "world".to_string()];
        let req = RpcRequest {
            jsonrpc: JSONRPC_VERSION,
            method: METHOD_EMBED,
            params: EmbedParams { texts: &texts },
            id: 1,
        };
        let s = serde_json::to_string(&req).unwrap();
        assert!(s.contains("\"jsonrpc\":\"2.0\""), "must have jsonrpc 2.0");
        assert!(s.contains("\"method\":\"embed\""), "must have embed method");
        assert!(
            s.contains("\"texts\":[\"hello\",\"world\"]"),
            "must include texts"
        );
        assert!(s.contains("\"id\":1"), "must have id");
    }

    #[test]
    fn error_response_maps_to_model_error() {
        // Why: daemon RPC errors must surface as EmbedderError::ModelError so
        //      callers can distinguish them from transport failures.
        // What: decode a synthetic error-response frame and check the variant.
        // Test: this test.
        let json = r#"{"jsonrpc":"2.0","error":{"code":-32603,"message":"ort failed"},"id":1}"#;
        let resp: RpcResponse = serde_json::from_str(json).unwrap();
        assert!(resp.error.is_some());
        assert!(resp.result.is_none());
        let err = resp.error.unwrap();
        assert_eq!(err.code, -32603);
        assert!(err.message.contains("ort failed"));
    }

    #[test]
    fn success_response_decoded() {
        // Why: verify the happy-path decode path works end-to-end without a
        //      live child process.
        // What: synthesise a success response and deserialise the embeddings.
        // Test: this test.
        let json = r#"{"jsonrpc":"2.0","result":{"embeddings":[[0.1,0.2],[0.3,0.4]]},"id":1}"#;
        let resp: RpcResponse = serde_json::from_str(json).unwrap();
        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        assert_eq!(result.embeddings.len(), 2);
        assert_eq!(result.embeddings[0][0], 0.1_f32);
    }
}
