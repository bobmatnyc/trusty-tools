//! Concurrency limiter for the interactive query methods (issue #41 Phase 1).
//!
//! Why: Without bounded in-flight concurrency, a burst of search requests can
//! saturate memory (each in-flight reindex holds
//! a parsed batch + embeddings + HNSW write lock) and degrade *every*
//! response, including cheap ones like `/health`. A semaphore-based limiter
//! caps in-flight work; a bounded waiting queue absorbs short bursts. Beyond
//! the queue depth callers are refused `server_busy` (a 503 status) so they
//! back off and try again instead of piling up more pressure.
//!
//! Additionally (issue #907): the semaphore `.acquire_owned().await` is now
//! bounded by `TRUSTY_QUEUE_TIMEOUT_SECS` (default 30 s). When the wait
//! exceeds the deadline the request returns 503 immediately rather than
//! hanging indefinitely behind a stalled reindex or a burst that never clears.
//!
//! What: One `Arc<ConcurrencyLimiter>` on `SearchAppState`. [`admit`]
//! attempts a non-blocking `Semaphore::try_acquire`, and if that fails,
//! checks whether the bounded waiting queue still has room (via the
//! `waiting` `AtomicUsize`). When the queue is also full it refuses with 503
//! immediately; otherwise it waits for a permit (bounded by the queue
//! timeout). #9535 deleted the axum middleware that wrapped it.
//!
//! Config:
//!   - `TRUSTY_MAX_CONCURRENT_REQUESTS` (default 8) — semaphore permits.
//!   - `TRUSTY_QUEUE_DEPTH` (default 32) — max waiters before 503.
//!   - `TRUSTY_QUEUE_TIMEOUT_SECS` (default 30) — max wait for a permit (issue #907).
//!
//! Test: covered by `tests` at the bottom — limit, queue, 503, and queue-timeout paths.

use axum::http::StatusCode;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::Semaphore;

/// Default in-flight cap when `TRUSTY_MAX_CONCURRENT_REQUESTS` is unset.
const DEFAULT_MAX_CONCURRENT: usize = 8;

/// Default waiting-queue depth when `TRUSTY_QUEUE_DEPTH` is unset.
const DEFAULT_QUEUE_DEPTH: usize = 32;

/// Default bounded wait for a concurrency-semaphore permit (issue #907).
///
/// Why: `acquire_owned().await` blocks forever when all permits are held by a
/// stalled reindex. 30 s is long enough to absorb normal burst traffic and
/// short enough that a user query never hangs past a client's own HTTP timeout.
const DEFAULT_QUEUE_TIMEOUT_SECS: u64 = 30;

