//! Multi-flight stdio embedder client for a sidecar `trusty-embedderd` process
//! (issue #753).
//!
//! Why: the old single-Mutex write→wait→read round-trip left the ANE ~78%
//! idle. Splitting into a write-only stdin lock and a dedicated reader task
//! enables N concurrent in-flight batches (`TRUSTY_EMBED_INFLIGHT`, default 2).
//!
//! Correlation guarantee: requests are matched to responses by JSON-RPC `id`
//! (a monotonic `u64`). The sidecar echoes the request `id` in every response.
//! The reader task looks up each response by id in a `HashMap`; a response
//! whose id is not in the map (orphaned stale frame from a timed-out request)
//! is discarded with a `warn!`. This eliminates the FIFO-misattribution hazard:
//! a stale late-arriving response can never be dispatched to a new request.
//!
//! Crash/restart: EOF or IO error drains all pending oneshots with an error so
//! callers return immediately; the supervisor swaps in a fresh client.
//!
//! Reader-task health (issues #1448/#1450): the reader task's `JoinHandle` is
//! retained by a small monitor task rather than dropped on a bare
//! fire-and-forget `tokio::spawn`. If the reader task dies unexpectedly
//! (panics, or returns via its EOF/IO-error arms) the monitor drains any
//! residual pending requests immediately and flips `unhealthy_signal` to
//! `true` (#1448) — the client no longer relies on individual per-call
//! timeouts to notice a dead reader. Separately, the reader task itself
//! tracks *consecutive* call timeouts with no intervening successful reply;
//! past `WEDGED_TIMEOUT_THRESHOLD` it flips the same `unhealthy_signal`,
//! covering the "process alive but wedged" case (#1450) that a bare
//! EOF/exit check can never see. `EmbedderSupervisor` observes
//! `unhealthy_signal` alongside `child.wait()` and forces a kill + respawn
//! either way.
//!
//! Post-death registration window (#1448 CRITICAL follow-up): the one-time
//! drain above only clears requests that existed *at the instant* the reader
//! died. A request registered afterward — e.g. during the kill/respawn window
//! while the OS process is still alive — would otherwise `write_all`
//! successfully and then hang on its reply forever, since no reader task is
//! left to answer it. `embed_batch` closes this window by (a) failing fast if
//! the client is already unhealthy before ever registering, and (b) racing
//! the reply against the unhealthy signal via `await_reply_or_unhealthy` so a
//! request stranded mid-wait still errors promptly.
//!
//! Test: unit tests cover wire format, error decoding, stalled-reader timeout,
//! the stale-frame misattribution proof, reader-task panic detection, the
//! wedged-timeout threshold, and the post-death registration window.
//! Multi-flight + correlation: `trusty-embedderd/tests/multiflight.rs`.
//! End-to-end: `bit_identical -- --include-ignored`.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout};
use tokio::sync::{Mutex, Semaphore, oneshot, watch};
use tokio::time::Duration;

use super::{EmbedderClient, EmbedderError};
use crate::embedder::{ExecutionProvider, resolve_expected_provider};

// ── Wedged-sidecar detection (#1450) ────────────────────────────────────────

/// Number of consecutive reader-task call timeouts (each already gated by
/// `TRUSTY_EMBEDDERD_CALL_TIMEOUT_SECS`, default 30s) with zero intervening
/// successful replies, past which the sidecar is declared wedged rather than
/// merely slow.
///
/// Why: a sidecar that is alive but stuck (e.g. the CUDA BFCArena stall from
/// #1428, or the CoreML/ANE scheduling stall reported on #1450) never exits,
/// so `EmbedderSupervisor`'s process-exit-based restart never fires. Three
/// consecutive timeouts tolerates one or two genuinely slow/oversized batches
/// while still declaring a truly wedged sidecar within a bounded window
/// (3 x 30s = 90s minimum at the default timeout).
/// What: compared against `TimeoutTracker`'s running count in `reader_task`.
/// Test: `wedge_threshold_fires_after_consecutive_timeouts`,
/// `wedge_threshold_resets_on_success` in `stdio_tests.rs`.
const WEDGED_TIMEOUT_THRESHOLD: u32 = 3;

