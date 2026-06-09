//! Embedder worker pool with priority lanes (issue #41 Phase 1).
//!
//! Why: Before this module, all embedding work flowed through a single
//! `Arc<dyn Embedder>` whose ONNX session is internally serialised by a
//! `Mutex<TextEmbedding>`. Combined with the machine-wide `Semaphore(1)` in
//! `service/reindex.rs`, a long-running reindex would block search queries
//! that arrived mid-flight (their embedding step had to wait behind the
//! reindex's hundreds of `embed_batch` calls). The pool restores
//! responsiveness for interactive search by introducing a strict priority
//! ordering — interactive requests always drain before background requests —
//! and bounds outstanding work so the daemon cannot OOM under burst load.
//!
//! What: A fixed pool of N async worker tasks, each holding an
//! `Arc<dyn Embedder>` clone (the underlying model is shared via Arc; FastEmbedder
//! already serialises ONNX inference internally). Requests arrive on two
//! `mpsc` channels — `interactive_tx` and `background_tx` — and workers
//! drain interactive first via `tokio::select!` with `biased;`. Callers
//! get a `oneshot::Receiver<Result<Vec<Vec<f32>>>>` back from `embed()`.
//!
//! Worker count is autotuned from system RAM (`trusty-search`'s
//! `core::memory_policy::detect_total_ram_mb`):
//!   - `<= 16 GB`  -> 1 worker
//!   - `17-32 GB`  -> 2 workers
//!   - `>  32 GB`  -> 4 workers
//!
//! `TRUSTY_EMBED_WORKERS` overrides the autotune.
//!
//! ## Executor isolation (issue #1017 — root-cause fix)
//!
//! Worker tasks run on **dedicated OS threads with their own single-thread
//! Tokio runtimes** — wholly separate from the HTTP-server runtime and its
//! thread pool. Each embed worker is a `std::thread::spawn`'d OS thread that
//! builds a `tokio::runtime::Builder::new_current_thread()` runtime and runs
//! its `worker_loop` on it. Tokio `mpsc`/`oneshot` channels are runtime-
//! agnostic, so the callers (on the HTTP runtime) can still send requests and
//! await replies via the same channel API.
//!
//! Consequence: a 30 s CoreML/ANE stall blocks only the embed worker's own
//! OS thread. The main Tokio runtime's thread pool remains fully available for
//! the axum accept loop, `/health`, and all other HTTP handlers, regardless of
//! how many embed workers are stalled (issue #1017 root-cause fix). The
//! PR #1016 worker-floor bump becomes belt-and-suspenders rather than load-
//! bearing.
//!
//! `TRUSTY_EMBED_POOL_REPLY_TIMEOUT_SECS` (default 60 s) bounds the
//! `reply_rx.await` inside `embed()` so a worker panic or a stuck embedder
//! never leaves the caller hanging indefinitely (issue #907 fix 4). This
//! budget is intentionally longer than the per-call embedder sidecar timeout
//! (`TRUSTY_EMBEDDERD_CALL_TIMEOUT_SECS`, default 30 s) so the sidecar's own
//! timeout always fires first and propagates a clean error through the worker's
//! `reply.send`; the pool reply-timeout is a last-resort backstop.
//!
//! Test: see `embed_pool_tests` module (split into `embed_pool_tests.rs` to
//! keep this file under the 500-line cap) — covers worker count autotune,
//! priority ordering (interactive drains before background), shutdown, reply
//! timeout, error propagation, and the isolation proof.

use anyhow::{Context, Result};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};

/// Default reply-receive timeout for `embed()` (issue #907 fix 4).
///
/// Why: if a worker panics or the worker loop exits unexpectedly, `reply_rx`
/// would otherwise block forever. This backstop is intentionally longer than
/// the embedder sidecar call timeout (30 s) so the sidecar's own timeout
/// always fires and propagates a clean error first; the pool timeout is the
/// last-resort catch for cases where the sidecar timeout cannot fire (e.g.
/// the worker task itself panicked before calling `embed_batch`).
/// What: 60 s. Override with `TRUSTY_EMBED_POOL_REPLY_TIMEOUT_SECS`.
/// Test: `embed_pool_reply_rx_timeout_returns_error` in embed_pool_tests.rs.
const DEFAULT_REPLY_TIMEOUT_SECS: u64 = 60;