/// Read `TRUSTY_QUEUE_TIMEOUT_SECS` once and cache it.
///
/// Why: avoids repeated env lookups per request while still allowing tests to
/// override via `std::env::set_var` before the first call.
/// What: reads env var, parses as u64, falls back to `DEFAULT_QUEUE_TIMEOUT_SECS`.
/// Test: `queue_wait_returns_503_on_timeout`.
fn queue_timeout() -> std::time::Duration {
    static CACHED: std::sync::OnceLock<std::time::Duration> = std::sync::OnceLock::new();
    *CACHED.get_or_init(|| {
        let secs = std::env::var("TRUSTY_QUEUE_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(DEFAULT_QUEUE_TIMEOUT_SECS);
        std::time::Duration::from_secs(secs)
    })
}

/// Shared limiter state, held once on `SearchAppState`.
///
/// Why: a single semaphore + atomic counter shared across handlers is the
/// minimal correct implementation. We deliberately avoid Tower's
/// `ConcurrencyLimitLayer` because it has no graceful 503 path — it just
/// queues unboundedly.
/// What: `semaphore` enforces in-flight count, `waiting` tracks queued-but-
/// not-yet-admitted requests so we can fast-fail the (N+1)th waiter.
/// `queue_timeout` bounds the `.acquire_owned().await` so a request can
/// never hang indefinitely behind a stalled index operation (issue #907).
/// Test: `limiter_returns_503_when_queue_full`, `limiter_admits_up_to_concurrency`,
/// `queue_wait_returns_503_on_timeout`.
pub struct ConcurrencyLimiter {
    semaphore: Arc<Semaphore>,
    queue_depth: usize,
    waiting: Arc<AtomicUsize>,
    max_concurrent: usize,
    /// Bounded wait deadline for a semaphore permit (issue #907).
    queue_timeout: std::time::Duration,
}

impl ConcurrencyLimiter {
    /// Construct a limiter using env-tuned defaults.
    ///
    /// Why: `start.rs` calls this once at daemon boot; no need to expose the
    /// internal knobs to callers.
    /// What: reads `TRUSTY_MAX_CONCURRENT_REQUESTS` and `TRUSTY_QUEUE_DEPTH`
    /// from the environment, falling back to the constants above.
    /// Test: `from_env_uses_defaults_when_unset`.
    pub fn from_env() -> Arc<Self> {
        let max_concurrent = std::env::var("TRUSTY_MAX_CONCURRENT_REQUESTS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .map(|n| n.max(1))
            .unwrap_or(DEFAULT_MAX_CONCURRENT);
        let queue_depth = std::env::var("TRUSTY_QUEUE_DEPTH")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(DEFAULT_QUEUE_DEPTH);
        tracing::info!(
            "concurrency limiter: max_concurrent={} queue_depth={}",
            max_concurrent,
            queue_depth
        );
        Arc::new(Self {
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
            queue_depth,
            waiting: Arc::new(AtomicUsize::new(0)),
            max_concurrent,
            queue_timeout: queue_timeout(),
        })
    }

    /// Construct a limiter with explicit knobs (tests, integration callers).
    #[cfg(test)]
    pub fn with_limits(max_concurrent: usize, queue_depth: usize) -> Arc<Self> {
        Self::with_limits_and_timeout(
            max_concurrent,
            queue_depth,
            std::time::Duration::from_secs(DEFAULT_QUEUE_TIMEOUT_SECS),
        )
    }

    /// Construct a limiter with explicit knobs including a custom queue timeout.
    ///
    /// Why: allows tests to inject a very short queue timeout to prove the
    /// 503-on-timeout path fires without actually waiting 30 s.
    /// What: same as `with_limits` but overrides the queue-wait deadline.
    /// Test: `queue_wait_returns_503_on_timeout`.
    #[cfg(test)]
    pub fn with_limits_and_timeout(
        max_concurrent: usize,
        queue_depth: usize,
        queue_timeout: std::time::Duration,
    ) -> Arc<Self> {
        Arc::new(Self {
            semaphore: Arc::new(Semaphore::new(max_concurrent.max(1))),
            queue_depth,
            waiting: Arc::new(AtomicUsize::new(0)),
            max_concurrent: max_concurrent.max(1),
            queue_timeout,
        })
    }

    /// Current number of waiters (admitted to the queue but not yet holding
    /// a permit). Exposed for metrics.
    pub fn waiting(&self) -> usize {
        self.waiting.load(Ordering::Relaxed)
    }

    /// Maximum in-flight permits. Exposed for metrics/logging.
    pub fn max_concurrent(&self) -> usize {
        self.max_concurrent
    }
}

/// The refusal a caller that could not be admitted gets, without the transport
/// (#6285 slice 3).
///
/// Why: the socket's query methods carry this `error` and `message` in the
/// refusal's `data`; `RpcError` has nowhere to put a `Retry-After` header.
/// What: `503` beside `{"error": "server_busy", …}`.
/// Test: `queries_are_refused_when_the_shared_limiter_is_saturated`.
pub fn busy_refusal() -> (StatusCode, serde_json::Value) {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        serde_json::json!({
            "error": "server_busy",
            "message": "Request queue full, retry shortly",
        }),
    )
}

/// Take one admission slot, or report why the caller cannot have one.
///
/// Why (#6285 slice 3): the admission decision with no transport shape, so
/// every socket query method gates on the SAME semaphore.
/// What: the fast-path `try_acquire_owned`, then the bounded queue — a full
/// queue and a queue-wait that outlasts `queue_timeout` both answer
/// [`busy_refusal`]. The returned permit holds the slot; dropping it releases
/// it, so a caller must keep it alive for as long as the work runs.
/// Test: `limiter_returns_503_when_queue_full`, `queue_wait_returns_503_on_timeout`,
/// `queries_are_refused_when_the_shared_limiter_is_saturated`.
pub async fn admit(
    limiter: &Arc<ConcurrencyLimiter>,
) -> Result<tokio::sync::OwnedSemaphorePermit, (StatusCode, serde_json::Value)> {
    // Fast-path: try to acquire a permit without waiting. The common case is
    // an idle daemon where there's always a permit free.
    if let Ok(permit) = limiter.semaphore.clone().try_acquire_owned() {
        return Ok(permit);
    }

    // No permit available — try to admit to the waiting queue.
    let current_waiters = limiter.waiting.fetch_add(1, Ordering::Relaxed);
    metrics::gauge!("trusty_queue_depth").set((current_waiters + 1) as f64);
    if current_waiters >= limiter.queue_depth {
        // Queue full — back off the waiter counter and reject.
        limiter.waiting.fetch_sub(1, Ordering::Relaxed);
        metrics::gauge!("trusty_queue_depth").set(limiter.waiting.load(Ordering::Relaxed) as f64);
        metrics::counter!("trusty_requests_rejected_total").increment(1);
        tracing::warn!("concurrency limiter: queue full, returning 503");
        return Err(busy_refusal());
    }
    // Wait for a permit, bounded by the queue timeout (issue #907).
    // On timeout, return 503 immediately — never hang indefinitely.
    let deadline = limiter.queue_timeout;
    let acquired = tokio::time::timeout(deadline, limiter.semaphore.clone().acquire_owned()).await;
    limiter.waiting.fetch_sub(1, Ordering::Relaxed);
    metrics::gauge!("trusty_queue_depth").set(limiter.waiting.load(Ordering::Relaxed) as f64);
    match acquired {
        Err(_elapsed) => {
            // Timed out waiting for a permit — refuse with the busy verdict so
            // clients back off gracefully.
            metrics::counter!("trusty_requests_rejected_total").increment(1);
            tracing::warn!(
                timeout_secs = deadline.as_secs(),
                "concurrency limiter: queue-wait timed out, returning 503 (issue #907)"
            );
            Err(busy_refusal())
        }
        Ok(Ok(permit)) => Ok(permit),
        // Semaphore closed — should never happen during normal operation; fail closed.
        Ok(Err(_)) => Err(busy_refusal()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // #5937: removes TRUSTY_MAX_CONCURRENT_REQUESTS / TRUSTY_QUEUE_DEPTH; join the #[serial] env group.
    #[serial_test::serial]
    #[tokio::test]
    async fn from_env_uses_defaults_when_unset() {
        // SAFETY: env-mutating; this test must not run concurrently with
        // others that touch TRUSTY_MAX_CONCURRENT_REQUESTS. cargo's default
        // test parallelism is acceptable here because the assertion only
        // checks that *some* sane default is returned.
        std::env::remove_var("TRUSTY_MAX_CONCURRENT_REQUESTS");
        std::env::remove_var("TRUSTY_QUEUE_DEPTH");
        let limiter = ConcurrencyLimiter::from_env();
        assert_eq!(limiter.max_concurrent(), DEFAULT_MAX_CONCURRENT);
    }

    #[tokio::test]
    async fn limiter_admits_up_to_concurrency() {
        let limiter = ConcurrencyLimiter::with_limits(2, 4);

        let first = admit(&limiter).await.expect("first caller is admitted");
        let second = admit(&limiter).await.expect("second caller is admitted");
        assert_eq!(limiter.waiting(), 0, "neither caller had to queue");
        drop((first, second));
    }

    #[tokio::test]
    async fn limiter_returns_503_when_queue_full() {
        // 1 concurrent, queue depth 0 — a second caller while the only permit
        // is held is refused immediately.
        let limiter = ConcurrencyLimiter::with_limits(1, 0);
        let held = admit(&limiter).await.expect("first caller is admitted");

        let (status, body) = admit(&limiter).await.expect_err("queue is full");
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["error"], "server_busy");
        assert_eq!(limiter.waiting(), 0, "a refused caller leaves the queue");
        drop(held);
    }

    /// Prove that a caller waiting in the semaphore queue is refused with 503
    /// (not a hang) when the queue-wait deadline expires (issue #907 fix 2).
    ///
    /// Why: before the fix `.acquire_owned().await` blocked forever when all
    /// permits were held by a stalled operation. The fix wraps the await in
    /// `tokio::time::timeout`; this test proves it fires deterministically.
    /// What: 1 permit, queue depth 1, timeout 50 ms. The test holds the only
    /// permit, so the second caller is admitted to the queue and must be
    /// refused after ~50 ms rather than wait for a permit that never frees.
    /// Test: this test.
    #[tokio::test]
    async fn queue_wait_returns_503_on_timeout() {
        let limiter = ConcurrencyLimiter::with_limits_and_timeout(1, 1, Duration::from_millis(50));
        let held = admit(&limiter).await.expect("first caller is admitted");

        let start = std::time::Instant::now();
        let refused = admit(&limiter).await;
        let elapsed = start.elapsed();

        let (status, body) = refused.expect_err("queue-wait timeout must refuse");
        assert_eq!(
            status,
            StatusCode::SERVICE_UNAVAILABLE,
            "queue-wait timeout must return 503, got {status} (elapsed: {elapsed:?})",
        );
        assert_eq!(body["error"], "server_busy");
        assert!(
            elapsed < Duration::from_secs(2),
            "queue-wait timeout must not block indefinitely (elapsed: {elapsed:?})",
        );
        assert_eq!(limiter.waiting(), 0, "a timed-out caller leaves the queue");
        drop(held);
    }
}