/// Tracks consecutive reader-task call timeouts for wedge detection (#1450).
///
/// Why: needs to be shared between the reader task (which increments/resets
/// it) and is otherwise process-local, so a plain `AtomicU32` behind an `Arc`
/// is sufficient — no locking required.
/// What: `record_timeout` increments and reports whether the threshold was
/// just crossed (fires exactly once per accumulation); `record_success`
/// resets the count to zero.
/// Test: `wedge_threshold_*` in `stdio_tests.rs`.
struct TimeoutTracker {
    consecutive_timeouts: AtomicU32,
}

impl TimeoutTracker {
    fn new() -> Self {
        Self {
            consecutive_timeouts: AtomicU32::new(0),
        }
    }

    /// Record one real (non-idle) call timeout. Returns `true` exactly once,
    /// on the call that makes the running count equal `WEDGED_TIMEOUT_THRESHOLD`.
    fn record_timeout(&self) -> bool {
        let n = self.consecutive_timeouts.fetch_add(1, Ordering::AcqRel) + 1;
        n == WEDGED_TIMEOUT_THRESHOLD
    }

    /// Reset the consecutive-timeout count — called whenever the reader
    /// receives any response frame, proving the sidecar is still talking.
    fn record_success(&self) {
        self.consecutive_timeouts.store(0, Ordering::Release);
    }
}

// ── Per-call timeout ─────────────────────────────────────────────────────────

/// Default sidecar call timeout — lowered from 120 s to 30 s (issue #907).
/// Aligned with `TRUSTY_QUERY_TIMEOUT_SECS` so the embedder error surfaces
/// before the HTTP 408 fires. Reindex remains unbounded overall (the pipeline
/// retries per-timeout). Override via `TRUSTY_EMBEDDERD_CALL_TIMEOUT_SECS`.
const EMBED_CALL_TIMEOUT_DEFAULT_SECS: u64 = 30;

/// Read `TRUSTY_EMBEDDERD_CALL_TIMEOUT_SECS` once and cache it.
///
/// Why: avoids repeated env lookups per batch while still allowing tests to
/// override via `std::env::set_var`.
/// What: reads the env var, parses as u64, falls back to `EMBED_CALL_TIMEOUT_DEFAULT_SECS`.
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
    // id is parsed separately in `extract_response_id` (via the `IdOnly`
    // helper) rather than here, so we omit it from this struct to avoid a
    // dead-field lint. The sidecar echoes the request id in every response;
    // see `extract_response_id` for the correlation lookup.
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

// ── Pending-request map ──────────────────────────────────────────────────────

/// One in-flight request waiting for its response.
struct PendingRequest {
    /// Number of texts sent (used for count validation on reply).
    sent: usize,
    /// Channel to deliver the decoded result to the waiter.
    reply: oneshot::Sender<Result<Vec<Vec<f32>>, EmbedderError>>,
}

