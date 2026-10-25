//! Per-request deadline for the interactive query methods (issue #907).
//!
//! Why: Interactive search queries (`search.query`, `search.grep`, etc.) must
//! return within a bounded time — never hang. Long-running operations like
//! reindex and index-file are explicitly excluded so a valid reindex is never
//! cut off by this timer.
//!
//! What: [`with_query_deadline`] wraps a query future in `tokio::time::timeout`.
//! On expiry it refuses with `query_timeout` (a 408 status). The deadline lives
//! in a `QueryTimeoutConfig` on `SearchAppState`, tunable via
//! `TRUSTY_QUERY_TIMEOUT_SECS` (default 30 s); tests inject a short one.
//! #9535 deleted the axum middleware that wrapped it.
//!
//! Config:
//!   - `TRUSTY_QUERY_TIMEOUT_SECS` (default 30) — max wall-clock seconds for
//!     a single search/grep request before it is refused.
//!
//! Test: `query_timeout_returns_408_when_handler_stalls`,
//!        `query_timeout_passes_through_fast_response`.

use axum::http::StatusCode;
use std::sync::Arc;

/// Default wall-clock timeout for interactive query routes (issue #907).
///
/// Why: 30 s is longer than the slowest legitimate query (HNSW + BM25 on a
/// large corpus) but short enough that Claude Code / client-side HTTP timeouts
/// will not fire first, and stalled-embedder requests are detected quickly.
const DEFAULT_QUERY_TIMEOUT_SECS: u64 = 30;

/// The environment variable that sets the interactive-query deadline.
pub const QUERY_TIMEOUT_ENV: &str = "TRUSTY_QUERY_TIMEOUT_SECS";

/// The deadline, in seconds, a `TRUSTY_QUERY_TIMEOUT_SECS` value names.
///
/// Why (#9168): the MCP bridge sizes its client-side query budget from the
/// same value the daemon enforces, so both must parse it the same way.
/// What: the value as `u64`; the 30 s default when absent or unparsable.
/// Test: `query_budget_tracks_the_daemon_deadline`.
pub fn query_timeout_secs(raw: Option<&str>) -> u64 {
    raw.and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_QUERY_TIMEOUT_SECS)
}

/// Shared timeout config, held once on `SearchAppState`.
///
/// Why: storing the deadline in state (rather than reading env vars per call)
/// lets tests inject a tiny timeout without touching global state or
/// process-wide caches.
/// What: a single `Duration` wrapped in `Arc` so it can be cloned cheaply
/// into every request.
/// Test: `query_timeout_returns_408_when_handler_stalls` injects a 50 ms
/// deadline through this type.
#[derive(Clone, Debug)]
pub struct QueryTimeoutConfig {
    /// Wall-clock budget for one interactive query request.
    pub timeout: std::time::Duration,
}

impl QueryTimeoutConfig {
    /// Construct from `TRUSTY_QUERY_TIMEOUT_SECS` env var with a 30 s default.
    ///
    /// Why: called once at daemon boot so per-request env-var lookups are avoided.
    /// What: reads env var, parses as u64, falls back to `DEFAULT_QUERY_TIMEOUT_SECS`.
    /// Test: daemon-boot integration path; unit-tested via `from_secs`.
    pub fn from_env() -> Arc<Self> {
        let secs = query_timeout_secs(std::env::var(QUERY_TIMEOUT_ENV).ok().as_deref());
        tracing::info!("query timeout: {}s (TRUSTY_QUERY_TIMEOUT_SECS)", secs);
        Arc::new(Self {
            timeout: std::time::Duration::from_secs(secs),
        })
    }

    /// Construct with an explicit duration (test helper).
    ///
    /// Why: lets tests inject a tiny deadline without touching env vars or
    /// the process-wide `OnceLock` cache.
    /// What: wraps `duration` in `Arc<QueryTimeoutConfig>`.
    /// Test: used by `query_timeout_returns_408_when_handler_stalls`.
    #[cfg(test)]
    pub fn from_duration(duration: std::time::Duration) -> Arc<Self> {
        Arc::new(Self { timeout: duration })
    }
}

