//! The one bound on how much GitHub fetching a single `tga` run may do (#6084).
//!
//! Why: every GitHub call in this crate retried 429 on a fixed 1s/2s/4s ladder
//! and then moved on to the next call, which retried the same way. Nothing
//! carried state ACROSS calls, so a secondary rate limit made every remaining
//! item pay four more rejected requests: a self-audit of this repo sat in a
//! continuous 429 loop for 45+ minutes across 2625 pull requests, and the
//! per-`#N` reference path reproduced it across 3681 references. A per-request
//! attempt cap cannot stop that — only a budget the whole run shares can.
//! What: [`FetchBudget`] is that shared state. It holds a total sleep
//! allowance, latches a breaker the first time the allowance is exhausted so
//! every later call fails fast instead of issuing another rejected request,
//! and carries the ledger of bounds the run actually hit so a truncated result
//! is reported as truncated rather than presented as complete.
//! [`rate_limit_delay`] is the single place a GitHub response is classified as
//! rate-limited and its `Retry-After` turned into a bounded wait.
//! Test: `budget_tests` in this directory.

use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::header::{HeaderMap, RETRY_AFTER};

use crate::collect::errors::CollectError;

/// Hard ceiling on pages any one paginated GitHub listing will walk.
///
/// At [`super::client::PAGE_SIZE`] (100) per page this is 10,000 items — far
/// past any repository this tool profiles, and finite even when GitHub keeps
/// answering with full pages.
pub(crate) const MAX_PAGES: u32 = 100;

/// Hard ceiling on per-reference issue lookups in one classification batch.
///
/// The `fetch_on_reference` path issues one Issues-API call per unique `#N`
/// found in commit messages, and that count scales with repository history
/// rather than with anything the operator chose.
pub(crate) const MAX_REFERENCE_LOOKUPS: usize = 500;

/// Total wall-clock one run may spend asleep waiting out GitHub.
pub(crate) const RATE_LIMIT_SLEEP_BUDGET: Duration = Duration::from_secs(120);

/// Longest single wait honoured, however large `Retry-After` is.
///
/// GitHub answers a drained primary limit with a reset up to an hour out.
/// Sleeping that long inside a collection run is indistinguishable from a
/// hang, so the budget stops the run instead.
pub(crate) const MAX_RETRY_AFTER: Duration = Duration::from_secs(60);

/// Wait used when a rate-limited response names no delay of its own.
pub(crate) const DEFAULT_RATE_LIMIT_DELAY: Duration = Duration::from_secs(1);

/// Classify a GitHub response and, when it is rate-limited, say how long to wait.
///
/// Why: GitHub signals its two rate limits differently and neither is just
/// "429". A secondary limit arrives as 403 or 429 carrying `Retry-After`; a
/// drained primary limit arrives as 403 with `x-ratelimit-remaining: 0` and an
/// `x-ratelimit-reset` epoch. A plain 403 — a token without the scope — is
/// neither, and retrying it is pure waste, so the classification has to be
/// narrower than "the status was 403".
/// What: returns `Some(delay)` only for those two shapes, preferring
/// `Retry-After`, then `x-ratelimit-reset`, then [`DEFAULT_RATE_LIMIT_DELAY`],
/// clamped to [`MAX_RETRY_AFTER`]. Returns `None` for everything else,
/// including a 403 with no rate-limit evidence.
/// Test: `budget_tests::retry_after_is_preferred_over_every_other_hint`,
/// `budget_tests::a_403_without_rate_limit_evidence_is_not_a_rate_limit`,
/// `budget_tests::a_reset_epoch_is_used_when_retry_after_is_absent`,
/// `budget_tests::a_very_long_retry_after_is_clamped`.
pub(crate) fn rate_limit_delay(status: u16, headers: &HeaderMap) -> Option<Duration> {
    let retry_after = header_u64(headers, RETRY_AFTER.as_str()).map(Duration::from_secs);
    let quota_drained = header_u64(headers, "x-ratelimit-remaining") == Some(0);

    let rate_limited = status == 429 || (status == 403 && (retry_after.is_some() || quota_drained));
    if !rate_limited {
        return None;
    }

    let delay = retry_after
        .or_else(|| reset_delay(headers))
        .unwrap_or(DEFAULT_RATE_LIMIT_DELAY);
    Some(delay.min(MAX_RETRY_AFTER))
}

/// Read a header as an unsigned integer, or `None` when absent or unparseable.
fn header_u64(headers: &HeaderMap, name: &str) -> Option<u64> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
}

/// Turn an `x-ratelimit-reset` epoch into a delay from now.
///
/// A reset already in the past yields `Duration::ZERO` rather than `None`, so a
/// clock skew shortens the wait instead of falling through to the default.
fn reset_delay(headers: &HeaderMap) -> Option<Duration> {
    let reset = header_u64(headers, "x-ratelimit-reset")?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    Some(Duration::from_secs(reset.saturating_sub(now)))
}

