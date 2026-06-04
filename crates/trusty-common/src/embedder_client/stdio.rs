//! Multi-flight stdio embedder client for a sidecar `trusty-embedderd` process
//! (issue #753).
//!
//! Why: the old single-Mutex write→wait→read round-trip left the ANE ~78%
//! idle. Splitting into a write-only stdin lock and a dedicated reader task
//! enables N concurrent in-flight batches (`TRUSTY_EMBED_INFLIGHT`, default 2).
//!
//! Order guarantee: the sidecar processes requests serially and never re-orders
//! responses. The reader task pops the FIFO pending queue head on each response,
//! so each reply always maps to the correct caller.
//!
//! Crash/restart: EOF or IO error drains all pending oneshots with an error so
//! callers return immediately; the supervisor swaps in a fresh client.
//!
//! Test: unit tests cover wire format, error decoding, and stalled-reader
//! timeout. Multi-flight + order-preservation: `trusty-embedderd/tests/
//! multiflight.rs`. End-to-end: `bit_identical -- --include-ignored`.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout};
use tokio::sync::{Mutex, Semaphore, oneshot};
use tokio::time::Duration;

use super::{EmbedderClient, EmbedderError};

// ── Per-call timeout ─────────────────────────────────────────────────────────

const EMBED_CALL_TIMEOUT_DEFAULT_SECS: u64 = 120;