/// Id-keyed map of pending requests shared between writers and the reader task.
///
/// Why: using an id-keyed map instead of a FIFO queue prevents
/// stale-frame misattribution. After a request times out its entry is removed
/// from the map; when the sidecar eventually delivers the stale response, the
/// reader finds no map entry for that id and discards the frame harmlessly.
/// With a FIFO queue the stale frame would be popped and misattributed to
/// the *next* enqueued request, silently injecting wrong embeddings into the
/// HNSW index.
/// Mutex held only for insert/remove, not during IO.
type PendingMap = Arc<Mutex<HashMap<u64, PendingRequest>>>;

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
/// in the id-keyed pending map, serialises the request to the write-only stdin
/// lock, releases both locks, then awaits the oneshot. A single reader task
/// (spawned in `new`) owns stdout, reads response frames, looks up the pending
/// entry by the echoed JSON-RPC id, and dispatches the decoded result. Stale
/// frames (id not in map) are discarded with a `warn!`. Crash/restart: EOF or
/// read errors drain all pending oneshots with an error.
///
/// Test: unit tests in this module; multi-flight integration tests in
/// `trusty-embedderd/tests/multiflight.rs`.
pub struct StdioEmbedderClient {
    /// Write half — stdin lock held only for the duration of `write_all + flush`.
    stdin: Arc<Mutex<ChildStdin>>,
    /// Pending id-keyed map shared between writers and the reader task.
    pending: PendingMap,
    /// Semaphore bounding max in-flight requests.
    inflight: Arc<Semaphore>,
    /// Monotonic counter for request ids.
    next_id: Arc<AtomicU64>,
    /// Flips to `true` exactly once the reader task dies unexpectedly
    /// (#1448) or the sidecar is judged wedged via accumulating call
    /// timeouts (#1450). `EmbedderSupervisor` watches this alongside
    /// `child.wait()` to force a restart even when the child process is
    /// still alive. Retained here (rather than only inside the spawned
    /// tasks) so `unhealthy_signal()` can hand out fresh receivers.
    unhealthy_tx: watch::Sender<bool>,
    /// Keeps `unhealthy_tx`'s receiver count above zero for the client's
    /// whole lifetime. `watch::Sender::send` silently drops the value
    /// (returns `Err` without updating the stored value) when the receiver
    /// count is zero, so if every `unhealthy_signal()` receiver were dropped
    /// before the monitor task's `send(true)` fires, that transition would be
    /// lost for any receiver subscribed afterward. Never read directly.
    _unhealthy_rx_keepalive: watch::Receiver<bool>,
}

impl StdioEmbedderClient {
    /// Construct a multi-flight client and spawn the background reader task.
    ///
    /// Why: the reader task must be running before any `embed_batch` calls so
    /// it can dispatch responses to waiting callers.
    /// What: wraps stdin in a `Mutex`; wraps stdout in a `BufReader` owned
    /// exclusively by the reader task. Spawns `reader_task` and a monitor
    /// task that retains its `JoinHandle` (#1448). Returns the client handle
    /// immediately.
    /// Test: indirectly covered by every test that constructs and calls the client.
    pub fn new(stdin: ChildStdin, stdout: ChildStdout) -> Self {
        let stdin = Arc::new(Mutex::new(stdin));
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let inflight = Arc::new(Semaphore::new(embed_inflight()));
        let next_id = Arc::new(AtomicU64::new(1));
        let timeout = embed_call_timeout();

        let (unhealthy_tx, unhealthy_rx_keepalive) =
            spawn_reader_with_monitor(BufReader::new(stdout), Arc::clone(&pending), timeout);

        Self {
            stdin,
            pending,
            inflight,
            next_id,
            unhealthy_tx,
            _unhealthy_rx_keepalive: unhealthy_rx_keepalive,
        }
    }

    /// Hand out a fresh receiver for the reader-health signal (#1448/#1450).
    ///
    /// Why: `EmbedderSupervisor` needs to detect a dead-but-not-exited reader
    /// or a wedged sidecar without downcasting the `Arc<dyn EmbedderClient>`
    /// trait object — it calls this directly on the concrete
    /// `StdioEmbedderClient` right after `spawn_child` constructs one, before
    /// erasing it to the trait object.
    /// What: clones the `watch::Sender`'s receiver; the initial value is
    /// `false` and `changed()`/`borrow()` observe the transition to `true`
    /// exactly once, even if the receiver is created after the flip.
    /// Test: `reader_panic_drains_pending_and_signals_unhealthy`,
    /// `wedge_threshold_fires_after_consecutive_timeouts` in `stdio_tests.rs`.
    pub fn unhealthy_signal(&self) -> watch::Receiver<bool> {
        self.unhealthy_tx.subscribe()
    }
}

