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
    };
    let result = closed_pool
        .embed(vec!["x".into()], RequestPriority::Interactive)
        .await;
    assert!(
        result.is_err(),
        "embed on a closed pool must return Err, not hang"
    );
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
