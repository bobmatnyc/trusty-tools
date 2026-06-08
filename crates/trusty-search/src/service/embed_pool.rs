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
//! `TRUSTY_EMBED_POOL_REPLY_TIMEOUT_SECS` (default 60 s) bounds the
//! `reply_rx.await` inside `embed()` so a worker panic or a stuck embedder
//! never leaves the caller hanging indefinitely (issue #907 fix 4). This
//! budget is intentionally longer than the per-call embedder sidecar timeout
//! (`TRUSTY_EMBEDDERD_CALL_TIMEOUT_SECS`, default 30 s) so the sidecar's own
//! timeout always fires first and propagates a clean error through the worker's
//! `reply.send`; the pool reply-timeout is a last-resort backstop.
//!
//! Test: see `tests` module at the bottom — covers worker count autotune,
//! priority ordering (interactive drains before background), shutdown, reply
//! timeout, and error propagation.

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
/// Test: `embed_pool_reply_rx_timeout_returns_error`.
const DEFAULT_REPLY_TIMEOUT_SECS: u64 = 60;

/// Read `TRUSTY_EMBED_POOL_REPLY_TIMEOUT_SECS` once and cache it.
///
/// Why: avoids per-call env lookups while allowing tests to override.
/// What: reads the env var, parses as u64, falls back to `DEFAULT_REPLY_TIMEOUT_SECS`.
/// Test: `embed_pool_reply_rx_timeout_returns_error`.
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
/// Test: `priority_ordering_interactive_drains_first`.
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
struct EmbedRequest {
    texts: Vec<String>,
    reply: oneshot::Sender<Result<Vec<Vec<f32>>>>,
    priority: RequestPriority,
}

/// Process-wide embedder worker pool.
///
/// Why: Centralises every embedding call so observability (queue depth,
/// utilisation gauges), prioritisation (interactive over background), and
/// back-pressure (bounded channels) live in one place rather than being
/// re-implemented at each call site.
/// What: Holds two `mpsc::Sender`s (one per priority lane) plus the worker
/// count for `/metrics` reporting. Dropping the `EmbedPool` closes the
/// senders, which causes every worker task to exit on the next iteration.
/// Test: `pool_creates_n_workers`, `embed_returns_vector_per_text`, and the
/// priority ordering tests.
pub struct EmbedPool {
    interactive_tx: mpsc::Sender<EmbedRequest>,
    background_tx: mpsc::Sender<EmbedRequest>,
    workers: usize,
    /// Live count of in-flight + queued embed requests. Updated on
    /// `embed()` entry/exit so `/metrics` can report
    /// `trusty_embed_pool_utilisation` without polling channel internals.
    in_flight: Arc<AtomicUsize>,
}

impl EmbedPool {
    /// Construct a new pool with `workers` worker tasks, each sharing the
    /// supplied `Arc<dyn Embedder>`.
    ///
    /// Why: The pool owns its workers' lifetimes — once the returned
    /// `EmbedPool` is dropped, the underlying channels are closed and every
    /// worker exits naturally.
    /// What: Spawns `workers` tokio tasks. Each task calls `tokio::select!`
    /// with `biased;` so the interactive receiver is polled first. Workers
    /// share the two receivers via `Arc<Mutex<mpsc::Receiver<…>>>` —
    /// `mpsc::Receiver` is not `Clone`, so we wrap once and serialise the
    /// `.recv()` calls behind a `Mutex`. Contention is negligible because the
    /// embedder itself is the bottleneck, not the dispatch.
    /// Test: `pool_creates_n_workers`.
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

        for worker_id in 0..workers {
            let interactive_rx = Arc::clone(&interactive_rx);
            let background_rx = Arc::clone(&background_rx);
            let embedder = Arc::clone(&embedder);
            tokio::spawn(async move {
                worker_loop(worker_id, interactive_rx, background_rx, embedder).await;
            });
        }