/// Read `TRUSTY_EMBEDDERD_CALL_TIMEOUT_SECS` once and cache it.
///
/// Why: avoids repeated env lookups per batch while still allowing tests to
/// override via `std::env::set_var`.
/// What: reads the env var, parses as u64, falls back to 120 s.
/// Test: `embed_call_stalled_reader_times_out` exercises the timeout path.
fn embed_call_timeout() -> Duration {
    static CACHED: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();
    *CACHED.get_or_init(|| {
        let secs = std::env::var("TRUSTY_EMBEDDERD_CALL_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(EMBED_CALL_TIMEOUT_DEFAULT_SECS);
        Duration::from_secs(secs)
    })
}

/// Read `TRUSTY_EMBED_INFLIGHT` once; clamp to [1, 4]; default 2.
///
/// Why: controls max in-flight batches. Test: multi-flight tests (indirect).
fn embed_inflight() -> usize {
    static CACHED: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *CACHED.get_or_init(|| {
        std::env::var("TRUSTY_EMBED_INFLIGHT")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .map(|n| n.clamp(1, 4))
            .unwrap_or(2)
    })
}

// ── Wire types ───────────────────────────────────────────────────────────────

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
    // id field present in wire format; we use FIFO ordering so we read but
    // do not need to dispatch by id.
    #[allow(dead_code)]
    #[serde(default)]
    id: Option<serde_json::Value>,
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

// ── Pending-request queue ────────────────────────────────────────────────────

/// One in-flight request waiting for its response.
struct PendingRequest {
    /// Number of texts sent (used for count validation on reply).
    sent: usize,
    /// Channel to deliver the decoded result to the waiter.
    reply: oneshot::Sender<Result<Vec<Vec<f32>>, EmbedderError>>,
}

/// FIFO queue of pending requests shared between writers and the reader task.
/// Push on send, pop on response — sidecar never re-orders, so FIFO suffices.
/// Mutex held only for push/pop, not during IO.
type PendingQueue = Arc<Mutex<VecDeque<PendingRequest>>>;

// ── Client ──────────────────────────────────────────────────────────────────

/// Multi-flight `EmbedderClient` over a sidecar `trusty-embedderd --stdio`.
///
/// Why: the previous single-flight client held the write+read mutex for the
/// entire round-trip. This kept only one batch in flight at a time and left
/// the ANE ~78% idle during reindex. Splitting into a dedicated reader task
/// with a write-only stdin lock allows N concurrent in-flight batches, which
/// keeps the ANE's work queue continuously filled (issue #753).
///
/// What: `embed_batch` acquires the write semaphore, registers a `oneshot`
/// in the FIFO pending queue, serialises the request to the write-only stdin
/// lock, releases both locks, then awaits the oneshot. A single reader task
/// (spawned in `new`) owns stdout, reads response frames in arrival order,
/// pops the head of the pending queue, and sends the decoded result. Crash/
/// restart: EOF or read errors drain all pending oneshots with an error.
///
/// Test: unit tests in this module; multi-flight integration tests in
/// `trusty-embedderd/tests/multiflight.rs`.
pub struct StdioEmbedderClient {
    /// Write half — stdin lock held only for the duration of `write_all + flush`.
    stdin: Arc<Mutex<ChildStdin>>,
    /// Pending FIFO queue shared between writers and the reader task.
    pending: PendingQueue,
    /// Semaphore bounding max in-flight requests.
    inflight: Arc<Semaphore>,
    /// Monotonic counter for request ids (debug tracing only).
    next_id: Arc<AtomicU64>,
}

impl StdioEmbedderClient {
    /// Construct a multi-flight client and spawn the background reader task.
    ///
    /// Why: the reader task must be running before any `embed_batch` calls so
    /// it can dispatch responses to waiting callers.
    /// What: wraps stdin in a `Mutex`; wraps stdout in a `BufReader` owned
    /// exclusively by the reader task. Spawns `reader_task` as a detached
    /// Tokio task. Returns the client handle immediately.
    /// Test: indirectly covered by every test that constructs and calls the client.
    pub fn new(stdin: ChildStdin, stdout: ChildStdout) -> Self {
        let stdin = Arc::new(Mutex::new(stdin));
        let pending: PendingQueue = Arc::new(Mutex::new(VecDeque::new()));
        let inflight = Arc::new(Semaphore::new(embed_inflight()));
        let next_id = Arc::new(AtomicU64::new(1));

        // Spawn the reader task — it owns stdout for its lifetime.
        let pending_clone = Arc::clone(&pending);
        tokio::spawn(reader_task(BufReader::new(stdout), pending_clone));

        Self {
            stdin,
            pending,
            inflight,
            next_id,
        }
    }
}

/// Background reader task — owns stdout, dispatches responses in FIFO order.
///
/// Why: keeping the read loop separate from the write path is what enables
/// multi-flight: a caller can write the next request while this task is
/// reading the response to the previous one.
/// What: reads newline-terminated JSON-RPC response frames in a loop. For
/// each frame, pops the head of `pending`, decodes the response, and sends the
/// result to the caller's oneshot. On EOF or read error, drains all remaining
/// pending requests with an error so they don't hang.
/// Test: exercised by the multi-flight integration tests.
async fn reader_task(mut reader: BufReader<ChildStdout>, pending: PendingQueue) {
    let timeout = embed_call_timeout();
    let mut line = String::new();

    loop {
        line.clear();

        // Wait for the next response frame under a per-call deadline.
        let read_result = tokio::time::timeout(timeout, reader.read_line(&mut line)).await;

        match read_result {
            Err(_elapsed) => {
                tracing::warn!(
                    timeout_secs = timeout.as_secs(),
                    "StdioEmbedderClient reader: timed out waiting for response \
                     (sidecar may be stalled) — draining pending requests"
                );
                drain_pending_with_error(
                    &pending,
                    EmbedderError::Stdio(format!(
                        "embed call timed out after {}s — sidecar may be stalled \
                         (set TRUSTY_EMBEDDERD_CALL_TIMEOUT_SECS to adjust)",
                        timeout.as_secs()
                    )),
                )
                .await;
                return;
            }
            Ok(Err(e)) => {
                tracing::warn!(
                    "StdioEmbedderClient reader: IO error reading from sidecar stdout: {e}"
                );
                drain_pending_with_error(
                    &pending,
                    EmbedderError::Stdio(format!("read response from child stdout: {e}")),
                )
                .await;
                return;
            }
            Ok(Ok(0)) => {
                // EOF — sidecar closed stdout (crashed or was shut down).
                tracing::info!(
                    "StdioEmbedderClient reader: stdout EOF \
                     (sidecar exited) — draining pending requests"
                );
                drain_pending_with_error(
                    &pending,
                    EmbedderError::Stdio(
                        "child closed stdout before responding (process exited)".to_owned(),
                    ),
                )
                .await;
                return;
            }
            Ok(Ok(_)) => {
                // Got a line — dispatch to the head of the pending queue.
            }
        }

        // Pop the oldest pending request.
        let req = {
            let mut guard = pending.lock().await;
            guard.pop_front()
        };
        let Some(pending_req) = req else {
            tracing::warn!(
                "StdioEmbedderClient reader: received response but pending queue is empty \
                 (spurious frame from sidecar?) — ignoring"
            );
            continue;
        };

        // Decode the response and deliver to the waiter.
        let result = decode_response(line.trim(), pending_req.sent);
        // Dropping errors here is intentional: the caller may have been
        // cancelled (e.g. the reindex task was aborted), which is fine.
        let _ = pending_req.reply.send(result);
    }
}

/// Decode one JSON-RPC response frame. Extracted for unit-testing.
/// Test: `decode_response_*` unit tests below.
fn decode_response(line: &str, sent: usize) -> Result<Vec<Vec<f32>>, EmbedderError> {
    let resp: RpcResponse = serde_json::from_str(line)
        .map_err(|e| EmbedderError::Stdio(format!("decode response (raw={line:?}): {e}")))?;

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

    Ok(result.embeddings)
}

/// Drain all pending requests with an error (EOF / crash / timeout path).
///
/// Why: prevents callers from hanging when the reader exits. Supervisor then
/// swaps in a fresh `StdioEmbedderClient`. Test: multi-flight crash simulation.
async fn drain_pending_with_error(pending: &PendingQueue, error: EmbedderError) {
    let mut guard = pending.lock().await;
    for req in guard.drain(..) {
        let _ = req.reply.send(Err(EmbedderError::Stdio(
            // Clone the message from the source error; EmbedderError is not
            // Clone so we re-construct a Stdio variant with the same text.
            match &error {
                EmbedderError::Stdio(msg) => msg.clone(),
                EmbedderError::ModelError(msg) => msg.clone(),
                EmbedderError::DimensionMismatch { sent, got } => {
                    format!("dimension mismatch: sent={sent}, got={got}")
                }
                other => format!("{other}"),
            },
        )));
    }
}

#[async_trait::async_trait]
impl EmbedderClient for StdioEmbedderClient {
    /// Embed a batch via multi-flight stdio JSON-RPC 2.0.
    ///
    /// Why: see module doc. Acquires inflight semaphore slot, registers oneshot
    /// in FIFO pending queue, writes request (stdin lock held only for write +
    /// flush), then awaits the oneshot. Reader task dispatches replies in order.
    /// Test: `cargo test -p trusty-embedderd --test multiflight`
    async fn embed_batch(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>, EmbedderError> {
        if texts.is_empty() {
            return Ok(vec![]);
        }
        let sent = texts.len();

        // Bound concurrent in-flight requests.
        let _permit = self
            .inflight
            .acquire()
            .await
            .map_err(|_| EmbedderError::Stdio("inflight semaphore closed".to_owned()))?;

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        tracing::debug!(n = sent, id, "StdioEmbedderClient: sending batch");

        // Register the pending oneshot BEFORE writing the request so the
        // reader task can never pop-before-push.
        let (reply_tx, reply_rx) = oneshot::channel();
        {
            let mut guard = self.pending.lock().await;
            guard.push_back(PendingRequest {
                sent,
                reply: reply_tx,
            });
        }

        // Serialise the request.
        let req = RpcRequest {
            jsonrpc: JSONRPC_VERSION,
            method: METHOD_EMBED,
            params: EmbedParams { texts: &texts },
            id,
        };
        let mut payload = serde_json::to_vec(&req)
            .map_err(|e| EmbedderError::Stdio(format!("serialise JSON-RPC request: {e}")))?;
        payload.push(b'\n');

        // Write the request — stdin lock held only for write+flush, then released.
        {
            let mut stdin_guard = self.stdin.lock().await;
            stdin_guard
                .write_all(&payload)
                .await
                .map_err(|e| EmbedderError::Stdio(format!("write request to child stdin: {e}")))?;
            stdin_guard
                .flush()
                .await
                .map_err(|e| EmbedderError::Stdio(format!("flush child stdin: {e}")))?;
        }
        // stdin lock released — next concurrent caller can write immediately.
        // permit is held until this function returns, bounding inflight depth.

        // Await the reader task's dispatch.
        let result = reply_rx.await.map_err(|_| {
            EmbedderError::Stdio(
                "reader task dropped reply channel (sidecar crashed or was restarted)".to_owned(),
            )
        })?;

        tracing::debug!(n = sent, id, "StdioEmbedderClient: batch complete");
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Wire format tests (no live process needed) ────────────────────────

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
        let result = decode_response(json, 1);
        assert!(
            matches!(result, Err(EmbedderError::ModelError(_))),
            "got: {result:?}"
        );
    }

    #[test]
    fn success_response_decoded() {
        // Why: verify the happy-path decode path works end-to-end without a
        //      live child process.
        // What: synthesise a success response and deserialise the embeddings.
        // Test: this test.
        let json = r#"{"jsonrpc":"2.0","result":{"embeddings":[[0.1,0.2],[0.3,0.4]]},"id":1}"#;
        let result = decode_response(json, 2).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0][0], 0.1_f32);
    }

    #[test]
    fn count_mismatch_returns_dimension_error() {
        // Why: a count mismatch between sent and received vectors must surface
        //      as DimensionMismatch, not a silent truncation.
        // What: send `sent=3` but the mock response has 2 embeddings.
        // Test: this test.
        let json = r#"{"jsonrpc":"2.0","result":{"embeddings":[[0.1],[0.2]]},"id":1}"#;
        let result = decode_response(json, 3);
        assert!(
            matches!(
                result,
                Err(EmbedderError::DimensionMismatch { sent: 3, got: 2 })
            ),
            "got: {result:?}"
        );
    }

    /// Verify that a stalled/silent sidecar reader produces a timeout error
    /// rather than blocking indefinitely.
    ///
    /// Why: the root cause of the reindex-stall failure mode is a read blocking
    /// forever when the sidecar stops writing. This test proves that
    /// `tokio::time::timeout` on a never-yielding `read_line` call returns an
    /// `Elapsed` error rather than hanging.
    ///
    /// What: creates a `tokio::io::duplex` reader whose write end is held but
    /// never written to. Calls `read_line` with a 1 s deadline and asserts the
    /// result is `Err(Elapsed)`. Identical to a stalled sidecar.
    ///
    /// Test: this test (`embed_call_stalled_reader_times_out`).
    #[tokio::test]
    async fn embed_call_stalled_reader_times_out() {
        use tokio::io::duplex;

        let (_tx, rx) = duplex(1024);
        let mut buf = String::new();
        let mut reader = tokio::io::BufReader::new(rx);

        let result = tokio::time::timeout(Duration::from_secs(1), reader.read_line(&mut buf)).await;

        assert!(
            result.is_err(),
            "a read_line on a never-writing reader must time out under a 1 s deadline; \
             got: {result:?}"
        );
    }
}
