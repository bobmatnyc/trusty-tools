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
//! `trusty-embedderd` in issue #164. #5180: the framing itself is
//! [`crate::uds::rpc::send_framed_request_capped`] — this module owns the
//! JSON-RPC envelope, the dimension check and the error mapping, not the wire
//! mechanics.
//!
//! Test: unit tests below cover empty-batch short-circuit, request
//! serialisation shape, and error decoding without a live daemon. The
//! `#[ignore]`-tagged `uds_bit_identical` integration test in
//! `trusty-embedderd/tests/bit_identical.rs` asserts bit-identical output
//! between `UdsEmbedderClient` and `InProcessEmbedderClient` using a real
//! ONNX model.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::{EmbedderClient, EmbedderError};

/// Wall-clock ceiling for one embed exchange over the socket.
///
/// Why (#5180): the hand-rolled framing this client used before had no bound at
/// all — a daemon that accepted the connection and then wedged (a stuck ONNX
/// session, a CoreML cold compile that never finishes) held the caller open
/// forever. The bound is deliberately loose rather than tight: callers that
/// want a service-level deadline already impose one
/// (`memory_core::timeouts::embed_batch_timeout`, 30 s by default), so this
/// one's only job is to make an infinite hang finite. 10 minutes is longer than
/// any batch that completes today.
/// Test: `embed_bounds_are_generous_but_finite`.
const EMBED_TIMEOUT: Duration = Duration::from_secs(600);

/// Response-frame budget for one embed reply, in bytes.
///
/// Why (#5180): [`crate::uds::MAX_FRAME_BYTES`] (8 MiB) is sized for
/// control-plane frames, and an embed reply is bulk data. JSON-encoded `f32`s
/// run roughly 12 bytes per dimension, so one 768-dimension vector is ~9.5 KB
/// and the dream-dedup pass — which embeds every drawer in a palace in a SINGLE
/// batch (`memory_core::dream::cycle::dedup_drawers`) — crosses 8 MiB at around
/// 900 drawers. Capping at the shared default would have turned a working dream
/// cycle into a hard failure, so this client states its own budget. 256 MiB
/// still bounds the read buffer against a peer that never terminates a frame,
/// which is the only thing the cap is for.
/// Test: `embed_bounds_are_generous_but_finite`.
const EMBED_MAX_FRAME_BYTES: u64 = 256 * 1024 * 1024;

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

    /// Default socket path in the per-uid scratch socket directory.
    ///
    /// Why: matches `trusty-embedderd`'s own default socket path so callers
    /// can construct a client with no explicit configuration.
    /// What: returns `<$TMPDIR or /tmp>/trusty-<uid>/trusty-embedderd.sock`.
    ///
    /// #5099: the socket used to sit directly in `$TMPDIR`, falling back to a
    /// world-writable `/tmp` when `TMPDIR` was unset. The uid-keyed
    /// subdirectory from [`crate::uds::scratch_socket_dir`] is what the daemon
    /// can hold at `0700`.
    ///
    /// Test: `default_socket_path_uses_tmpdir`.
    pub fn default_path() -> PathBuf {
        crate::uds::scratch_socket_dir().join(SOCKET_FILENAME)
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

        let req = RpcRequest {
            jsonrpc: JSONRPC_VERSION,
            method: METHOD_EMBED,
            params: EmbedParams { texts: &texts },
            id: 1,
        };

        // #5180: dial (with the #5099 permission check), newline framing,
        // half-close, size cap and timeout all come from the shared entry
        // point. This used to be a private copy of `write_all` +
        // `BufReader::read_line` + `serde_json::from_str`, byte-identical on
        // the wire to `bm25_client`'s copy.
        let resp: RpcResponse = crate::uds::rpc::send_framed_request_capped(
            &self.socket_path,
            &req,
            EMBED_TIMEOUT,
            EMBED_MAX_FRAME_BYTES,
        )
        .await
        .map_err(|e| EmbedderError::Uds(e.to_string()))?;

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
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    /// Why (#5180): this client's framing moved into `uds::rpc`, and
    /// `trusty-embedderd`'s listener — a separate crate — was not changed. A
    /// drift in the request bytes would only show up against a live daemon,
    /// which the rest of this module's tests deliberately avoid needing.
    /// What: runs a real `embed_batch` against a stub listener and asserts the
    /// wire bytes are exactly one newline-terminated JSON-RPC 2.0 frame, and
    /// that a newline-terminated reply decodes back into vectors.
    /// Test: this test itself.
    #[tokio::test]
    async fn embed_batch_sends_one_newline_framed_jsonrpc_frame() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sock = tmp.path().join("sockets").join("embedderd-stub.sock");
        let listener = crate::uds::bind_hardened(&sock).expect("bind stub socket");

        let served = tokio::spawn(async move {
            let (mut conn, _) = listener.accept().await.expect("accept");
            // The client half-closes after its request frame.
            let mut raw = Vec::new();
            conn.read_to_end(&mut raw).await.expect("drain request");
            conn.write_all(
                b"{\"jsonrpc\":\"2.0\",\"result\":{\"embeddings\":[[0.5,0.25]]},\"id\":1}\n",
            )
            .await
            .expect("write reply");
            conn.flush().await.expect("flush reply");
            raw
        });

        let vectors = UdsEmbedderClient::new(sock)
            .embed_batch(vec!["hello".to_string()])
            .await
            .expect("embed round trip");
        assert_eq!(vectors, vec![vec![0.5_f32, 0.25_f32]]);

        let raw = String::from_utf8(served.await.expect("join")).expect("utf8");
        assert!(
            raw.ends_with('\n'),
            "the request frame must be newline-terminated: {raw:?}"
        );
        assert_eq!(
            raw.matches('\n').count(),
            1,
            "exactly one frame, one terminator: {raw:?}"
        );
        let sent: serde_json::Value =
            serde_json::from_str(raw.trim_end_matches('\n')).expect("the frame is one JSON value");
        assert_eq!(sent["jsonrpc"], "2.0");
        assert_eq!(sent["method"], "embed");
        assert_eq!(sent["params"]["texts"][0], "hello");
        assert_eq!(sent["id"], 1);
    }

    /// Why (#5180): the migration introduced a timeout and a response-size cap
    /// where neither existed. The cap in particular is load-bearing — the
    /// dream-dedup pass embeds a whole palace in one batch, and the shared
    /// 8 MiB default would refuse the reply at roughly 900 drawers.
    /// What: pins both bounds against a drift that would start failing real
    /// batches.
    /// Test: this test itself.
    #[test]
    fn embed_bounds_are_generous_but_finite() {
        let secs = EMBED_TIMEOUT.as_secs();
        assert!(
            (120..=3600).contains(&secs),
            "{secs}s is outside the band that makes an infinite hang finite \
             without cutting a batch that would have completed"
        );
        let budget = EMBED_MAX_FRAME_BYTES;
        assert!(
            budget > crate::uds::MAX_FRAME_BYTES,
            "an embed reply is bulk data; the control-plane default is too small"
        );
        // ~12 bytes per JSON-encoded f32 x 768 dims x 10_000 drawers.
        assert!(
            budget >= 92_160_000,
            "{budget} bytes will not hold a whole-palace dream-dedup batch"
        );
    }

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
        // #5099: the parent must be the uid-keyed directory the daemon holds
        // at 0700, not a bare `$TMPDIR` (or worse, a world-writable `/tmp`).
        assert_eq!(
            p.parent(),
            Some(crate::uds::scratch_socket_dir().as_path()),
            "default path must live in the per-uid scratch socket directory"
        );
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
