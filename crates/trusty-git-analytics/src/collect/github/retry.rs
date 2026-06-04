//! Shared HTTP retry helper for all GitHub API calls.
//!
//! Why: the main PR client (`GitHubClient`) and the org-discovery path
//! both need exponential backoff on transient failures (5xx, 429). A single
//! free function here avoids duplicating the logic and makes the retry
//! policy easy to change in one place.
//! What: [`retry_get`] retries a GET request up to [`MAX_RETRIES`] times
//! with exponential backoff delays (`RETRY_BASE_MS * 2^attempt`).
//! Test: covered indirectly by all callers; `GitHubClient::retry_request`
//! delegates here; wiremock integration tests exercise it end-to-end.

use std::time::Duration;

use tracing::{debug, warn};

use crate::collect::errors::{CollectError, Result};

/// Maximum retry attempts for transient failures (5xx, 429).
pub(crate) const MAX_RETRIES: u32 = 3;
/// Base delay (in milliseconds) for exponential backoff: 1s, 2s, 4s.
pub(crate) const RETRY_BASE_MS: u64 = 1000;

/// Send a GET with exponential backoff on transient HTTP failures.
///
/// Why: GitHub occasionally returns 502/504 under load and 429 when the
/// per-token rate limit drains. Both the main PR client and the org-discovery
/// path need this behaviour; sharing one free function avoids duplicating the
/// backoff logic. `GitHubClient::retry_request` delegates here so all retry
/// behaviour lives in a single place.
/// What: retries up to [`MAX_RETRIES`] times on HTTP 429 or any 5xx; delays
/// follow `RETRY_BASE_MS * 2^attempt` (1s, 2s, 4s). Returns the final
/// non-transient response; caller is responsible for calling
/// `.error_for_status()` on it.
/// Test: covered indirectly by all callers and by `wiremock` integration tests.
pub(crate) async fn retry_get(client: &reqwest::Client, url: &str) -> Result<reqwest::Response> {
    let mut last_err: Option<reqwest::Error> = None;
    for attempt in 0..=MAX_RETRIES {
        debug!(url = %url, attempt, "GET (with retry)");
        match client.get(url).send().await {
            Ok(resp) => {
                let status = resp.status();
                let transient = status.as_u16() == 429 || (500..=599).contains(&status.as_u16());
                if !transient || attempt == MAX_RETRIES {
                    return Ok(resp);
                }
                let delay = RETRY_BASE_MS * (1u64 << attempt);
                warn!(
                    status = %status,
                    attempt,
                    delay_ms = delay,
                    "GitHub returned transient status; retrying"
                );
                tokio::time::sleep(Duration::from_millis(delay)).await;
            }
            Err(e) => {
                if attempt == MAX_RETRIES {
                    return Err(CollectError::Http(e));
                }
                let delay = RETRY_BASE_MS * (1u64 << attempt);
                warn!(error = %e, attempt, delay_ms = delay, "transport error; retrying");
                last_err = Some(e);
                tokio::time::sleep(Duration::from_millis(delay)).await;
            }
        }
    }
    // Unreachable in practice: the loop returns by `attempt == MAX_RETRIES`.
    Err(CollectError::Http(
        last_err.expect("retry loop preserved error"),
    ))
}
