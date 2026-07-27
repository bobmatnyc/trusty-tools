//! Bounded retry-with-backoff for JIRA HTTP requests (issue #3966,
//! PR #4067 review round 1).
//!
//! Why: `fetch_comments` issues one request *per ticket*. Against a
//! 10,000-ticket backfill, a JIRA Cloud rate-limit response (429) or a
//! transient 5xx is close to certain. Without a retry a single throttled
//! response turns into a per-ticket ingestion failure, which now (correctly)
//! holds the incremental cursor back and fails the whole run — so the cheap
//! fix is to not let a transient blip become a failure in the first place.
//!
//! What: [`with_retry`] re-runs a fallible async operation while the error is
//! classified retryable by [`is_retryable`], sleeping an exponentially
//! growing, capped delay between attempts.
//!
//! Deliberately NOT retried: 4xx other than 429 (auth/permission/not-found
//! are permanent for the life of the run), and JSON decode failures (a
//! malformed payload will decode identically on the next attempt).
//!
//! `Retry-After` is not yet honoured — `reqwest`'s `error_for_status()`
//! consumes the response before we can read its headers. The exponential
//! schedule below is a strict superset of the usual 1s Jira Cloud hint for
//! the first two attempts, so this is a conservative approximation rather
//! than a correctness gap.

use std::time::Duration;

use tracing::warn;

use crate::collect::errors::{CollectError, Result};

/// Retry schedule for one JIRA client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Total attempts including the first. `1` disables retrying.
    pub max_attempts: u32,
    /// Delay before the second attempt; doubles each subsequent attempt.
    pub base_delay: Duration,
    /// Ceiling applied to the doubling schedule.
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    /// 4 attempts with 1s / 2s / 4s backoff — ~7s of patience per request,
    /// which absorbs a Jira Cloud rate-limit window without materially
    /// slowing a healthy backfill.
    fn default() -> Self {
        Self {
            max_attempts: 4,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(8),
        }
    }
}

/// Delay to sleep *before* attempt number `attempt` (1-based; attempt 1 is
/// the initial try and never sleeps).
///
/// Why a pure function: makes the backoff schedule unit-testable without
/// actually sleeping.
/// What: `base_delay * 2^(attempt - 2)`, clamped to `max_delay`.
/// Test: `delay_schedule_doubles_and_clamps`, `delay_for_first_attempt_is_zero`.
pub fn delay_for_attempt(policy: &RetryPolicy, attempt: u32) -> Duration {
    if attempt <= 1 {
        return Duration::ZERO;
    }
    let shift = (attempt - 2).min(16);
    let scaled = policy
        .base_delay
        .saturating_mul(1u32.checked_shl(shift).unwrap_or(u32::MAX));
    scaled.min(policy.max_delay)
}

/// Classify whether an error is worth retrying.
///
/// Why: retrying a 401 or a 404 wastes the operator's time and hides the
/// real problem; retrying a 429/503 is the difference between a completed
/// backfill and a held cursor.
/// What: retryable = HTTP 429, any 5xx, or a transport-level timeout/connect
/// failure. Everything else — including decode errors and other 4xx — is
/// permanent.
/// Test: `is_retryable_rejects_non_http_errors`, plus the end-to-end
/// `fetch_comments_retries_a_transient_500` in `client_tests.rs`.
pub fn is_retryable(err: &CollectError) -> bool {
    let CollectError::Http(e) = err else {
        return false;
    };
    match e.status() {
        Some(status) => {
            status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
        }
        None => e.is_timeout() || e.is_connect(),
    }
}

/// Run `op` until it succeeds, its error is classified permanent, or
/// `policy.max_attempts` is exhausted.
///
/// `label` names the operation in the retry warning so an operator reading
/// logs can tell which request is flapping.
///
/// # Errors
///
/// Returns the final attempt's error. A retryable error that outlives the
/// attempt budget is returned unchanged — the caller still sees a failure,
/// it simply took longer to conclude one.
pub async fn with_retry<T, F, Fut>(label: &str, policy: &RetryPolicy, mut op: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let mut attempt = 1u32;
    loop {
        match op().await {
            Ok(v) => return Ok(v),
            Err(e) if attempt < policy.max_attempts && is_retryable(&e) => {
                let delay = delay_for_attempt(policy, attempt + 1);
                warn!(
                    op = label,
                    attempt,
                    max_attempts = policy.max_attempts,
                    delay_ms = delay.as_millis() as u64,
                    error = %e,
                    "transient JIRA failure; retrying after backoff"
                );
                tokio::time::sleep(delay).await;
                attempt += 1;
            }
            Err(e) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delay_for_first_attempt_is_zero() {
        let policy = RetryPolicy::default();
        assert_eq!(delay_for_attempt(&policy, 1), Duration::ZERO);
        assert_eq!(delay_for_attempt(&policy, 0), Duration::ZERO);
    }

    #[test]
    fn delay_schedule_doubles_and_clamps() {
        let policy = RetryPolicy {
            max_attempts: 6,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_millis(400),
        };
        assert_eq!(delay_for_attempt(&policy, 2), Duration::from_millis(100));
        assert_eq!(delay_for_attempt(&policy, 3), Duration::from_millis(200));
        assert_eq!(delay_for_attempt(&policy, 4), Duration::from_millis(400));
        assert_eq!(
            delay_for_attempt(&policy, 5),
            Duration::from_millis(400),
            "the schedule must clamp at max_delay rather than growing forever"
        );
    }

    #[test]
    fn is_retryable_rejects_non_http_errors() {
        assert!(!is_retryable(&CollectError::Config("bad config".into())));
        assert!(!is_retryable(&CollectError::Identity("nope".into())));
    }

    /// A permanent (non-retryable) error must be returned on the first
    /// attempt — no sleeping, no extra calls.
    #[tokio::test]
    async fn with_retry_does_not_retry_permanent_errors() {
        let policy = RetryPolicy {
            max_attempts: 4,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(1),
        };
        let mut calls = 0usize;
        let result: Result<()> = with_retry("test", &policy, || {
            calls += 1;
            async { Err(CollectError::Config("permanent".into())) }
        })
        .await;
        assert!(result.is_err());
        assert_eq!(calls, 1, "a permanent error must not be retried");
    }

    /// A succeeding operation runs exactly once.
    #[tokio::test]
    async fn with_retry_returns_first_success_without_retrying() {
        let policy = RetryPolicy::default();
        let mut calls = 0usize;
        let value = with_retry("test", &policy, || {
            calls += 1;
            async { Ok(7u32) }
        })
        .await
        .expect("succeeds");
        assert_eq!(value, 7);
        assert_eq!(calls, 1);
    }
}
