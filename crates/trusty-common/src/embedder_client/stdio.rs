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

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
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
        let timeout = embed_call_timeout();
        tokio::spawn(reader_task(BufReader::new(stdout), pending_clone, timeout));

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
/// result to the caller's oneshot. On timeout, drains pending requests, clears
/// the partial line buffer, and CONTINUES the loop — the task MUST NOT exit on
/// timeout (fix #763). The stale response the sidecar eventually writes will
/// arrive as a spurious frame once the ONNX call completes, and will be safely
/// discarded by the empty-queue guard below. On EOF or read error the task exits
/// and the supervisor handles respawn.
/// Test: `reader_task_survives_timeout_and_serves_next_request` proves the task
/// stays alive after a timeout and still delivers the next successful response.
async fn reader_task<R: AsyncBufRead + Unpin>(
    mut reader: R,
    pending: PendingQueue,
    timeout: Duration,
) {
    let mut line = String::new();

    loop {
        line.clear();

        // Wait for the next response frame under a per-call deadline.
        let read_result = tokio::time::timeout(timeout, reader.read_line(&mut line)).await;

        match read_result {
            Err(_elapsed) => {
                // CRITICAL FIX (#763): Do NOT return here. The old `return`
                // killed the reader task permanently on the first CUDA timeout,
                // causing every subsequent embed_batch to hang forever.
                //
                // Instead: drain pending callers with an error (they can retry),
                // clear the partial line buffer, and continue the loop. When the
                // sidecar eventually finishes the slow ONNX call and writes its
                // response to stdout, the reader will consume it and find an
                // empty pending queue — the "spurious frame" guard below will
                // discard it harmlessly.
                tracing::warn!(
                    timeout_secs = timeout.as_secs(),
                    "StdioEmbedderClient reader: timed out waiting for response \
                     (sidecar ONNX call exceeded {}s — CUDA OOM/BFCArena stall?) \
                     — draining pending requests and re-arming; reader task STAYS ALIVE",
                    timeout.as_secs()
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
                // Clear any partial data accumulated in `line` during the
                // timed-out read. The next loop iteration calls `line.clear()`
                // anyway but we do it here for clarity.
                line.clear();
                // continue — re-arm the timeout and wait for the next frame.
                // The sidecar's stale response for the timed-out batch will
                // arrive here eventually; the empty-queue guard discards it.
                continue;
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

// Tests are in a sibling file to keep this file under the 500-line cap.
// The submodule can access private items via `super::` (Rust child-module rule).
#[cfg(test)]
#[path = "stdio_tests.rs"]
mod tests;
