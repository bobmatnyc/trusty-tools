//! Concurrency limiter middleware for expensive HTTP endpoints (issue #41
//! Phase 1).
//!
//! Why: Without bounded in-flight concurrency, a burst of `POST /search` or
//! `POST /reindex` requests can saturate memory (each in-flight reindex holds
//! a parsed batch + embeddings + HNSW write lock) and degrade *every*
//! response, including cheap ones like `/health`. A semaphore-based limiter
//! caps in-flight work; a bounded waiting queue absorbs short bursts. Beyond
//! the queue depth callers receive `503 Service Unavailable` with a
//! `Retry-After` header so they back off and try again instead of piling up
//! more pressure.
//!
//! Additionally (issue #907): the semaphore `.acquire_owned().await` is now
//! bounded by `TRUSTY_QUEUE_TIMEOUT_SECS` (default 30 s). When the wait
//! exceeds the deadline the request returns 503 immediately rather than
//! hanging indefinitely behind a stalled reindex or a burst that never clears.
//!
//! What: An `Arc<ConcurrencyLimiter>` installed as an axum `Extension`. The
//! middleware fn `apply_limiter` reads it out of the request extensions,
//! attempts a non-blocking `Semaphore::try_acquire`, and if that fails,
//! checks whether the bounded waiting queue still has room (via the
//! `waiting` `AtomicUsize`). When the queue is also full it returns 503
//! immediately; otherwise it waits for a permit (bounded by the queue
//! timeout). Light endpoints (`/health`, `/metrics`, `/indexes`) bypass this
//! middleware entirely by not being wrapped in the limited router subtree.
//!
//! Config:
//!   - `TRUSTY_MAX_CONCURRENT_REQUESTS` (default 8) — semaphore permits.
//!   - `TRUSTY_QUEUE_DEPTH` (default 32) — max waiters before 503.
//!   - `TRUSTY_QUEUE_TIMEOUT_SECS` (default 30) — max wait for a permit (issue #907).
//!
//! Test: covered by `tests` at the bottom — limit, queue, 503, and queue-timeout paths.

use axum::{
    body::Body,
    extract::Extension,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Json, Response},
};
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

/// Shared limiter state, cloned into every request via axum's `Extension`.
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

/// Build the 503 response body used when the queue is full.
fn busy_response() -> Response {
    let body = Json(serde_json::json!({
        "error": "server_busy",
        "message": "Request queue full, retry shortly",
    }));
    let mut resp = (StatusCode::SERVICE_UNAVAILABLE, body).into_response();
    resp.headers_mut().insert(
        axum::http::header::RETRY_AFTER,
        axum::http::HeaderValue::from_static("2"),
    );
    resp
}