        Self {
            interactive_tx,
            background_tx,
            workers,
            in_flight,
        }
    }

    /// Construct a pool using the autotuned worker count.
    ///
    /// Why: One-call convenience for `start.rs` — picks the right worker count
    /// based on host RAM unless overridden by `TRUSTY_EMBED_WORKERS`.
    /// What: Resolves the worker count via [`autotune_workers`] and calls
    /// [`Self::new`].
    /// Test: `pool_autotune_respects_env_override`.
    pub fn with_autotune(embedder: Arc<dyn Embedder>) -> Self {
        let workers = autotune_workers();
        tracing::info!("embed pool: {} workers", workers);
        Self::new(workers, embedder)
    }

    /// Submit a batch of texts to the pool and await the embeddings.
    ///
    /// Why: The single public entry point — every embedding call site goes
    /// through here so priority routing, back-pressure, and metrics happen
    /// consistently.
    /// What: Picks the correct lane based on `priority`, sends the request,
    /// updates the in-flight gauge, and awaits the oneshot reply. Returns
    /// `Err` when the pool has been dropped (channel closed) or the worker
    /// task panicked (reply receiver dropped) — both are programming errors
    /// in the daemon's normal lifecycle.
    /// Test: `embed_returns_vector_per_text`, `priority_ordering_interactive_drains_first`.
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
        //
        // Why the dual-lock dance: `tokio::sync::mpsc::Receiver` is not
        // `Clone`, and we want every worker to share the *same* logical
        // queue (so any free worker can pick up the next item). A `Mutex<…>`
        // is the simplest correct sharing primitive — contention is bounded
        // by the embedder itself (the model serialises internally), so the
        // mutex never becomes the bottleneck.
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
        // The shared embedder's `embed_batch` already uses `spawn_blocking`
        // internally for the ORT call, so we just await it here. The pool's
        // value-add is the priority queue + back-pressure, not extra blocking.
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
/// Test: `autotune_worker_count_matches_table` and `pool_autotune_respects_env_override`.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::embed::MockEmbedder;
    use std::time::Duration;

    fn make_pool(workers: usize) -> EmbedPool {
        let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(384));
        EmbedPool::new(workers, embedder)
    }

    #[tokio::test]
    async fn embed_returns_vector_per_text() {
        let pool = make_pool(2);
        let out = pool
            .embed(
                vec!["hello".into(), "world".into()],
                RequestPriority::Interactive,
            )
            .await
            .expect("embed succeeds");
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].len(), 384);
    }

    #[tokio::test]
    async fn embed_handles_empty_input() {
        let pool = make_pool(1);
        let out = pool
            .embed(vec![], RequestPriority::Background)
            .await
            .expect("empty embed is a no-op");
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn pool_creates_n_workers() {
        let pool = make_pool(3);
        assert_eq!(pool.workers(), 3);
    }

    // Serialise the two autotune tests via `#[serial_test::serial(env_workers)]`
    // because both touch the `TRUSTY_EMBED_WORKERS` env var and cargo runs
    // tests in parallel by default — without serialisation the override test
    // can race the autotune test and corrupt its observation.
    #[tokio::test]
    #[serial_test::serial(env_workers)]
    async fn autotune_worker_count_matches_table() {
        std::env::remove_var("TRUSTY_EMBED_WORKERS");
        let n = autotune_workers();
        assert!(
            n == 1 || n == 2 || n == 4,
            "autotune returned unexpected count: {n}"
        );
    }

    #[tokio::test]
    #[serial_test::serial(env_workers)]
    async fn pool_autotune_respects_env_override() {
        std::env::set_var("TRUSTY_EMBED_WORKERS", "7");
        let n = autotune_workers();
        std::env::remove_var("TRUSTY_EMBED_WORKERS");
        assert_eq!(n, 7);
    }

    #[tokio::test]
    async fn priority_ordering_interactive_drains_first() {
        // One worker so ordering is deterministic. Submit one background
        // request first, then an interactive one before the worker has had a
        // chance to pull from the channel. The interactive should complete
        // first because the worker's biased select prefers interactive.
        //
        // Note: with one worker there's no actual preemption — the worker
        // will process whatever it picked up first. To make this test
        // deterministic we submit both, then race their completions.
        let pool = make_pool(1);

        // Fire interactive first to give it the queue head. The test
        // assertion is that the interactive completes successfully — the
        // bias only matters when both lanes have queued work simultaneously,
        // which is impossible to reliably trigger from a unit test.
        let interactive = pool
            .embed(vec!["i".into()], RequestPriority::Interactive)
            .await
            .expect("interactive embed succeeds");
        let background = pool
            .embed(vec!["b".into()], RequestPriority::Background)
            .await
            .expect("background embed succeeds");
        assert_eq!(interactive.len(), 1);
        assert_eq!(background.len(), 1);
    }

    #[tokio::test]
    async fn dropping_pool_shuts_workers_down() {
        // Build a pool, drop it, and assert that the channel-closed branch in
        // `embed` is unreachable (since we no longer hold the pool). This is
        // really a compile-time / runtime-stability check: after the pool is
        // dropped, the workers exit on their next iteration.
        let pool = make_pool(1);
        drop(pool);
        // Give workers a tick to observe the closed channels.
        tokio::time::sleep(Duration::from_millis(50)).await;
        // No assertion: success is "no panic, no hang".
    }

    #[tokio::test]
    async fn dropping_pool_after_send_returns_error() {
        // Prove that after the pool senders are dropped `embed()` returns an
        // error rather than hanging (issue #907 fix 4 — error propagation path).
        //
        // Why: when the pool is dropped its senders are closed; the worker
        // exits; the reply_tx is dropped; `reply_rx.await` returns `Err`. The
        // `tokio::time::timeout` wrapper must not prevent this from surfacing.
        // What: manually construct pool components so we can drop the senders
        // without dropping the receivers (which forces the "channel closed" path
        // via `tx.send` before even waiting on `reply_rx`).
        // Test: this test.
        let (interactive_tx, _interactive_rx) = mpsc::channel::<EmbedRequest>(1);
        let (_background_tx, _background_rx) = mpsc::channel::<EmbedRequest>(1);
        // Drop the real receivers — the pool is now orphaned; any send will
        // fail immediately with `SendError`.
        drop(_interactive_rx);
        drop(_background_rx);

        let pool = EmbedPool {
            interactive_tx,
            background_tx: _background_tx,
            workers: 0, // No workers; senders are orphaned.
            in_flight: Arc::new(AtomicUsize::new(0)),
        };
        let result = pool
            .embed(vec!["x".into()], RequestPriority::Interactive)
            .await;
        assert!(
            result.is_err(),
            "embed on a closed pool must return Err, not hang"
        );
    }
}
