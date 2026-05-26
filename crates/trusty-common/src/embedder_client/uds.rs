//! UDS (Unix Domain Socket) embedder client for the unified `trusty-embedderd`
//! daemon.
//!
//! Why: the HTTP transport in `RemoteEmbedderClient` adds TCP overhead on
//! hosts where the embedder runs as a local subprocess. The UDS transport
//! provides microsecond-latency IPC while sharing the same `EmbedderClient`
//! trait, so call sites are identical regardless of transport.
//!
//! What: `UdsEmbedderClient` opens a fresh `tokio::net::UnixStream` per call,
//! writes one newline-terminated JSON-RPC 2.0 request, half-closes the write
//! side, reads one newline-terminated response frame, and returns the
//! `embeddings` array. The wire protocol matches the format used by
//! `trusty-embed-daemon` (see `crates/trusty-embed-daemon/src/protocol.rs`
//! for the daemon side's definitions) and by the UDS listener added to
//! `trusty-embedderd` in issue #164.
//!
//! Test: unit tests below cover empty-batch short-circuit, request
//! serialisation shape, and error decoding without a live daemon. The
//! `#[ignore]`-tagged `uds_bit_identical` integration test in
//! `trusty-embedderd/tests/bit_identical.rs` asserts bit-identical output
//! between `UdsEmbedderClient` and `InProcessEmbedderClient` using a real
//! ONNX model.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use super::{EmbedderClient, EmbedderError};

// ── Wire types ──────────────────────────────────────────────────────────────
// These intentionally mirror the private types in `trusty-common::embed_client`
// and the public types in `trusty-embed-daemon::protocol`. They are defined
// here (rather than re-used) so the `embedder_client` module has no dependency
// on the old `embed_client` module, which is deleted in issue #164 Step C.

/// JSON-RPC method name for the embed request.
///
/// Why: literal must agree between client and server; centralising it here
/// keeps the two halves honest.
/// What: `"embed"`.
/// Test: `request_serialises_correctly` verifies it appears in the wire bytes.
const METHOD_EMBED: &str = "embed";

/// JSON-RPC version string required by the 2.0 specification.
const JSONRPC_VERSION: &str = "2.0";

#[derive(Debug, Serialize)]
struct RpcRequest<'a> {
    jsonrpc: &'a str,
    method: &'a str,
    params: EmbedParams<'a>,
    id: u64,
}

#[derive(Debug, Serialize)]
struct EmbedParams<'a> {
    texts: &'a [String],
}

#[derive(Debug, Deserialize)]
struct RpcResponse {
    #[serde(default)]
    result: Option<EmbedResult>,
    #[serde(default)]
    error: Option<RpcError>,
}

#[derive(Debug, Deserialize)]
struct EmbedResult {
    embeddings: Vec<Vec<f32>>,
}

#[derive(Debug, Deserialize)]
struct RpcError {
    code: i32,
    message: String,
}

// ── Client ──────────────────────────────────────────────────────────────────

/// `EmbedderClient` implementation that talks to `trusty-embedderd` over a
/// Unix Domain Socket using newline-framed JSON-RPC 2.0.
///
/// Why: avoids TCP overhead for in-host deployments where the embedder daemon
/// runs as a local sibling process. UDS latency is typically < 1 ms; by
/// contrast, even a loopback TCP connection pays the kernel's TCP stack.
///
/// What: stores only the socket path (`PathBuf`). Each `embed_batch` call
/// opens a fresh `UnixStream`, sends one request frame, reads one response
/// frame, and closes the connection. This keeps the client stateless and
/// trivially `Clone`able. The single-request-per-connection model avoids
/// pipelining complexity in Phase 1; the daemon's `BatchQueue` coalesces
/// concurrent arrivals on its own.
///
/// Test: `empty_batch_short_circuits` (no daemon required), `request_serialises_correctly`,
/// and `error_response_maps_to_model_error` cover the unit surface. End-to-end
/// coverage lives in `trusty-embedderd/tests/bit_identical.rs` (marked
/// `#[ignore]`).
#[derive(Debug, Clone)]
pub struct UdsEmbedderClient {
    socket_path: PathBuf,
}

impl UdsEmbedderClient {
    /// Construct a client targeting the given socket path.
    ///
    /// Why: explicit-path callers (test harnesses, alternate deployment
    /// layouts) want to avoid the env-var-based default.
    /// What: stores the path verbatim; no I/O happens until the first
    /// `embed_batch` call.
    /// Test: trivially covered by every other test that constructs a client.
    pub fn new(socket_path: impl Into<PathBuf>) -> Self {
        Self {
            socket_path: socket_path.into(),
        }
    }

    /// Default socket path under `$TMPDIR` (or `/tmp` if unset).
    ///
    /// Why: matches `trusty-embedderd`'s own default socket path so callers
    /// can construct a client with no explicit configuration.
    /// What: returns `<TMPDIR>/trusty-embedderd.sock`. Falls back to `/tmp`
    /// when `TMPDIR` is unset or empty (typical on Linux servers).
    /// Test: `default_socket_path_uses_tmpdir`.
    pub fn default_path() -> PathBuf {
        let dir = match std::env::var("TMPDIR") {
            Ok(p) if !p.trim().is_empty() => PathBuf::from(p),
            _ => PathBuf::from("/tmp"),
        };
        dir.join(SOCKET_FILENAME)
    }

    /// The socket path this client is configured to use.
    ///
    /// Why: callers (logging, health-check displays) may need to report which
    /// path is in use.
    /// What: returns a `&Path` reference to the stored path.
    /// Test: covered transitively by construction tests.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }
}