/// Axum middleware that gates the wrapped handler behind the limiter.
///
/// Why: applying the limiter selectively (only to expensive endpoints) means
/// the middleware needs to be attachable to individual routes, not the whole
/// router. axum's `from_fn_with_state`-style middleware is the cleanest fit.
/// What: increments the waiter counter, fast-fails with 503 if the queue is
/// already at depth, then awaits a semaphore permit. On admission, the
/// `trusty_queue_depth` gauge is updated and the request flows downstream.
/// On exit, the permit is automatically dropped (releasing the slot) and
/// the gauge is decremented.
/// Test: `limiter_returns_503_when_queue_full`.
pub async fn apply_limiter(
    Extension(limiter): Extension<Arc<ConcurrencyLimiter>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    // Fast-path: try to acquire a permit without waiting. The common case is
    // an idle daemon where there's always a permit free.
    let permit = limiter.semaphore.clone().try_acquire_owned().ok();

    let permit = match permit {
        Some(p) => p,
        None => {
            // No permit available — try to admit to the waiting queue.
            let current_waiters = limiter.waiting.fetch_add(1, Ordering::Relaxed);
            metrics::gauge!("trusty_queue_depth").set((current_waiters + 1) as f64);
            if current_waiters >= limiter.queue_depth {
                // Queue full — back off the waiter counter and reject.
                limiter.waiting.fetch_sub(1, Ordering::Relaxed);
                metrics::gauge!("trusty_queue_depth")
                    .set(limiter.waiting.load(Ordering::Relaxed) as f64);
                metrics::counter!("trusty_requests_rejected_total").increment(1);
                tracing::warn!("concurrency limiter: queue full, returning 503");
                return busy_response();
            }
            // Wait for a permit, bounded by the queue timeout (issue #907).
            // On timeout, return 503 immediately — never hang indefinitely.
            let deadline = limiter.queue_timeout;
            let acquired =
                tokio::time::timeout(deadline, limiter.semaphore.clone().acquire_owned()).await;
            limiter.waiting.fetch_sub(1, Ordering::Relaxed);
            metrics::gauge!("trusty_queue_depth")
                .set(limiter.waiting.load(Ordering::Relaxed) as f64);
            match acquired {
                Err(_elapsed) => {
                    // Timed out waiting for a permit — return busy/503 with a
                    // Retry-After header so clients back off gracefully.
                    metrics::counter!("trusty_requests_rejected_total").increment(1);
                    tracing::warn!(
                        timeout_secs = deadline.as_secs(),
                        "concurrency limiter: queue-wait timed out, returning 503 (issue #907)"
                    );
                    return busy_response();
                }
                Ok(Ok(p)) => p,
                Ok(Err(_)) => {
                    // Semaphore closed — should never happen during normal
                    // operation; fail closed.
                    return busy_response();
                }
            }
        }
    };

    let response = next.run(request).await;
    // Permit drops here, releasing the slot for the next waiter.
    drop(permit);
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        routing::get,
        Router,
    };
    use std::time::Duration;
    use tower::ServiceExt;

    fn limited_router(limiter: Arc<ConcurrencyLimiter>) -> Router {
        Router::new()
            .route(
                "/slow",
                get(|| async {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    "ok"
                }),
            )
            .route_layer(axum::middleware::from_fn(apply_limiter))
            .layer(Extension(limiter))
    }

    /// Build a router whose `/forever` handler signals a oneshot once it holds
    /// the permit, then stalls — simulates a blocked embedder call (issue #907).
    ///
    /// Why: the handler must notify the test harness *after* it acquires the
    /// permit so the second request is sent only when the semaphore is
    /// exhausted. This makes the test deterministic; a time-based sleep was
    /// the flaky pattern this replaces.
    /// What: returns the router and a oneshot receiver that fires when the
    /// in-flight handler has acquired its permit.
    /// Test: `queue_wait_returns_503_on_timeout`.
    fn forever_router_with_signal(
        limiter: Arc<ConcurrencyLimiter>,
    ) -> (Router, tokio::sync::oneshot::Receiver<()>) {
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        // Wrap in Arc<Mutex<Option<…>>> so we can move into the async closure
        // and take the sender exactly once.
        let tx = std::sync::Arc::new(tokio::sync::Mutex::new(Some(tx)));
        let router = Router::new()
            .route(
                "/forever",
                get(move || {
                    let tx = std::sync::Arc::clone(&tx);
                    async move {
                        // Signal: we now hold the permit.
                        if let Some(sender) = tx.lock().await.take() {
                            let _ = sender.send(());
                        }
                        // Stall forever — never resolves during the test.
                        std::future::pending::<&str>().await
                    }
                }),
            )
            .route_layer(axum::middleware::from_fn(apply_limiter))
            .layer(Extension(limiter));
        (router, rx)
    }

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
        let app = limited_router(limiter);

        let req = || {
            Request::builder()
                .uri("/slow")
                .body(Body::empty())
                .expect("valid request")
        };
        let r1 = app.clone().oneshot(req());
        let r2 = app.clone().oneshot(req());
        let (res1, res2) = tokio::join!(r1, r2);
        assert_eq!(res1.unwrap().status(), StatusCode::OK);
        assert_eq!(res2.unwrap().status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn limiter_returns_503_when_queue_full() {
        // 1 concurrent, queue depth 0 — the second simultaneous request
        // should be rejected immediately.
        let limiter = ConcurrencyLimiter::with_limits(1, 0);
        let app = limited_router(limiter);

        let req = || {
            Request::builder()
                .uri("/slow")
                .body(Body::empty())
                .expect("valid request")
        };
        // Kick off a slow request that holds the only permit for ~100ms.
        let in_flight = tokio::spawn(app.clone().oneshot(req()));
        // Yield once so the first request has a chance to grab the permit.
        tokio::time::sleep(Duration::from_millis(10)).await;
        // Second request: should 503 immediately.
        let rejected = app.oneshot(req()).await.expect("oneshot returns");
        assert_eq!(rejected.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            rejected
                .headers()
                .get(axum::http::header::RETRY_AFTER)
                .map(|v| v.to_str().unwrap()),
            Some("2")
        );
        let _ = in_flight.await;
    }

    /// Prove that a request waiting in the semaphore queue returns 503 (not a
    /// hang) when the queue-wait deadline expires (issue #907 fix 2).
    ///
    /// Why: before the fix `.acquire_owned().await` blocked forever when all
    /// permits were held by a stalled operation. The fix wraps the await in
    /// `tokio::time::timeout`; this test proves it fires deterministically.
    /// What: 1 permit, queue depth 1, timeout 50 ms. A first request holds the
    /// permit and fires a oneshot once admitted. Only after that signal is the
    /// second request sent, guaranteeing the semaphore is exhausted. The second
    /// request must receive 503 after ~50 ms, not hang.
    /// Test: this test.
    #[tokio::test]
    async fn queue_wait_returns_503_on_timeout() {
        // 1 permit, 1 queue slot, 50 ms deadline — the second request will be
        // admitted to the queue but time out before the first request finishes.
        let limiter = ConcurrencyLimiter::with_limits_and_timeout(1, 1, Duration::from_millis(50));
        let (app, permit_acquired) = forever_router_with_signal(limiter);

        let req = || {
            Request::builder()
                .uri("/forever")
                .body(Body::empty())
                .expect("valid request")
        };

        // Kick off the first request — it grabs the only permit and stalls.
        let _in_flight = tokio::spawn(app.clone().oneshot(req()));

        // Wait until the first request signals it holds the permit.  This
        // replaces the timing-sensitive `sleep(5ms)` with a deterministic
        // signal so the second request is sent only after admission is
        // confirmed and the semaphore is definitely exhausted.
        permit_acquired
            .await
            .expect("in-flight handler must send the permit-acquired signal");

        // Second request: admitted to the queue but should 503 after ~50 ms.
        let start = std::time::Instant::now();
        let waiting = app.oneshot(req()).await.expect("oneshot returns");
        let elapsed = start.elapsed();

        assert_eq!(
            waiting.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "queue-wait timeout must return 503, got {} (elapsed: {:?})",
            waiting.status(),
            elapsed,
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "queue-wait timeout must not block indefinitely (elapsed: {:?})",
            elapsed,
        );
        assert_eq!(
            waiting
                .headers()
                .get(axum::http::header::RETRY_AFTER)
                .map(|v| v.to_str().unwrap()),
            Some("2"),
            "503 response must include Retry-After header"
        );
    }
}
