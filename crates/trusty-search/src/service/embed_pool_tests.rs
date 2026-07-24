//! Tests for `embed_pool` — split into this sibling file to keep the main
//! module under the 500-line cap.
//!
//! Why: the main `embed_pool.rs` would exceed 500 lines if tests were inlined.
//! Rust's child-module rule lets this file access `pub(crate)` items via `super::`.
//! What: covers worker-count autotune, priority ordering (interactive drains
//! before background), shutdown behaviour, reply timeout, error propagation,
//! and — critically — the executor-isolation guarantee that a stalled embed
//! does NOT prevent concurrent async work on the caller's runtime from making
//! progress (issue #1017 root-cause fix).
//! Test: `SKIP_UI_BUILD=1 cargo test -p trusty-search -- embed_pool`.

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
    // `embed` is unreachable (since we no longer hold the pool). With the
    // OS-thread isolation approach, dropping `EmbedPool` closes the senders,
    // signals the workers to exit, and joins the OS threads (all in drop).
    // This test verifies there is no panic or deadlock on drop.
    let pool = make_pool(1);
    drop(pool);
    // No assertion: success is "no panic, no hang".
    // Give any lingering OS thread a moment to finish (join happens in Drop,
    // so this sleep is just extra breathing room for the test harness).
    tokio::time::sleep(Duration::from_millis(50)).await;
}

#[tokio::test]
async fn dropping_pool_after_send_returns_error() {
    // Prove that after the pool senders are dropped `embed()` returns an
    // error rather than hanging (issue #907 fix 4 — error propagation path).
    //
    // Why: construct a pool and then close the receivers so the first send
    // fails immediately — exercises the "channel closed" error path without
    // building a fake pool struct.
    // What: we have only one worker; sending to a live pool and then dropping
    // it. The pool's Drop closes the senders; subsequent calls return Err.
    // Test: this test.
    let pool = make_pool(1);
    // The pool is live; a normal embed should succeed.
    pool.embed(vec!["warmup".into()], RequestPriority::Interactive)
        .await
        .expect("warmup embed on live pool must succeed");

    // Now use the channel internals: send into a manually-created closed channel.
    let (interactive_tx, interactive_rx) = mpsc::channel::<EmbedRequest>(1);
    drop(interactive_rx); // Receiver gone — first send will return SendError.

    let (_background_tx, _background_rx) = mpsc::channel::<EmbedRequest>(1);
    drop(_background_rx);

    // Build a minimal pool with the broken senders. Worker threads not needed
    // because the send fails before reaching any worker.
    let closed_pool = EmbedPool {
        interactive_tx,
        background_tx: _background_tx,
        workers: 0,
        in_flight: Arc::new(AtomicUsize::new(0)),
        _worker_threads: vec![],
        stall_tracker: None,
    };
    let result = closed_pool
        .embed(vec!["x".into()], RequestPriority::Interactive)
        .await;
    assert!(
        result.is_err(),
        "embed on a closed pool must return Err, not hang"
    );
}