/// Spawn the reader task plus a monitor task that retains its `JoinHandle`
/// (#1448) and returns the shared unhealthy-signal `(Sender, Receiver)` pair.
///
/// Why: extracted from `StdioEmbedderClient::new` and kept generic over the
/// reader type so it is unit-testable with `tokio::io::duplex` (and a panic-
/// injecting fake reader) without needing a real child process. The returned
/// `Receiver` must be retained by the caller for the client's lifetime (see
/// `_unhealthy_rx_keepalive` doc) — `watch::Sender::send` silently drops its
/// value if the receiver count has reached zero.
/// What: spawns `reader_task` (which also owns a fresh `TimeoutTracker` for
/// #1450), then spawns a monitor task that awaits the reader's `JoinHandle`.
/// On a panic (`Err(JoinError)`), the monitor drains any pending requests
/// that the reader task's own EOF/IO-error arms never got to run — a bare
/// `tokio::spawn` with the handle dropped would otherwise leave those oneshots
/// (and therefore their `embed_batch` callers) hanging forever. Either way
/// (panic or normal exit) the monitor flips the sender to `true`.
/// Test: `reader_panic_drains_pending_and_signals_unhealthy`,
/// `reader_eof_exit_also_signals_unhealthy` in `stdio_tests.rs`.
fn spawn_reader_with_monitor<R>(
    reader: R,
    pending: PendingMap,
    timeout: Duration,
) -> (watch::Sender<bool>, watch::Receiver<bool>)
where
    R: AsyncBufRead + Unpin + Send + 'static,
{
    let (unhealthy_tx, unhealthy_rx) = watch::channel(false);
    let timeout_tracker = Arc::new(TimeoutTracker::new());

    let reader_handle = tokio::spawn(reader_task(
        reader,
        Arc::clone(&pending),
        timeout,
        Arc::clone(&timeout_tracker),
        unhealthy_tx.clone(),
    ));

    let monitor_pending = pending;
    let monitor_unhealthy_tx = unhealthy_tx.clone();
    tokio::spawn(async move {
        match reader_handle.await {
            Ok(()) => {
                // Normal exit (EOF/IO-error arms already drained pending
                // before returning) — nothing left to do beyond signalling.
            }
            Err(join_err) => {
                tracing::error!(
                    "StdioEmbedderClient: reader task panicked: {join_err} — \
                     draining pending requests so callers fail fast instead \
                     of hanging"
                );
                drain_pending_with_error(
                    &monitor_pending,
                    EmbedderError::Stdio(format!("embedder reader task panicked: {join_err}")),
                )
                .await;
            }
        }
        // The reader is gone either way: mark the client unhealthy so the
        // supervisor forces a restart even if the child process itself never
        // exits (e.g. a panic that leaves the OS process running).
        let _ = monitor_unhealthy_tx.send(true);
    });

    (unhealthy_tx, unhealthy_rx)
}

/// Why: issue #857 — former static "CUDA OOM/BFCArena stall?" text was emitted
/// on every platform, sending macOS (CoreML/ANE) operators down the wrong path.
/// What: maps each [`ExecutionProvider`] to a terse, provider-specific hint.
/// Test: `timeout_stall_hint_is_provider_aware` in `stdio_tests.rs`.
fn timeout_stall_hint(provider: ExecutionProvider) -> &'static str {
    match provider {
        ExecutionProvider::Cuda => "CUDA OOM/BFCArena stall?",
        ExecutionProvider::CoreML | ExecutionProvider::CoreMLAne => {
            "CoreML/ANE session-init or oversized-batch stall?"
        }
        ExecutionProvider::Cpu => "embedder sidecar stall?",
    }
}

