//! Shared HTTP retry helper for every GitHub API call in this crate.
//!
//! Why: the PR client, org discovery, and the per-reference issue lookup all
//! need backoff on transient failures, and before #6084 each bounded only
//! itself — a per-request attempt cap with no memory across requests, which is
//! what let a secondary rate limit sustain itself for 45+ minutes. One helper
//! that charges every wait against a shared [`FetchBudget`] is what makes the
//! whole run bounded rather than each request individually bounded.
//! What: [`retry_send`] retries a request up to [`MAX_RETRIES`] times, honouring
//! `Retry-After` on rate-limited responses and using exponential backoff
//! (`RETRY_BASE_MS * 2^attempt`) on 5xx and transport errors. Every wait is
//! reserved from the budget first, so the run has a total ceiling; when the
//! ceiling is reached the budget latches and this helper stops issuing requests
//! at all. [`retry_get`] is the GET convenience wrapper.
//! Test: `retry_tests` in this directory drives both paths against `wiremock`.

use std::time::Duration;

use tracing::{debug, warn};

use crate::collect::errors::{CollectError, Result};
use crate::collect::github::budget::{rate_limit_delay, FetchBudget};

/// Maximum retry attempts for one request.
pub(crate) const MAX_RETRIES: u32 = 3;
/// Base delay (in milliseconds) for exponential backoff: 1s, 2s, 4s.
pub(crate) const RETRY_BASE_MS: u64 = 1000;

/// Exponential backoff delay for `attempt`, starting at [`RETRY_BASE_MS`].
fn backoff(attempt: u32) -> Duration {
    Duration::from_millis(RETRY_BASE_MS * (1u64 << attempt))
}

/// Send a GET with bounded, rate-limit-aware retries.
///
/// Why/What/Test: see [`retry_send`], which this delegates to.
///
/// # Errors
///
/// Whatever [`retry_send`] returns.
pub(crate) async fn retry_get(
    client: &reqwest::Client,
    url: &str,
    budget: &FetchBudget,
) -> Result<reqwest::Response> {
    debug!(url = %url, "GET (with retry)");
    retry_send(client.get(url), budget).await
}

/// Send `req` with bounded, rate-limit-aware retries against a shared budget.
///
/// Why: the caller must never be able to spin. Three bounds apply at once — a
/// per-request attempt cap, a per-wait clamp, and the run-wide sleep allowance
/// in `budget` — and the last is the one that stops a storm, because it is the
/// only bound that survives from one request to the next.
/// What: on a rate-limited response (see
/// [`rate_limit_delay`](crate::collect::github::budget::rate_limit_delay))
/// waits the server's own `Retry-After`; on 5xx or a transport error waits
/// [`backoff`]. Every wait is reserved from `budget` first, so exhausting the
/// allowance returns [`CollectError::Throttled`] instead of sleeping. Once the
/// budget has latched, this returns that error without sending anything —
/// which is what converts the remaining thousands of doomed calls into an
/// immediate terminal outcome. A non-transient response is returned as-is;
/// calling `.error_for_status()` on it stays the caller's job.
/// Test: `retry_tests::a_secondary_rate_limit_is_retried_then_terminates`,
/// `retry_tests::retry_after_is_honoured_instead_of_the_fixed_ladder`,
/// `retry_tests::a_latched_budget_sends_no_further_requests`,
/// `retry_tests::a_transient_5xx_still_recovers`.
///
/// # Errors
///
/// - [`CollectError::Throttled`] when GitHub is still rate-limiting after
///   [`MAX_RETRIES`], or when the run's sleep allowance is exhausted.
/// - [`CollectError::Http`] on a transport failure that outlived its retries.
pub(crate) async fn retry_send(
    req: reqwest::RequestBuilder,
    budget: &FetchBudget,
) -> Result<reqwest::Response> {
    if let Some(e) = budget.tripped_error() {
        return Err(e);
    }

    let mut last_err: Option<reqwest::Error> = None;
    for attempt in 0..=MAX_RETRIES {
        // A GET carries no body and always clones. A request that does not is
        // given its single attempt rather than being silently dropped.
        let Some(this_attempt) = req.try_clone() else {
            return req.send().await.map_err(CollectError::Http);
        };

        match this_attempt.send().await {
            Ok(resp) => {
                let status = resp.status().as_u16();

                if let Some(delay) = rate_limit_delay(status, resp.headers()) {
                    if attempt == MAX_RETRIES {
                        warn!(
                            status,
                            attempt,
                            "GitHub is still rate-limiting after the attempt cap; \
                                      stopping GitHub collection for this run"
                        );
                        return Err(budget.trip(status, Some(delay)));
                    }
                    let delay = budget.reserve(delay, status)?;
                    warn!(
                        status,
                        attempt,
                        delay_ms = delay.as_millis() as u64,
                        "GitHub rate-limited the request; waiting out Retry-After"
                    );
                    tokio::time::sleep(delay).await;
                    continue;
                }

                let transient = (500..=599).contains(&status);
                if !transient || attempt == MAX_RETRIES {
                    return Ok(resp);
                }
                let delay = budget.reserve(backoff(attempt), status)?;
                warn!(
                    status,
                    attempt,
                    delay_ms = delay.as_millis() as u64,
                    "GitHub returned a transient status; retrying"
                );
                tokio::time::sleep(delay).await;
            }
            Err(e) => {
                if attempt == MAX_RETRIES {
                    return Err(CollectError::Http(e));
                }
                let delay = budget.reserve(backoff(attempt), 0)?;
                warn!(error = %e, attempt, delay_ms = delay.as_millis() as u64,
                      "transport error; retrying");
                last_err = Some(e);
                tokio::time::sleep(delay).await;
            }
        }
    }

    // Unreachable in practice: the loop returns by `attempt == MAX_RETRIES`.
    Err(CollectError::Http(
        last_err.expect("retry loop preserved error"),
    ))
}

#[cfg(test)]
#[path = "retry_tests.rs"]
mod retry_tests;