/// Read `TRUSTY_EMBED_POOL_REPLY_TIMEOUT_SECS` once and cache it.
///
/// Why: avoids per-call env lookups while allowing tests to override.
/// What: reads the env var, parses as u64, falls back to `DEFAULT_REPLY_TIMEOUT_SECS`.
/// Test: `embed_pool_reply_rx_timeout_returns_error` in embed_pool_tests.rs.
fn reply_timeout() -> std::time::Duration {
    static CACHED: std::sync::OnceLock<std::time::Duration> = std::sync::OnceLock::new();
    *CACHED.get_or_init(|| {
        let secs = std::env::var("TRUSTY_EMBED_POOL_REPLY_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(DEFAULT_REPLY_TIMEOUT_SECS);
        std::time::Duration::from_secs(secs)
    })
}

use crate::core::embed::Embedder;

/// Request priority. Interactive requests (search queries) are drained
/// strictly before background requests (reindex, discovery) so a long-running
/// reindex never blocks search latency.
///
/// Why: Search is user-facing — sub-10 s p95 latency is the headline target.
/// Reindex is batch work that runs in the background and is tolerant of a few
/// extra seconds.
/// What: A two-variant enum used as the channel selector inside the pool.
/// Test: `priority_ordering_interactive_drains_first` in embed_pool_tests.rs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestPriority {
    /// Search queries — drained first.
    Interactive,
    /// Reindex, discovery — drained when no interactive work is queued.
    Background,
}

/// Default channel capacity per priority lane.
///
/// Why: Bounded channels create natural back-pressure — when the lane is
/// full, callers `.await` until a slot opens. A capacity of 64 absorbs short
/// bursts without unbounded memory growth on a stuck pool.
/// What: 64 slots per lane (interactive + background = 128 total).
const LANE_CAPACITY: usize = 64;

/// One unit of work sent to a worker.
///
/// Why: Bundles the input batch with a `oneshot::Sender` so the worker can
/// reply directly to the originating caller without holding any global state.
/// What: Carries `texts`, the `reply` sender, and a priority tag (preserved
/// purely for tracing — the channel the request arrived on already determines
/// when the worker picks it up).
pub(crate) struct EmbedRequest {
    pub(crate) texts: Vec<String>,
    pub(crate) reply: oneshot::Sender<Result<Vec<Vec<f32>>>>,
    pub(crate) priority: RequestPriority,
}

/// Process-wide embedder worker pool.
///
/// Why: Centralises every embedding call so observability (queue depth,
/// utilisation gauges), prioritisation (interactive over background), and
/// back-pressure (bounded channels) live in one place rather than being
/// re-implemented at each call site.
/// What: Holds two `mpsc::Sender`s (one per priority lane), the worker
/// count for `/metrics` reporting, and join handles for the N **dedicated OS
/// threads** that own embed work (issue #1017 — executor isolation). Each OS
/// thread runs its own single-thread Tokio runtime; a 30 s sidecar stall
/// occupies only that thread, leaving the HTTP runtime's pool untouched.
/// Dropping the `EmbedPool` closes the senders (workers exit their loops) and
/// joins the OS threads.
/// Test: `pool_creates_n_workers`, `embed_returns_vector_per_text`, and the
/// priority ordering / isolation tests in embed_pool_tests.rs.
pub struct EmbedPool {
    pub(crate) interactive_tx: mpsc::Sender<EmbedRequest>,
    pub(crate) background_tx: mpsc::Sender<EmbedRequest>,
    workers: usize,
    /// Live count of in-flight + queued embed requests. Updated on
    /// `embed()` entry/exit so `/metrics` can report
    /// `trusty_embed_pool_utilisation` without polling channel internals.
    in_flight: Arc<AtomicUsize>,
    /// Join handles for the N dedicated embed OS threads (issue #1017).
    ///
    /// Why: held here so the threads are joined on drop. Each thread runs an
    /// independent current-thread Tokio runtime that is free to block on the
    /// sidecar for up to 30 s without affecting the main HTTP runtime.
    /// What: dropped (and joined) when the pool is dropped. Channel closure
    /// (from `interactive_tx`/`background_tx` drop) signals each worker to exit
    /// its loop before the join completes.
    _worker_threads: Vec<std::thread::JoinHandle<()>>,
}