/// Background reader task — owns stdout, dispatches responses by JSON-RPC id.
///
/// Why: separating the read loop from the write path enables multi-flight; id-
/// based dispatch prevents stale-frame misattribution after a timeout (fix #763).
/// Tracks consecutive real (non-idle) timeouts via `timeout_tracker` and, past
/// `WEDGED_TIMEOUT_THRESHOLD`, flips `unhealthy_tx` to declare the sidecar
/// wedged (#1450) — the process may still be alive, so this is the only signal
/// the supervisor gets in that case.
/// What: reads newline-framed JSON-RPC responses, looks up each by echoed id,
/// and dispatches to the caller's oneshot. On timeout, removes only the oldest
/// stalled entry and CONTINUEs — MUST NOT exit (fix #763). On EOF, exits.
/// Every successfully read response frame (matched or stale) resets the
/// timeout tracker, since receiving any frame proves the sidecar is still
/// talking.
/// Test: `reader_task_survives_timeout_and_serves_next_request`,
/// `wedge_threshold_fires_after_consecutive_timeouts`,
/// `wedge_threshold_resets_on_success` in stdio_tests.
async fn reader_task<R: AsyncBufRead + Unpin>(
    mut reader: R,
    pending: PendingMap,
    timeout: Duration,
    timeout_tracker: Arc<TimeoutTracker>,
    unhealthy_tx: watch::Sender<bool>,
) {
    let mut line = String::new();

    loop {
        line.clear();

        // Snapshot the oldest pending id BEFORE arming the deadline so we know
        // which entry to remove if the timeout fires.
        let oldest_id: Option<u64> = {
            let guard = pending.lock().await;
            if guard.is_empty() {
                None
            } else {
                guard.keys().copied().min()
            }
        };

        // Wait for the next response frame under a per-call deadline.
        let read_result = tokio::time::timeout(timeout, reader.read_line(&mut line)).await;

        match read_result {
            Err(_elapsed) => {
                // Fix #763: DO NOT return (kills the task); remove only the oldest
                // entry (not all) so other in-flight requests stay valid. The stale
                // frame is discarded by the id-lookup when it eventually arrives.
                // Issue #857: provider-aware hint so macOS operators are not misled.
                //
                // Why (log-level split, closes #1326): when `oldest_id` is None the
                // pending map was empty — no in-flight request to blame, just a
                // periodic re-arm while the embedder is idle and healthy. Emitting
                // WARN there produces ~2,800 benign lines/day. When `oldest_id` is
                // Some(id) a real in-flight request timed out — that is a genuine
                // stall signal worth WARN.
                // What: branches on the Option to select DEBUG (benign) vs WARN (real).
                // Test: existing `reader_task_survives_timeout_and_serves_next_request`
                // unit test covers the Some-id path; the None path is exercised by
                // running with RUST_LOG=debug and verifying no WARN during idle periods.
                let stall_hint = timeout_stall_hint(resolve_expected_provider());
                if let Some(id) = oldest_id {
                    tracing::warn!(
                        timeout_secs = timeout.as_secs(),
                        timed_out_id = id,
                        "StdioEmbedderClient reader: timed out waiting for response \
                         ({}s — {}) — removing stalled entry, \
                         re-arming; task STAYS ALIVE",
                        timeout.as_secs(),
                        stall_hint,
                    );
                } else {
                    tracing::debug!(
                        timeout_secs = timeout.as_secs(),
                        timed_out_id = ?oldest_id,
                        "StdioEmbedderClient reader: timeout fired with no in-flight \
                         request (idle re-arm, {}s — embedder healthy) — re-arming; \
                         task STAYS ALIVE",
                        timeout.as_secs(),
                    );
                }
                if let Some(id) = oldest_id {
                    let req = {
                        let mut guard = pending.lock().await;
                        guard.remove(&id)
                    };
                    if let Some(r) = req {
                        let _ = r.reply.send(Err(EmbedderError::Stdio(format!(
                            "embed call timed out after {}s (id={id}) — sidecar \
                             stalled (set TRUSTY_EMBEDDERD_CALL_TIMEOUT_SECS to adjust)",
                            timeout.as_secs()
                        ))));
                    }

                    // #1450: only a REAL in-flight timeout counts toward the
                    // wedge threshold — an idle re-arm (oldest_id is None,
                    // handled above) means there was nothing to time out and
                    // is not evidence of a stuck sidecar.
                    if timeout_tracker.record_timeout() {
                        tracing::error!(
                            consecutive_timeouts = WEDGED_TIMEOUT_THRESHOLD,
                            "StdioEmbedderClient reader: {} consecutive call \
                             timeouts with no successful reply — sidecar \
                             appears wedged (process alive but not \
                             responding); signalling supervisor to force a \
                             restart",
                            WEDGED_TIMEOUT_THRESHOLD,
                        );
                        let _ = unhealthy_tx.send(true);
                    }
                }
                line.clear();
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
                // Got a line — dispatch to the matching pending entry by id.
                // #1450: any received frame (even a stale/orphaned one)
                // proves the sidecar is still talking, so reset the
                // consecutive-timeout wedge counter.
                timeout_tracker.record_success();
            }
        }

        // Parse the response id from the frame so we can look up the pending entry.
        // We parse the full response below; extract id first for the lookup.
        let resp_id: Option<u64> = extract_response_id(line.trim());

        let Some(response_id) = resp_id else {
            tracing::warn!(
                raw = %line.trim(),
                "StdioEmbedderClient reader: received response with no parseable id — \
                 discarding (malformed sidecar frame)"
            );
            continue;
        };

        // Look up and remove the pending entry for this id.
        let req = {
            let mut guard = pending.lock().await;
            guard.remove(&response_id)
        };

        let Some(pending_req) = req else {
            // No entry for this id: this is a stale frame from a previously
            // timed-out request whose entry was already removed. Discard it —
            // this is the misattribution-prevention path.
            tracing::warn!(
                response_id,
                "StdioEmbedderClient reader: received response for id={} but \
                 no pending entry found — discarding stale/orphaned frame \
                 (likely a late reply for a previously timed-out request)",
                response_id
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

/// Extract the numeric JSON-RPC `id` from a raw response frame without
/// fully parsing the embeddings (which can be large).
///
/// Why: we need the id to look up the pending entry BEFORE committing to a
/// full decode, and we want a fast path for the common case.
/// What: fully deserialises into `RpcResponse` (serde is fast for this shape)
/// and extracts the `id` field as a `u64`. Returns `None` if the frame is
/// unparseable or the id is not a u64 (e.g. null or string).
/// Test: exercised indirectly by all reader_task tests; direct coverage via
/// the wire-format unit tests.
fn extract_response_id(line: &str) -> Option<u64> {
    #[derive(serde::Deserialize)]
    struct IdOnly {
        #[serde(default)]
        id: Option<serde_json::Value>,
    }
    let parsed: IdOnly = serde_json::from_str(line).ok()?;
    match parsed.id? {
        serde_json::Value::Number(n) => n.as_u64(),
        _ => None,
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

/// Race a request's reply against the client's unhealthy signal (#1448
/// CRITICAL fix).
///
/// Why: the one-time drain in `spawn_reader_with_monitor`'s monitor task only
/// clears the requests that existed *at the instant* the reader died. A NEW
/// `embed_batch` call registered during the kill/respawn window (process
/// still alive, no reader task left to answer, drain already ran) would
/// otherwise `write_all` successfully and then hang on `reply_rx.await`
/// forever — nothing will ever resolve that oneshot. Racing the reply against
/// `unhealthy_rx.changed()` closes that window: once the client is (or
/// becomes) unhealthy, the caller gets a prompt error instead of hanging.
/// What: `biased` select prefers a real reply if one is already available
/// (avoids discarding a legitimate last-moment success in favour of an error
/// that raced it). On the unhealthy branch, removes this request's own entry
/// from `pending` (it will never be answered) before returning an error.
/// Extracted as a standalone function — generic over its inputs rather than
/// `&self` — so it is unit-testable without a real child process.
/// Test: `request_after_reader_death_errors_promptly`,
/// `unhealthy_signal_during_wait_errors_promptly`,
/// `await_reply_or_unhealthy_returns_ok_on_real_reply` in `stdio_tests.rs`.
async fn await_reply_or_unhealthy(
    id: u64,
    pending: &PendingMap,
    reply_rx: oneshot::Receiver<Result<Vec<Vec<f32>>, EmbedderError>>,
    mut unhealthy_rx: watch::Receiver<bool>,
) -> Result<Vec<Vec<f32>>, EmbedderError> {
    tokio::select! {
        biased;

        recv = reply_rx => {
            recv.map_err(|_| EmbedderError::Stdio(
                "reader task dropped reply channel (sidecar crashed or was restarted)".to_owned(),
            ))?
        }

        changed = unhealthy_rx.changed() => {
            // `Ok(())` means the value just flipped to true; `Err` means the
            // sender was dropped (the client itself is gone) — either way,
            // nobody is left to answer this request.
            let _ = changed;
            {
                let mut guard = pending.lock().await;
                guard.remove(&id);
            }
            Err(EmbedderError::Stdio(format!(
                "embedder client became unhealthy while awaiting a reply (id={id}); \
                 reader task died or the sidecar was judged wedged — a supervisor \
                 restart is likely already in progress"
            )))
        }
    }
}

/// Drain all pending requests with an error (EOF / crash path).
///
/// Why: prevents callers from hanging when the reader exits. Supervisor then
/// swaps in a fresh `StdioEmbedderClient`. Test: multi-flight crash simulation.
async fn drain_pending_with_error(pending: &PendingMap, error: EmbedderError) {
    let mut guard = pending.lock().await;
    for (_id, req) in guard.drain() {
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
    /// in the id-keyed pending map, writes request (stdin lock held only for
    /// write + flush), then awaits the oneshot. Reader task dispatches replies
    /// by echoed JSON-RPC id, so stale/orphaned frames from timed-out requests
    /// can never be misattributed to new requests.
    ///
    /// #1448 CRITICAL fix: a request can be issued during the window after the
    /// reader task has died (panic or wedge-declared) but before the
    /// supervisor has killed + respawned the process. The one-time drain that
    /// runs when the reader dies cannot see a request that didn't exist yet,
    /// so this method (a) fails fast if the client is already known unhealthy,
    /// before ever registering a pending entry, and (b) races the reply
    /// against the unhealthy signal via `await_reply_or_unhealthy` so a
    /// request that becomes stranded mid-wait still errors promptly instead
    /// of hanging forever.
    /// Test: `cargo test -p trusty-embedderd --test multiflight`;
    /// `request_after_reader_death_errors_promptly`,
    /// `unhealthy_signal_during_wait_errors_promptly` in `stdio_tests.rs`.
    async fn embed_batch(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>, EmbedderError> {
        if texts.is_empty() {
            return Ok(vec![]);
        }
        let sent = texts.len();

        // Fail fast if the client is already known unhealthy (#1448): avoids
        // registering a pending entry and writing to stdin when no reader
        // task exists to ever answer it.
        let unhealthy_rx = self.unhealthy_tx.subscribe();
        if *unhealthy_rx.borrow() {
            return Err(EmbedderError::Stdio(
                "embedder client is unhealthy (reader task died or the sidecar was \
                 judged wedged); refusing new request — a supervisor restart is \
                 likely already in progress"
                    .to_owned(),
            ));
        }

        // Bound concurrent in-flight requests.
        let _permit = self
            .inflight
            .acquire()
            .await
            .map_err(|_| EmbedderError::Stdio("inflight semaphore closed".to_owned()))?;

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        tracing::debug!(n = sent, id, "StdioEmbedderClient: sending batch");

        // Register the pending oneshot BEFORE writing the request so the
        // reader task can never dispatch-before-register.
        let (reply_tx, reply_rx) = oneshot::channel();
        {
            let mut guard = self.pending.lock().await;
            guard.insert(
                id,
                PendingRequest {
                    sent,
                    reply: reply_tx,
                },
            );
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

        // Await the reader task's dispatch, racing against the unhealthy
        // signal (#1448) so a reader death or wedge declaration that happens
        // AFTER this request was registered still resolves promptly.
        let result = await_reply_or_unhealthy(id, &self.pending, reply_rx, unhealthy_rx).await;

        tracing::debug!(n = sent, id, "StdioEmbedderClient: batch complete");
        result
    }
}

// Tests are in a sibling file to keep this file under the 500-line cap.
// The submodule can access private items via `super::` (Rust child-module rule).
#[cfg(test)]
#[path = "stdio_tests.rs"]
mod tests;
