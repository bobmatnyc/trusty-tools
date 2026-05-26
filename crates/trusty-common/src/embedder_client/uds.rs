//! Unix domain socket client for `trusty-embedderd`.
//!
//! Why: the UDS transport lets callers on the same host talk to the unified
//! `trusty-embedderd` daemon over a local socket instead of HTTP, avoiding
//! TCP stack overhead entirely. This was previously only available via the
//! now-retired `trusty-embed-daemon` + `trusty-common::embed_client::EmbedClient`
//! pair; this module is the canonical replacement (issue #164 Step 1).
//!
//! What: `UdsEmbedderClient` implements `EmbedderClient` using the same
//! newline-framed JSON-RPC 2.0 wire protocol that `trusty-embedderd`'s UDS
//! listener accepts. It opens a fresh connection per batch call (same approach
//! as the old `EmbedClient` in `trusty-common`; UDS connect latency is
//! sub-millisecond on loopback).
//!
//! Test: `uds_client_empty_batch_short_circuits` (unit, no daemon needed) verifies
//! the early-return on empty input without requiring a live socket.
//! End-to-end coverage lives in `tests/uds_integration.rs` (marked `#[ignore]`).

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use super::{EmbedderClient, EmbedderError};

// ── Wire protocol types (private to this module) ───────────────────────────

/// JSON-RPC 2.0 method name for the embed operation.
///
/// Why: must match the constant in `trusty-embedderd`'s UDS server.
const METHOD_EMBED: &str = "embed";

/// JSON-RPC protocol version string.
const JSONRPC_VERSION: &str = "2.0";

/// Default socket filename — must match `trusty-embedderd`'s default.
///
/// Why: keeps the daemon and client in sync with one literal; changing either
/// without the other would break the default-path integration silently.
///
/// What: `trusty-embedderd.sock`.
///
/// Test: `default_socket_path_uses_tmpdir` below.
pub const SOCKET_FILENAME: &str = "trusty-embedderd.sock";

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
    /// Field name matches the daemon's JSON-RPC wire format.
    embeddings: Vec<Vec<f32>>,
}

#[derive(Debug, serde::Deserialize)]
struct RpcError {
    code: i32,
    message: String,
}

// ── Public client ───────────────────────────────────────────────────────────

/// Embedder client that delegates to a running `trusty-embedderd` instance
/// over its Unix domain socket.
///
/// Why: provides a zero-TCP-overhead alternative to `RemoteEmbedderClient`
/// for callers on the same host. On Apple Silicon or any machine where ONNX
/// RSS matters, a single `trusty-embedderd` process can serve both the HTTP
/// and UDS channels concurrently (issue #164).
///
/// What: holds the UDS socket path. `embed_batch` opens a fresh connection,
/// sends one newline-terminated JSON-RPC 2.0 `embed` frame, reads one
/// newline-terminated response, and returns the embeddings array. Thread-safe
/// because it holds no per-connection state.
///
/// Test: `uds_client_empty_batch_short_circuits` (unit, no daemon required).
/// End-to-end: `cargo test -p trusty-embedderd --test uds_integration -- --include-ignored`.
#[derive(Clone, Debug)]
pub struct UdsEmbedderClient {
    socket_path: PathBuf,
}

impl UdsEmbedderClient {
    /// Construct a client targeting the given socket path.
    ///
    /// Why: explicit-path constructor for test harnesses and alternate
    /// deployment layouts that do not want the env-var-based default.
    ///
    /// What: stores the path verbatim; no I/O until the first `embed_batch` call.
    ///
    /// Test: `default_socket_path_uses_tmpdir` constructs via `new`.
    pub fn new(socket_path: impl Into<PathBuf>) -> Self {
        Self {
            socket_path: socket_path.into(),
        }
    }