/// Mutable half of a [`FetchBudget`], guarded by one lock.
#[derive(Default)]
struct BudgetState {
    /// Rate-limit and backoff sleep already committed by this run.
    slept: Duration,
    /// Set once the breaker latches: the status and delay that exhausted it.
    tripped: Option<(u16, Option<Duration>)>,
    /// Bounds this run hit, in operator-facing wording.
    notices: Vec<String>,
}

/// The run-wide bound every GitHub call in this crate charges against.
///
/// Why: see the module header — a spiral is what happens when each request
/// bounds only itself. Sharing one budget is what converts "45 minutes of
/// rejected requests" into a terminal outcome the caller can report.
/// What: a sleep allowance plus a latching breaker plus a truncation ledger.
/// [`Self::reserve`] is the only way to spend the allowance; once it refuses,
/// [`Self::tripped_error`] answers for every later call without a request
/// leaving the process.
/// Test: `budget_tests::*`.
pub struct FetchBudget {
    /// Total sleep this budget will authorise across its whole lifetime.
    sleep_budget: Duration,
    state: Mutex<BudgetState>,
}

impl FetchBudget {
    /// A budget with the standard [`RATE_LIMIT_SLEEP_BUDGET`] allowance.
    pub fn new() -> Self {
        Self::with_sleep_budget(RATE_LIMIT_SLEEP_BUDGET)
    }

    /// A budget with a caller-chosen allowance, so tests can exhaust it without
    /// sleeping for two minutes.
    pub fn with_sleep_budget(sleep_budget: Duration) -> Self {
        Self {
            sleep_budget,
            state: Mutex::new(BudgetState::default()),
        }
    }

    /// Take the lock, treating a poisoned mutex as usable.
    ///
    /// A panic while holding this lock leaves counters, not invariants, so
    /// refusing to fetch afterwards would be a worse outcome than continuing.
    fn lock(&self) -> std::sync::MutexGuard<'_, BudgetState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Commit `delay` of waiting against the allowance.
    ///
    /// Why: this is the single choke point that turns an unbounded retry storm
    /// into a terminal error. A caller that sleeps without reserving first has
    /// reintroduced the bug.
    /// What: returns the delay to sleep when it fits. When it does not — or
    /// when the breaker already latched — latches the breaker and returns
    /// [`CollectError::Throttled`], which the caller propagates.
    /// Test: `budget_tests::the_budget_latches_after_the_allowance_is_spent`,
    /// `budget_tests::a_latched_budget_refuses_every_later_reservation`.
    ///
    /// # Errors
    ///
    /// [`CollectError::Throttled`] once the allowance cannot cover `delay`.
    pub(crate) fn reserve(&self, delay: Duration, status: u16) -> Result<Duration, CollectError> {
        let mut state = self.lock();
        if let Some((status, retry_after)) = state.tripped {
            return Err(CollectError::Throttled {
                status,
                retry_after,
            });
        }
        if state.slept + delay > self.sleep_budget {
            state.tripped = Some((status, Some(delay)));
            return Err(CollectError::Throttled {
                status,
                retry_after: Some(delay),
            });
        }
        state.slept += delay;
        Ok(delay)
    }

    /// Latch the breaker because a request exhausted its own attempt cap while
    /// still rate-limited, and return the error to propagate.
    ///
    /// Test: `budget_tests::tripping_on_the_attempt_cap_latches_the_breaker`.
    pub(crate) fn trip(&self, status: u16, retry_after: Option<Duration>) -> CollectError {
        let mut state = self.lock();
        if state.tripped.is_none() {
            state.tripped = Some((status, retry_after));
        }
        CollectError::Throttled {
            status,
            retry_after,
        }
    }

    /// The error every call should fail with once the breaker has latched, or
    /// `None` while the run may still fetch.
    pub(crate) fn tripped_error(&self) -> Option<CollectError> {
        self.lock()
            .tripped
            .map(|(status, retry_after)| CollectError::Throttled {
                status,
                retry_after,
            })
    }

    /// Record that a bound trimmed the result set.
    ///
    /// Why: a cap that silently shortens a listing produces data that reads
    /// exactly like a complete collection. Every caller that breaks out of a
    /// walk early records why here so the run can say so.
    /// Test: `budget_tests::truncation_notices_are_recorded_in_order`.
    pub(crate) fn note_truncation(&self, message: impl Into<String>) {
        self.lock().notices.push(message.into());
    }

    /// Every truncation recorded so far, in the order it happened.
    pub(crate) fn notices(&self) -> Vec<String> {
        self.lock().notices.clone()
    }
}

impl Default for FetchBudget {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[path = "budget_tests.rs"]
mod budget_tests;
