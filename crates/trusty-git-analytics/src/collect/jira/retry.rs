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
//! # `Retry-After` and the backoff budget
//!
//! A server-supplied `Retry-After` **is** honoured, capped, in preference to
//! the exponential schedule. (Round 1 of this PR claimed the header was
//! unreachable because `error_for_status()` consumes the response; that was
//! wrong — see [`super::http::decode`], which reads the headers while the
//! response is still in hand, and classifies 429/503 as
//! [`CollectError::Throttled`].)
//!
//! Honouring the hint makes each attempt *slower*, which matters because
//! `fetch_comments` runs once per ticket: a sustained 429 against a
//! 10,000-ticket backfill could otherwise sleep for the better part of a day
//! while hammering a server that is explicitly asking us to stop. Two bounds
//! prevent that, and they compose:
//!
//! 1. **[`RetryBudget`]** — a whole-run ceiling on accumulated backoff
//!    (default 120s). Once spent, retries stop sleeping and fail immediately,
//!    so total time lost to throttling is bounded no matter how many tickets
//!    or how large the `Retry-After` values.
//! 2. **A consecutive-failure circuit breaker** in `run_sync`, which aborts
//!    the walk rather than issuing tens of thousands of doomed requests.

use std::sync::atomic::{AtomicU64, Ordering};
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
    /// Whole-run ceiling on accumulated backoff across every request. See the
    /// module header — this is the bound that keeps sustained throttling from
    /// turning a 10k-ticket backfill into a multi-hour sleep.
    pub max_total_delay: Duration,
}

impl Default for RetryPolicy {
    /// 4 attempts with 1s / 2s / 4s backoff — ~7s of patience per request,
    /// which absorbs a Jira Cloud rate-limit window without materially
    /// slowing a healthy backfill — under a 120s whole-run backoff ceiling.
    fn default() -> Self {
        Self {
            max_attempts: 4,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(8),
            max_total_delay: Duration::from_secs(120),
        }
    }
}

/// Remaining whole-run backoff allowance, shared by every request a client
/// issues.
///
/// Why a budget rather than a per-request cap: throttling is a property of
/// the *run*, not of any one request. A per-request cap still lets 10,000
/// tickets each sleep their maximum. This makes "time lost to backoff" a
/// single bounded quantity.
#[derive(Debug)]
pub struct RetryBudget {
    remaining_ms: AtomicU64,
}

impl RetryBudget {
    /// Create a budget holding `policy.max_total_delay`.
    pub fn new(policy: &RetryPolicy) -> Self {
        Self {
            remaining_ms: AtomicU64::new(policy.max_total_delay.as_millis() as u64),
        }
    }