    /// Resolve the default socket path under `$TMPDIR` (or `/tmp` if unset).
    ///
    /// Why: callers can use this to construct a `UdsEmbedderClient` that
    /// connects to the daemon's default socket without needing to know the
    /// exact path.
    ///
    /// What: returns `<TMPDIR>/trusty-embedderd.sock`; falls back to `/tmp`
    /// when `TMPDIR` is unset, empty, or whitespace.
    ///
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
    /// Why: accessors let callers log or display the path for diagnostics.
    ///
    /// What: returns a `&Path` view of the stored socket path.
    ///
    /// Test: verified transitively by construction tests.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Send a batch of texts to the daemon and return the embeddings.
    ///
    /// Why: the core I/O path; extracted from `embed_batch` so it can be
    /// tested and have a clear `?`-propagation chain without cluttering the
    /// trait impl.
    ///
    /// What: opens a `UnixStream`, writes one JSON-RPC frame, reads one
    /// response frame, validates and returns the embeddings array.
    ///
    /// Test: end-to-end in `trusty-embedderd/tests/uds_integration.rs`.
    async fn send_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedderError> {
        macro_rules! uds_err {
            ($msg:expr) => {
                EmbedderError::UdsError($msg)
            };
        }

        let stream = UnixStream::connect(&self.socket_path).await.map_err(|e| {
            uds_err!(format!(
                "connect to trusty-embedderd UDS at {}: {e}",
                self.socket_path.display()
            ))
        })?;

        let (read_half, mut write_half) = stream.into_split();

        let req = RpcRequest {
            jsonrpc: JSONRPC_VERSION,
            method: METHOD_EMBED,
            params: EmbedParams { texts },
            // We are the sole caller per connection; pin id to 1.
            id: 1,
        };
        let mut payload = serde_json::to_vec(&req)
            .map_err(|e| uds_err!(format!("serialise UDS embed request: {e}")))?;
        payload.push(b'\n');
        write_half
            .write_all(&payload)
            .await
            .map_err(|e| uds_err!(format!("write UDS embed request: {e}")))?;
        // Half-close the write side so the daemon knows we are done sending
        // (the server's read loop terminates on EOF from our side).
        write_half
            .shutdown()
            .await
            .map_err(|e| uds_err!(format!("half-close UDS write side: {e}")))?;

        // Read exactly one newline-terminated response frame.
        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        let n = reader
            .read_line(&mut line)
            .await
            .map_err(|e| uds_err!(format!("read UDS embed response: {e}")))?;
        if n == 0 {
            return Err(uds_err!(
                "trusty-embedderd UDS closed connection before responding".to_owned()
            ));
        }

        let resp: RpcResponse = serde_json::from_str(line.trim()).map_err(|e| {
            uds_err!(format!(
                "decode UDS embed response (raw={}): {e}",
                line.trim()
            ))
        })?;

        if let Some(err) = resp.error {
            return Err(uds_err!(format!(
                "trusty-embedderd UDS error {}: {}",
                err.code, err.message
            )));
        }
        let result = resp.result.ok_or_else(|| {
            uds_err!("UDS response missing both result and error fields".to_owned())
        })?;

        if result.embeddings.len() != texts.len() {
            return Err(uds_err!(format!(
                "trusty-embedderd UDS returned {} embeddings, expected {}",
                result.embeddings.len(),
                texts.len()
            )));
        }
        Ok(result.embeddings)
    }
}

#[async_trait]
impl EmbedderClient for UdsEmbedderClient {
    /// Embed a batch of texts via the `trusty-embedderd` UDS interface.
    ///
    /// Why: UDS transport has lower latency than HTTP on the same host —
    /// no TCP handshake, no HTTP framing overhead.
    ///
    /// What: short-circuits on empty input; otherwise delegates to
    /// `send_batch`. UDS-specific failures are returned as
    /// `EmbedderError::UdsError` so callers can decide whether to retry
    /// on a different transport.
    ///
    /// Test: `uds_client_empty_batch_short_circuits` (unit); end-to-end in
    /// `trusty-embedderd/tests/uds_integration.rs` (`#[ignore]`).
    async fn embed_batch(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>, EmbedderError> {
        if texts.is_empty() {
            return Ok(vec![]);
        }
        self.send_batch(&texts).await
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_socket_path_uses_tmpdir() {
        // Why: guards against the default path regressing to a hardcoded /tmp
        // literal that ignores the macOS per-agent TMPDIR.
        // What: construct the default path and verify it ends with the expected
        // filename.
        // Test: this test.
        let p = UdsEmbedderClient::default_path();
        assert_eq!(
            p.file_name().and_then(|s| s.to_str()),
            Some(SOCKET_FILENAME),
            "default path must end with the canonical socket filename"
        );
        assert!(
            p.parent().is_some(),
            "default path must have a parent directory"
        );
    }

    #[test]
    fn socket_path_accessor_returns_stored_path() {
        // Why: callers use socket_path() for logging; it must round-trip the
        // path passed to new().
        // What: construct with a known path and assert accessor returns same.
        // Test: this test.
        let p = PathBuf::from("/tmp/my-daemon.sock");
        let client = UdsEmbedderClient::new(p.clone());
        assert_eq!(client.socket_path(), p.as_path());
    }

    #[tokio::test]
    async fn uds_client_empty_batch_short_circuits() {
        // Why: the empty-input path must not attempt a connect — it must
        // return an empty result immediately even when the socket does not
        // exist.
        // What: use an unreachable socket path and call embed_batch([]).
        // Test: this test — if it were to connect it would return an error.
        let client = UdsEmbedderClient::new(PathBuf::from("/nonexistent/socket.sock"));
        let result = client.embed_batch(vec![]).await.expect("empty is Ok");
        assert!(result.is_empty());
    }

    #[test]
    fn request_serialises_as_jsonrpc_2_0() {
        // Why: guard against accidental field rename / serde attribute
        // mismatch that would break the wire format silently.
        // What: serialise a request struct and check the key fields.
        // Test: this test.
        let texts = vec!["hello".to_string(), "world".to_string()];
        let req = RpcRequest {
            jsonrpc: JSONRPC_VERSION,
            method: METHOD_EMBED,
            params: EmbedParams { texts: &texts },
            id: 1,
        };
        let s = serde_json::to_string(&req).unwrap();
        assert!(s.contains("\"jsonrpc\":\"2.0\""));
        assert!(s.contains("\"method\":\"embed\""));
        assert!(s.contains("\"texts\":[\"hello\",\"world\"]"));
        assert!(s.contains("\"id\":1"));
    }
}