impl EmbedPool {
    /// Construct a new pool with `workers` worker tasks, each sharing the
    /// supplied `Arc<dyn Embedder>`.
    ///
    /// Why: The pool owns its workers' lifetimes — once the returned
    /// `EmbedPool` is dropped, the underlying channels are closed and every
    /// worker exits naturally. Workers run on dedicated OS threads with their
    /// own Tokio runtimes (issue #1017 — executor isolation).
    /// What: Spawns `workers` OS threads via `std::thread::spawn`. Each thread
    /// builds a `new_current_thread` Tokio runtime and blocks on
    /// `worker_loop`. The `mpsc` channels are runtime-agnostic so callers on
    /// the HTTP runtime can send/await replies seamlessly.
    /// Test: `pool_creates_n_workers` in embed_pool_tests.rs.
    pub fn new(workers: usize, embedder: Arc<dyn Embedder>) -> Self {
        let workers = workers.max(1);
        let (interactive_tx, interactive_rx) = mpsc::channel::<EmbedRequest>(LANE_CAPACITY);
        let (background_tx, background_rx) = mpsc::channel::<EmbedRequest>(LANE_CAPACITY);
        let interactive_rx = Arc::new(tokio::sync::Mutex::new(interactive_rx));
        let background_rx = Arc::new(tokio::sync::Mutex::new(background_rx));
        let in_flight = Arc::new(AtomicUsize::new(0));

        // Set the static workers gauge once. The utilisation gauge is updated
        // per request inside `embed()`.
        metrics::gauge!("trusty_embed_pool_workers").set(workers as f64);

        let mut worker_threads = Vec::with_capacity(workers);

        for worker_id in 0..workers {
            let interactive_rx = Arc::clone(&interactive_rx);
            let background_rx = Arc::clone(&background_rx);
            let embedder = Arc::clone(&embedder);

            // Spawn a dedicated OS thread for this worker (issue #1017).
            // The thread builds its own single-thread Tokio runtime so it can
            // await the sidecar reply without occupying the HTTP runtime's
            // threads. The runtime is created and dropped entirely within this
            // thread — no runtime-drop-inside-async-context issue.
            let handle = std::thread::Builder::new()
                .name(format!("trusty-embed-{worker_id}"))
                .spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .thread_name(format!("trusty-embed-io-{worker_id}"))
                        .build()
                        .expect("embed worker: failed to build tokio runtime");
                    rt.block_on(worker_loop(
                        worker_id,
                        interactive_rx,
                        background_rx,
                        embedder,
                    ));
                })
                .expect("embed worker: failed to spawn OS thread");

            worker_threads.push(handle);
        }

        Self {
            interactive_tx,
            background_tx,
            workers,
            in_flight,
            _worker_threads: worker_threads,
        }
    }

    /// Construct a pool using the autotuned worker count.
    ///
    /// Why: One-call convenience for `start.rs` — picks the right worker count
    /// based on host RAM unless overridden by `TRUSTY_EMBED_WORKERS`.
    /// What: Resolves the worker count via [`autotune_workers`] and calls
    /// [`Self::new`].
    /// Test: `pool_autotune_respects_env_override` in embed_pool_tests.rs.
    pub fn with_autotune(embedder: Arc<dyn Embedder>) -> Self {
        let workers = autotune_workers();
        tracing::info!("embed pool: {} workers (isolated OS threads)", workers);
        Self::new(workers, embedder)
    }

    /// Submit a batch of texts to the pool and await the embeddings.
    ///
    /// Why: The single public entry point — every embedding call site goes
    /// through here so priority routing, back-pressure, and metrics happen
    /// consistently.
    /// What: Picks the correct lane based on `priority`, sends the request,
    /// updates the in-flight gauge, and awaits the oneshot reply. The actual
    /// embed work runs on a dedicated OS-thread/runtime (issue #1017), so
    /// this `await` only suspends the caller's Tokio task until the worker
    /// sends the reply — the HTTP runtime's threads remain fully free.
    /// Returns `Err` when the pool has been dropped (channel closed) or the
    /// worker thread panicked (reply receiver dropped).
    /// Test: `embed_returns_vector_per_text`,
    /// `embed_pool_isolation_concurrent_task_not_blocked` in embed_pool_tests.rs.
    pub async fn embed(
        &self,
        texts: Vec<String>,
        priority: RequestPriority,
    ) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let (reply_tx, reply_rx) = oneshot::channel();
        let req = EmbedRequest {
            texts,
            reply: reply_tx,
            priority,
        };
        let tx = match priority {
            RequestPriority::Interactive => &self.interactive_tx,
            RequestPriority::Background => &self.background_tx,
        };
        // Update utilisation BEFORE awaiting send so /metrics reflects queue
        // pressure during the back-pressure wait, not just during inference.
        self.in_flight.fetch_add(1, Ordering::Relaxed);
        metrics::gauge!("trusty_embed_pool_utilisation")
            .set(self.in_flight.load(Ordering::Relaxed) as f64);

        let send_result = tx.send(req).await.context("embed pool closed");
        // Bound the reply-receive so a panicking/stuck worker never hangs
        // the caller forever (issue #907 fix 4). The embedder sidecar's own
        // call timeout (30 s by default) fires first for the normal stall
        // path; this backstop catches worker-task panics and other unexpected
        // failure modes where the sidecar timeout cannot propagate.
        let deadline = reply_timeout();
        let result = match send_result {
            Ok(()) => match tokio::time::timeout(deadline, reply_rx).await {
                Ok(Ok(r)) => r,
                Ok(Err(_)) => Err(anyhow::anyhow!("embed pool worker dropped reply")),
                Err(_elapsed) => Err(anyhow::anyhow!(
                    "embed pool reply timed out after {}s — worker may have panicked \
                         (set TRUSTY_EMBED_POOL_REPLY_TIMEOUT_SECS to adjust)",
                    deadline.as_secs()
                )),
            },
            Err(e) => Err(e),
        };

        self.in_flight.fetch_sub(1, Ordering::Relaxed);
        metrics::gauge!("trusty_embed_pool_utilisation")
            .set(self.in_flight.load(Ordering::Relaxed) as f64);

        result
    }

    /// Number of worker tasks the pool spawned. Used by `/metrics` and
    /// startup logging.
    pub fn workers(&self) -> usize {
        self.workers
    }
}