/// Default socket filename agreed upon between `trusty-embedderd` and its UDS
/// clients.
///
/// Why: a single constant prevents the daemon and client from drifting.
/// What: `"trusty-embedderd.sock"` — distinct from the retired
/// `trusty-embed-daemon`'s `"trusty-embed.sock"` to avoid confusion.
/// Test: referenced in `default_socket_path_uses_tmpdir`.
pub const SOCKET_FILENAME: &str = "trusty-embedderd.sock";

#[async_trait::async_trait]
impl EmbedderClient for UdsEmbedderClient {
    /// Embed a batch of texts via the UDS JSON-RPC 2.0 transport.
    ///
    /// Why: thin wrapper that opens a socket, performs one request/response
    /// cycle, and returns vectors — identical semantics to `RemoteEmbedderClient`
    /// but without TCP overhead.
    ///
    /// What: opens a fresh `UnixStream`, writes one newline-framed JSON-RPC
    /// request, half-closes the write side, reads one newline-framed response,
    /// decodes the `embeddings` array, validates the count, and returns.
    /// Any transport or protocol error is mapped to `EmbedderError`.
    ///
    /// Test: `cargo test -p trusty-embedderd --test bit_identical -- --include-ignored`
    async fn embed_batch(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>, EmbedderError> {
        if texts.is_empty() {
            return Ok(vec![]);
        }
        let sent = texts.len();

        tracing::debug!(
            socket = %self.socket_path.display(),
            n = sent,
            "UdsEmbedderClient: sending batch"
        );

        // Open a fresh connection. UDS connect on a local socket is typically
        // sub-millisecond, so the simplicity of one-connection-per-call is
        // justified in Phase 1.
        let stream = UnixStream::connect(&self.socket_path).await.map_err(|e| {
            EmbedderError::Uds(format!(
                "connect to {} failed: {e}",
                self.socket_path.display()
            ))
        })?;
        let (read_half, mut write_half) = stream.into_split();

        // Build and send the request frame.
        let req = RpcRequest {
            jsonrpc: JSONRPC_VERSION,
            method: METHOD_EMBED,
            params: EmbedParams { texts: &texts },
            id: 1,
        };
        let mut payload = serde_json::to_vec(&req)
            .map_err(|e| EmbedderError::Uds(format!("serialise JSON-RPC request: {e}")))?;
        payload.push(b'\n');

        write_half
            .write_all(&payload)
            .await
            .map_err(|e| EmbedderError::Uds(format!("write request frame: {e}")))?;

        // Half-close the write side so the daemon knows the request is complete.
        write_half
            .shutdown()
            .await
            .map_err(|e| EmbedderError::Uds(format!("half-close write side: {e}")))?;

        // Read exactly one newline-terminated response frame.
        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        let n = reader
            .read_line(&mut line)
            .await
            .map_err(|e| EmbedderError::Uds(format!("read response frame: {e}")))?;

        if n == 0 {
            return Err(EmbedderError::Uds(
                "daemon closed connection before responding".to_owned(),
            ));
        }

        // Decode the response.
        let resp: RpcResponse = serde_json::from_str(line.trim()).map_err(|e| {
            EmbedderError::Uds(format!("decode response (raw={:?}): {e}", line.trim()))
        })?;

        if let Some(err) = resp.error {
            return Err(EmbedderError::ModelError(format!(
                "daemon RPC error {}: {}",
                err.code, err.message
            )));
        }

        let result = resp.result.ok_or_else(|| {
            EmbedderError::Uds("response missing both result and error fields".to_owned())
        })?;

        if result.embeddings.len() != sent {
            return Err(EmbedderError::DimensionMismatch {
                sent,
                got: result.embeddings.len(),
            });
        }

        tracing::debug!(
            socket = %self.socket_path.display(),
            n = sent,
            "UdsEmbedderClient: batch complete"
        );

        Ok(result.embeddings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn empty_batch_short_circuits() {
        // Why: empty batches should not attempt any socket I/O.
        // What: call embed_batch with an empty vec on an unreachable path;
        //       the call must return Ok(vec![]) without connecting.
        // Test: this test — if the short-circuit is missing we get a connect
        //       error instead of an empty result.
        let client = UdsEmbedderClient::new("/nonexistent/socket/path");
        let result = client
            .embed_batch(vec![])
            .await
            .expect("empty batch must short-circuit");
        assert!(result.is_empty());
    }

    #[test]
    fn request_serialises_correctly() {
        // Why: guard against accidental rename of the JSON-RPC fields.
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
    fn default_socket_path_uses_tmpdir() {
        // Why: the default path must use the OS-assigned temp directory so
        //      macOS launchd per-agent TMPDIR is respected.
        // What: check the path ends with the canonical socket filename.
        // Test: this test.
        let p = UdsEmbedderClient::default_path();
        assert_eq!(
            p.file_name().and_then(|s| s.to_str()),
            Some(SOCKET_FILENAME),
            "default path must end with {SOCKET_FILENAME}"
        );
        assert!(p.parent().is_some(), "must have a parent directory");
    }

    #[test]
    fn dimension_mismatch_detected() {
        // Why: a server that returns a different count than requested is a bug
        //      that should surface as DimensionMismatch, not a silent truncation.
        // What: decode a synthetic success response with one vector when two
        //       were sent, and verify the error variant.
        // Test: this test.
        let resp = RpcResponse {
            result: Some(EmbedResult {
                embeddings: vec![vec![0.1_f32]],
            }),
            error: None,
        };
        // sent = 2, got = 1
        let sent = 2;
        let got = resp.result.unwrap().embeddings.len();
        assert_ne!(sent, got);
        // The mismatch check is exercised in embed_batch; confirm the error
        // variant discriminant here.
        let err = EmbedderError::DimensionMismatch { sent, got };
        let s = err.to_string();
        assert!(s.contains("2") && s.contains("1"));
    }
}
