//! UDS JSON-RPC client for the `trusty-embed-daemon` subprocess.
//!
//! Why: trusty-memory's in-process embedder serialises all recall queries
//! through one ONNX mutex, hitting ~894 ms p99 under 50 concurrent callers.
//! Delegating embedding to a dedicated subprocess (with a batching queue)
//! eliminates the mutex contention. `EmbedClient` is the in-process surface
//! callers use to talk to that subprocess without depending on fastembed/ORT
//! themselves.
//!
//! What: a small async client that
//!   - opens a fresh `UnixStream` per call (no connection pool — keeps the
//!     initial version simple; latency on local UDS is microseconds),
//!   - sends one newline-terminated JSON-RPC `embed` request,
//!   - reads one newline-terminated response and returns the embedding(s).
//!
//! Test: unit tests in this module cover request shape and path defaults.
//! End-to-end coverage lives in `crates/trusty-embed-daemon/tests/`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// Default socket filename — must agree with `trusty-embed-daemon`.
///
/// Why: keeps the daemon and the client honest with a single literal.
/// Changing it in one place without the other would silently break the
/// integration.
/// What: `trusty-embed.sock`.
/// Test: `default_path_uses_tmpdir` confirms the resolved path ends with
/// this filename.
pub const SOCKET_FILENAME: &str = "trusty-embed.sock";

/// JSON-RPC method name expected by the daemon.
const METHOD_EMBED: &str = "embed";

/// JSON-RPC protocol version string.
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

/// Async client for the embed daemon.
///
/// Why: a tiny value type makes the client cheap to construct, clone, and
/// pass around. It owns nothing other than the socket path, so two callers
/// can share the same `EmbedClient` (or each hold their own) freely.
/// What: holds the resolved socket path and provides `embed_one` /
/// `embed_many` async methods.
/// Test: covered by the daemon's integration tests (single-end-to-end check
/// would require spawning a real subprocess from a library test, which we
/// avoid here).
#[derive(Debug, Clone)]
pub struct EmbedClient {
    socket_path: PathBuf,
}

impl EmbedClient {
    /// Construct a client targeting the given socket path.
    ///
    /// Why: explicit-path callers (test harnesses, alternate deployment
    /// layouts) want to avoid the env-var-based default.
    /// What: stores the path verbatim; no I/O happens until the first
    /// `embed_*` call.
    /// Test: trivially covered by every other test that constructs a client.
    pub fn new(socket_path: PathBuf) -> Self {
        Self { socket_path }
    }

    /// Default socket path under `$TMPDIR` (or `/tmp` if unset).
    ///
    /// Why: matches the daemon's own default so callers can call
    /// `EmbedClient::new(EmbedClient::default_path())` without coordinating.
    /// What: `<TMPDIR>/trusty-embed.sock`, falling back to `/tmp` when
    /// `TMPDIR` is unset or empty.
    /// Test: `default_path_uses_tmpdir`.
    pub fn default_path() -> PathBuf {
        let dir = match std::env::var("TMPDIR") {
            Ok(p) if !p.trim().is_empty() => PathBuf::from(p),
            _ => PathBuf::from("/tmp"),
        };
        dir.join(SOCKET_FILENAME)
    }

    /// The socket path this client is configured to use.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Embed a single text.
    ///
    /// Why: most call sites (e.g. recall queries) embed one string at a time.
    /// What: delegates to `embed_many` with a single-element slice and pops
    /// the lone vector out of the result.
    /// Test: covered indirectly by the daemon's integration test.
    pub async fn embed_one(&self, text: &str) -> Result<Vec<f32>> {
        let mut out = self.embed_many(&[text.to_string()]).await?;
        out.pop()
            .ok_or_else(|| anyhow!("embed daemon returned no embedding for non-empty input"))
    }

    /// Embed a batch of texts.
    ///
    /// Why: callers that already have a batch (e.g. indexing chunks) amortise
    /// the IPC round trip across the batch.
    /// What: opens a fresh `UnixStream`, writes one JSON-RPC frame, reads
    /// one response frame, decodes the embeddings array.
    /// Errors propagate with context so callers can distinguish connect,
    /// I/O, decode, and remote failures.
    /// Test: unit-tested via the request-shape test; end-to-end via the
    /// daemon's integration test.
    pub async fn embed_many(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        // Open a fresh connection per call. The kernel resolves the path,
        // checks the socket, and returns a ready-to-write stream — typically
        // sub-millisecond on local UDS, so the simplicity buys us a lot
        // without measurable latency cost.
        let stream = UnixStream::connect(&self.socket_path)
            .await
            .with_context(|| {
                format!("connect to embed daemon at {}", self.socket_path.display())
            })?;
        let (read_half, mut write_half) = stream.into_split();

        // Build the request. The id is a per-call counter is unnecessary since
        // we read one response per write; pin the id to 1.
        let req = RpcRequest {
            jsonrpc: JSONRPC_VERSION,
            method: METHOD_EMBED,
            params: EmbedParams { texts },
            id: 1,
        };
        let mut payload = serde_json::to_vec(&req).context("serialise embed JSON-RPC request")?;
        payload.push(b'\n');
        write_half
            .write_all(&payload)
            .await
            .context("write embed JSON-RPC request to daemon")?;
        // Half-close the write side so the daemon knows we are done sending —
        // it still keeps the read side open for the response.
        write_half
            .shutdown()
            .await
            .context("half-close write side of embed daemon socket")?;

        // Read exactly one newline-terminated response frame.
        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        let n = reader
            .read_line(&mut line)
            .await
            .context("read embed JSON-RPC response from daemon")?;
        if n == 0 {
            anyhow::bail!("embed daemon closed connection before responding");
        }

        let resp: RpcResponse = serde_json::from_str(line.trim())
            .with_context(|| format!("decode embed JSON-RPC response (raw={})", line.trim()))?;

        if let Some(err) = resp.error {
            anyhow::bail!("embed daemon error {}: {}", err.code, err.message);
        }
        let result = resp
            .result
            .ok_or_else(|| anyhow!("embed daemon response missing result and error fields"))?;
        if result.embeddings.len() != texts.len() {
            anyhow::bail!(
                "embed daemon returned {} embeddings, expected {}",
                result.embeddings.len(),
                texts.len()
            );
        }
        Ok(result.embeddings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_path_uses_tmpdir() {
        let p = EmbedClient::default_path();
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
    fn request_serialises_as_jsonrpc_2_0() {
        let texts = vec!["hello".to_string(), "world".to_string()];
        let req = RpcRequest {
            jsonrpc: JSONRPC_VERSION,
            method: METHOD_EMBED,
            params: EmbedParams { texts: &texts },
            id: 1,
        };
        let s = serde_json::to_string(&req).unwrap();
        // Spot-check the wire shape — version, method, and texts must be
        // present and unmangled.
        assert!(s.contains("\"jsonrpc\":\"2.0\""));
        assert!(s.contains("\"method\":\"embed\""));
        assert!(s.contains("\"texts\":[\"hello\",\"world\"]"));
        assert!(s.contains("\"id\":1"));
    }

    #[tokio::test]
    async fn embed_many_empty_returns_empty_without_connect() {
        // The empty-input path must short-circuit before attempting a connect,
        // so it works even when no daemon is running. Using a definitely-
        // invalid path proves the short-circuit.
        let client = EmbedClient::new(PathBuf::from("/nonexistent/socket"));
        let got = client.embed_many(&[]).await.unwrap();
        assert!(got.is_empty());
    }
}