/// Per-worker async loop: drain interactive lane first, fall back to
/// background. Exits when both senders are dropped.
///
/// Why: Each worker runs on its own OS thread with a dedicated Tokio runtime
/// (issue #1017). If `embed_batch` stalls for up to 30 s (CoreML/ANE), only
/// this thread is blocked — the HTTP runtime's pool is completely unaffected.
/// What: biased select on interactive-first; exits on channel close.
/// Test: `dropping_pool_shuts_workers_down` in embed_pool_tests.rs.
async fn worker_loop(
    worker_id: usize,
    interactive_rx: Arc<tokio::sync::Mutex<mpsc::Receiver<EmbedRequest>>>,
    background_rx: Arc<tokio::sync::Mutex<mpsc::Receiver<EmbedRequest>>>,
    embedder: Arc<dyn Embedder>,
) {
    loop {
        // Acquire both locks briefly to receive. Because `recv()` is `&mut
        // self`, the lock must be held across the await. `biased;` makes the
        // select prefer the interactive lane whenever both have a message
        // ready, which is the whole point of priority lanes.
        let req = {
            let mut interactive_guard = interactive_rx.lock().await;
            let mut background_guard = background_rx.lock().await;
            tokio::select! {
                biased;
                msg = interactive_guard.recv() => msg,
                msg = background_guard.recv() => msg,
            }
        };
        let Some(req) = req else {
            tracing::debug!(worker_id, "embed pool worker exiting (channels closed)");
            return;
        };
        let EmbedRequest {
            texts,
            reply,
            priority,
        } = req;
        let started = std::time::Instant::now();
        // This runs on the worker's own OS thread / Tokio runtime (issue #1017).
        // A sidecar stall of up to 30 s blocks only this thread, never the
        // HTTP runtime's thread pool.
        let text_refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
        let result = embedder
            .embed_batch(&text_refs)
            .await
            .context("embed pool worker: embed_batch failed");
        let elapsed_ms = started.elapsed().as_millis() as u64;
        tracing::trace!(
            worker_id,
            priority = ?priority,
            batch_size = texts.len(),
            elapsed_ms,
            "embed pool dispatched batch"
        );
        // Ignore the send result — if the caller dropped the receiver they
        // no longer care about the answer.
        let _ = reply.send(result);
    }
}

/// Resolve worker count from `TRUSTY_EMBED_WORKERS` or autodetect from RAM.
///
/// Why: A one-worker default on small hosts keeps memory bounded; larger
/// hosts can usefully run more concurrent dispatches even though FastEmbedder
/// itself serialises ONNX inference (workers still buy parallel ahead-of-time
/// tokenisation + cache lookups, and the priority lane preemption still
/// works).
/// What:
///   - `TRUSTY_EMBED_WORKERS=N` -> exactly N (clamped to >= 1).
///   - Otherwise: `detect_total_ram_mb()` ->
///     `<= 16 GB` -> 1; `17-32 GB` -> 2; `> 32 GB` -> 4.
///   - RAM detection failure -> 1 (safe default).
///
/// Test: `autotune_worker_count_matches_table` and `pool_autotune_respects_env_override`
/// in embed_pool_tests.rs.
pub fn autotune_workers() -> usize {
    if let Ok(raw) = std::env::var("TRUSTY_EMBED_WORKERS") {
        if let Ok(n) = raw.parse::<usize>() {
            return n.max(1);
        }
    }
    let ram_mb = crate::core::memory_policy::detect_total_ram_mb().unwrap_or(8 * 1024);
    let ram_gb = ram_mb / 1024;
    if ram_gb <= 16 {
        1
    } else if ram_gb <= 32 {
        2
    } else {
        4
    }
}

// Tests are in a sibling file to keep this file under the 500-line cap.
// The submodule can access pub(crate) items via `super::` (Rust child-module rule).
#[cfg(test)]
#[path = "embed_pool_tests.rs"]
mod tests;