/// The refusal an expired query gets, without the transport (#6285 slice 3).
///
/// Why: the socket serves the same interactive query methods and must report
/// an expiry as the caller's own timeout rather than as an internal fault.
/// What: `408` beside `{"error": "query_timeout", …}`.
/// Test: `query_timeout_returns_408_when_handler_stalls`,
/// `a_query_that_outlasts_the_deadline_reports_the_same_refusal_on_both_transports`.
pub fn timeout_refusal() -> (StatusCode, serde_json::Value) {
    (
        StatusCode::REQUEST_TIMEOUT,
        serde_json::json!({
            "error": "query_timeout",
            "message": "Query exceeded the configured time limit — try a narrower query or retry",
        }),
    )
}

/// Run `fut` under this daemon's interactive-query deadline (#6285 slice 3).
///
/// Why: a socket search against a stalled embedder would otherwise hang with
/// nothing to cancel it. This is the deadline with no transport shape, so every
/// query method applies the same one.
/// What: `tokio::time::timeout` around `fut`, run inside a query scope
/// (#10195); on expiry the future is dropped (cancelling it) and
/// [`timeout_refusal`] is returned.
/// Test: `query_timeout_returns_408_when_handler_stalls`,
/// `a_query_that_outlasts_the_deadline_reports_the_same_refusal_on_both_transports`.
pub async fn with_query_deadline<F>(
    cfg: &QueryTimeoutConfig,
    fut: F,
) -> Result<F::Output, (StatusCode, serde_json::Value)>
where
    F: std::future::Future,
{
    // #10195: the query scope carries the deadline and the current stage, so a
    // search dropped by the timeout below logs its index, query and stage.
    let deadline_at = std::time::Instant::now() + cfg.timeout;
    let scoped = crate::core::indexer::query_timings::scoped(deadline_at, fut);
    match tokio::time::timeout(cfg.timeout, scoped).await {
        Ok(value) => Ok(value),
        Err(_elapsed) => {
            tracing::warn!(
                timeout_secs = cfg.timeout.as_secs(),
                "query_timeout: interactive query exceeded deadline (issue #907)"
            );
            metrics::counter!("trusty_query_timeouts_total").increment(1);
            Err(timeout_refusal())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Verify the happy path: a fast query answers, not a 408.
    ///
    /// Why: the deadline must not break normal queries.
    /// What: a 100 ms deadline around a future that completes immediately;
    /// assert its value comes back.
    /// Test: this test.
    #[tokio::test]
    async fn query_timeout_passes_through_fast_response() {
        let cfg = QueryTimeoutConfig::from_duration(Duration::from_millis(100));

        let answered = with_query_deadline(&cfg, async { "ok" }).await;

        assert_eq!(
            answered.expect("fast query must not be cut off by the deadline"),
            "ok"
        );
    }

    /// Prove that a stalled query is refused with 408 within the deadline.
    ///
    /// Why: this is the invariant from issue #907 — interactive queries must
    /// never hang; the timeout MUST fire and return a clean error.
    /// What: the future stalls with `pending()`; the 50 ms deadline fires; the
    /// refusal must be 408 within ~1 s wall clock.
    /// Test: this test.
    #[tokio::test]
    async fn query_timeout_returns_408_when_handler_stalls() {
        let cfg = QueryTimeoutConfig::from_duration(Duration::from_millis(50));

        let start = std::time::Instant::now();
        let refused = with_query_deadline(&cfg, std::future::pending::<&str>()).await;
        let elapsed = start.elapsed();

        let (status, body) = refused.expect_err("a stalled query must be refused");
        assert_eq!(
            status,
            StatusCode::REQUEST_TIMEOUT,
            "stalled query must receive 408, not hang (elapsed: {elapsed:?})",
        );
        assert_eq!(body["error"], "query_timeout");
        assert!(
            elapsed < Duration::from_secs(2),
            "408 must arrive before the 2 s wall-clock guard (elapsed: {elapsed:?})",
        );
    }
}