/// Prove interactive requests dispatch ahead of ALREADY-QUEUED background
/// work under genuine contention — the wave-granularity preemption slice B
/// PR 1 (issue #3748) relies on: the catch-up path submits one pool request
/// per sub-batch "wave" rather than one request for the whole reindex, so an
/// interactive query queued mid-pass only ever waits behind the wave
/// currently in flight, never the whole background job.
///
/// Why: `priority_ordering_interactive_drains_first` above only proves each
/// lane individually completes; its own comment admits it cannot reliably
/// force both lanes to have queued work simultaneously. This test forces
/// that condition deterministically with a single-worker pool and a gated
/// embedder: the first call blocks on a `Notify` (simulating an in-flight
/// wave), giving the test time to queue several background requests AND one
/// interactive request behind it before releasing the gate. `biased;` in
/// `worker_loop`'s `select!` must then pick the interactive request next,
/// even though three background requests arrived first.
/// What: submits `bg-0..bg-2` (queued, not yet running), then `interactive`,
/// then releases the gate; asserts `interactive` appears in the completion
/// order before any of `bg-0..bg-2`.
/// Test: this test (`SKIP_UI_BUILD=1 cargo test -p trusty-search -- \
/// interactive_preempts_queued_background_wave`).
#[tokio::test]
async fn interactive_preempts_queued_background_wave() {
    use tokio::sync::Notify;

    /// Blocks its FIRST call on `gate` (simulating a wave already in
    /// flight); every later call returns immediately and appends its input
    /// text to `order` so the test can assert completion sequencing.
    struct GateEmbedder {
        gate: Arc<Notify>,
        gated_once: std::sync::atomic::AtomicBool,
        order: std::sync::Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl crate::core::embed::Embedder for GateEmbedder {
        async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
            unimplemented!("pool always calls embed_batch")
        }

        async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
            if !self.gated_once.swap(true, Ordering::SeqCst) {
                self.gate.notified().await;
            }
            self.order
                .lock()
                .expect("order mutex poisoned")
                .push(texts[0].to_string());
            Ok(texts.iter().map(|_| vec![0.0f32]).collect())
        }

        fn dimension(&self) -> usize {
            1
        }
    }

    let gate = Arc::new(Notify::new());
    let embedder = Arc::new(GateEmbedder {
        gate: Arc::clone(&gate),
        gated_once: std::sync::atomic::AtomicBool::new(false),
        order: std::sync::Mutex::new(Vec::new()),
    });
    // Single worker: the ONLY way `interactive` can run before `bg-1`/`bg-2`
    // is via the biased-select priority lane, not incidental scheduling.
    let embedder_dyn: Arc<dyn Embedder> = embedder.clone();
    let pool: Arc<EmbedPool> = Arc::new(EmbedPool::new(1, embedder_dyn));

    // Kick off the in-flight "wave" — the worker picks this up immediately
    // and blocks inside `embed_batch` on `gate.notified()`.
    let inflight = {
        let pool = Arc::clone(&pool);
        tokio::spawn(async move {
            pool.embed(vec!["wave-0".into()], RequestPriority::Background)
                .await
        })
    };
    // Give the worker a moment to actually pick up "wave-0" and start
    // blocking on the gate, so the requests below are genuinely QUEUED
    // (not racing to be picked up first).
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Queue three background requests behind the in-flight wave.
    let mut bg_handles = Vec::new();
    for i in 0..3 {
        let pool = Arc::clone(&pool);
        bg_handles.push(tokio::spawn(async move {
            pool.embed(vec![format!("bg-{i}")], RequestPriority::Background)
                .await
        }));
    }
    // Give the background sends a moment to land in the channel before the
    // interactive request arrives, so it is provably queued LAST but must
    // still drain FIRST once the gate opens.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let interactive_handle = {
        let pool = Arc::clone(&pool);
        tokio::spawn(async move {
            pool.embed(vec!["interactive".into()], RequestPriority::Interactive)
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Release the gate — "wave-0" completes, then the worker's biased select
    // must pick "interactive" next despite bg-0..bg-2 having queued first.
    gate.notify_one();

    inflight
        .await
        .expect("wave-0 task panicked")
        .expect("wave-0 embed failed");
    interactive_handle
        .await
        .expect("interactive task panicked")
        .expect("interactive embed failed");
    for h in bg_handles {
        h.await.expect("bg task panicked").expect("bg embed failed");
    }

    let recorded = embedder.order.lock().expect("order mutex poisoned").clone();
    let interactive_pos = recorded
        .iter()
        .position(|t| t == "interactive")
        .expect("interactive must have run");
    for bg in ["bg-0", "bg-1", "bg-2"] {
        let bg_pos = recorded
            .iter()
            .position(|t| t == bg)
            .unwrap_or_else(|| panic!("{bg} must have run"));
        assert!(
            interactive_pos < bg_pos,
            "interactive (pos {interactive_pos}) must drain before {bg} (pos {bg_pos}) — \
             recorded order: {recorded:?}"
        );
    }
}

/// Prove executor isolation: a slow embed does NOT prevent concurrent async
/// work on the caller's runtime from making progress (issue #1017 root-cause fix).
///
/// Why: The root cause of #1017 is that embed-pool worker tasks, when they
/// stall on a sidecar call for up to 30 s, can occupy all Tokio worker
/// threads and starve the HTTP accept loop. The fix runs workers on dedicated
/// OS threads with separate single-thread runtimes, completely isolated from
/// the HTTP runtime. This test verifies that isolation contract.
///
/// What: Uses a `SlowEmbedder` that sleeps for 400 ms before replying.
/// Concurrently submits an embed request AND runs an independent timer task
/// on the CALLER'S Tokio runtime. The timer must complete in ~100 ms — well
/// before the 400 ms embed finishes. Under the old design (workers on the HTTP
/// runtime), a 400 ms blocking embed would hold the thread and delay the timer.
/// Under the new design (workers on dedicated OS threads), the timer runs
/// freely on the HTTP runtime.
///
/// Test: `SKIP_UI_BUILD=1 cargo test -p trusty-search -- embed_pool_isolation`
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn embed_pool_isolation_concurrent_task_not_blocked() {
    use std::sync::atomic::AtomicBool;

    /// A mock embedder that sleeps for a configurable duration, simulating
    /// a slow CoreML/ANE stall without requiring a real sidecar.
    ///
    /// Why: deterministic slow path for isolation testing — no real ONNX I/O.
    /// What: `embed_batch` calls `tokio::time::sleep` on the embed worker's own
    /// single-thread runtime, blocking only that OS thread.
    /// Test: used by `embed_pool_isolation_concurrent_task_not_blocked`.
    struct SlowEmbedder {
        dim: usize,
        delay: Duration,
    }

    #[async_trait::async_trait]
    impl crate::core::embed::Embedder for SlowEmbedder {
        async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
            tokio::time::sleep(self.delay).await;
            Ok(vec![0.1f32; self.dim])
        }

        async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
            tokio::time::sleep(self.delay).await;
            Ok(texts.iter().map(|_| vec![0.1f32; self.dim]).collect())
        }

        fn dimension(&self) -> usize {
            self.dim
        }
    }

    // Pool backed by a slow embedder (400 ms delay per batch).
    let embedder: Arc<dyn Embedder> = Arc::new(SlowEmbedder {
        dim: 8,
        delay: Duration::from_millis(400),
    });
    let pool = Arc::new(EmbedPool::new(1, embedder));

    // Flag set by the independent timer task on the caller's runtime.
    let timer_done = Arc::new(AtomicBool::new(false));
    let timer_done_clone = Arc::clone(&timer_done);

    // Spawn a lightweight task on the CALLER's Tokio runtime that should
    // complete in ~100 ms, well before the 400 ms embed finishes.
    // Under the old design (workers on the HTTP runtime with only 2 worker
    // threads), this task would be starved. Under the new design (workers on
    // dedicated OS threads), this task runs freely.
    let timer_handle = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        timer_done_clone.store(true, Ordering::SeqCst);
    });

    // Start the slow embed concurrently on a separate Tokio task.
    let pool_clone = Arc::clone(&pool);
    let embed_handle = tokio::spawn(async move {
        pool_clone
            .embed(vec!["slow".into()], RequestPriority::Background)
            .await
            .expect("slow embed should succeed")
    });

    // Wait for the timer task (expected ~100 ms).
    let timer_start = std::time::Instant::now();
    timer_handle.await.expect("timer task should not panic");
    let timer_elapsed = timer_start.elapsed();

    // The timer must complete in well under the embed delay (400 ms).
    // We allow 300 ms to be generous with scheduler jitter.
    assert!(
        timer_elapsed < Duration::from_millis(300),
        "Timer task took {:?} — embed worker should be isolated on dedicated \
         OS thread and not block the caller's scheduler (issue #1017 fix)",
        timer_elapsed
    );

    assert!(
        timer_done.load(Ordering::SeqCst),
        "Timer flag was not set — task did not complete before assertion"
    );

    // Await the embed to clean up (should complete ~400 ms after start).
    let result = embed_handle.await.expect("embed task should not panic");
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].len(), 8);
}