    /// Reserve up to `want` from the budget.
    ///
    /// `None` means the budget is spent and the caller must stop retrying.
    /// `Some(d)` is the granted delay, which may legitimately be
    /// [`Duration::ZERO`] — a server answering `Retry-After: 0` is telling us
    /// to retry immediately, which is *not* the same as having no budget
    /// left. Conflating the two turned a "retry now" hint into an aborted
    /// retry; a zero request consumes nothing.
    ///
    /// Test: `budget_grants_until_exhausted`,
    /// `budget_grants_a_zero_delay_without_consuming_anything`.
    pub fn take(&self, want: Duration) -> Option<Duration> {
        let want_ms = want.as_millis() as u64;
        if want_ms == 0 {
            return Some(Duration::ZERO);
        }
        let mut remaining = self.remaining_ms.load(Ordering::Relaxed);
        loop {
            if remaining == 0 {
                return None;
            }
            let grant = want_ms.min(remaining);
            match self.remaining_ms.compare_exchange_weak(
                remaining,
                remaining - grant,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Some(Duration::from_millis(grant)),
                Err(actual) => remaining = actual,
            }
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
    match err {
        // 429/503 are classified up front by `http::decode` so the
        // `Retry-After` hint survives; they are always retryable.
        CollectError::Throttled { .. } => true,
        CollectError::Http(e) => match e.status() {
            Some(status) => status.is_server_error(),
            None => e.is_timeout() || e.is_connect(),
        },
        _ => false,
    }
}

/// The delay to use before `attempt`, preferring a server-supplied
/// `Retry-After` over our own schedule.
fn delay_for(policy: &RetryPolicy, attempt: u32, err: &CollectError) -> Duration {
    match err {
        CollectError::Throttled {
            retry_after: Some(hint),
            ..
        } => *hint,
        _ => delay_for_attempt(policy, attempt),
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
/// attempt budget — or the run-wide [`RetryBudget`] — is returned unchanged;
/// the caller still sees a failure, it simply took longer to conclude one.
pub async fn with_retry<T, F, Fut>(
    label: &str,
    policy: &RetryPolicy,
    budget: &RetryBudget,
    mut op: F,
) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let mut attempt = 1u32;
    loop {
        match op().await {
            Ok(v) => return Ok(v),
            Err(e) if attempt < policy.max_attempts && is_retryable(&e) => {
                let want = delay_for(policy, attempt + 1, &e);
                let Some(granted) = budget.take(want) else {
                    warn!(
                        op = label,
                        error = %e,
                        "run-wide retry budget exhausted; refusing to keep sleeping \
                         against a throttling server"
                    );
                    return Err(e);
                };
                warn!(
                    op = label,
                    attempt,
                    max_attempts = policy.max_attempts,
                    delay_ms = granted.as_millis() as u64,
                    honoured_retry_after = matches!(
                        e,
                        CollectError::Throttled {
                            retry_after: Some(_),
                            ..
                        }
                    ),
                    error = %e,
                    "transient JIRA failure; retrying after backoff"
                );
                tokio::time::sleep(granted).await;
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
            max_total_delay: Duration::from_secs(10),
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

    #[test]
    fn is_retryable_accepts_throttling() {
        assert!(is_retryable(&CollectError::Throttled {
            status: 429,
            retry_after: None,
        }));
    }

    /// A server-supplied `Retry-After` must win over our own schedule —
    /// the whole point of reading the header.
    #[test]
    fn retry_after_hint_overrides_the_exponential_schedule() {
        let policy = RetryPolicy::default();
        let throttled = CollectError::Throttled {
            status: 429,
            retry_after: Some(Duration::from_secs(17)),
        };
        assert_eq!(delay_for(&policy, 2, &throttled), Duration::from_secs(17));

        let no_hint = CollectError::Throttled {
            status: 429,
            retry_after: None,
        };
        assert_eq!(
            delay_for(&policy, 2, &no_hint),
            policy.base_delay,
            "without a hint we fall back to the exponential schedule"
        );
    }

    #[test]
    fn budget_grants_until_exhausted() {
        let budget = RetryBudget::new(&RetryPolicy {
            max_total_delay: Duration::from_millis(150),
            ..RetryPolicy::default()
        });
        assert_eq!(
            budget.take(Duration::from_millis(100)),
            Some(Duration::from_millis(100))
        );
        assert_eq!(
            budget.take(Duration::from_millis(100)),
            Some(Duration::from_millis(50)),
            "a partial grant is still progress"
        );
        assert_eq!(
            budget.take(Duration::from_millis(100)),
            None,
            "an exhausted budget must refuse so the caller stops"
        );
    }

    /// `Retry-After: 0` means "retry immediately", not "stop retrying".
    /// Conflating a zero-length grant with an exhausted budget aborted the
    /// retry on the one hint asking for the fastest possible one — caught by
    /// `a_429_is_retried_and_its_retry_after_is_read` before it shipped.
    #[test]
    fn budget_grants_a_zero_delay_without_consuming_anything() {
        let budget = RetryBudget::new(&RetryPolicy {
            max_total_delay: Duration::from_millis(10),
            ..RetryPolicy::default()
        });
        assert_eq!(budget.take(Duration::ZERO), Some(Duration::ZERO));
        assert_eq!(
            budget.take(Duration::from_millis(10)),
            Some(Duration::from_millis(10)),
            "the zero grant must not have spent any of the budget"
        );
    }

    fn fast_policy() -> RetryPolicy {
        RetryPolicy {
            max_attempts: 4,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(1),
            max_total_delay: Duration::from_millis(100),
        }
    }

    /// A permanent (non-retryable) error must be returned on the first
    /// attempt — no sleeping, no extra calls.
    #[tokio::test]
    async fn with_retry_does_not_retry_permanent_errors() {
        let policy = fast_policy();
        let budget = RetryBudget::new(&policy);
        let mut calls = 0usize;
        let result: Result<()> = with_retry("test", &policy, &budget, || {
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
        let policy = fast_policy();
        let budget = RetryBudget::new(&policy);
        let mut calls = 0usize;
        let value = with_retry("test", &policy, &budget, || {
            calls += 1;
            async { Ok(7u32) }
        })
        .await
        .expect("succeeds");
        assert_eq!(value, 7);
        assert_eq!(calls, 1);
    }

    /// The whole-run bound: once the budget is spent, a throttled request
    /// fails immediately instead of sleeping. This is what keeps a sustained
    /// 429 against a 10k-ticket backfill from turning into hours of sleep.
    #[tokio::test]
    async fn with_retry_stops_sleeping_once_the_run_budget_is_spent() {
        let policy = RetryPolicy {
            max_attempts: 10,
            base_delay: Duration::from_millis(10),
            max_delay: Duration::from_millis(10),
            max_total_delay: Duration::from_millis(10),
        };
        let budget = RetryBudget::new(&policy);
        let mut calls = 0usize;
        let result: Result<()> = with_retry("test", &policy, &budget, || {
            calls += 1;
            async {
                Err(CollectError::Throttled {
                    status: 429,
                    retry_after: None,
                })
            }
        })
        .await;
        assert!(result.is_err());
        assert_eq!(
            calls, 2,
            "one sleep exhausts the 10ms budget, so the second failure is final \
             even though max_attempts is 10"
        );
    }
}
