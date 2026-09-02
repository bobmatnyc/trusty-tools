//! Unit coverage for the run-wide GitHub fetch budget (#6084).
//!
//! Every test here is pure — no HTTP, no sleeping. The wiring that puts these
//! decisions on the request path is covered in `retry_tests.rs`.

use super::*;

use reqwest::header::{HeaderMap, HeaderValue};

/// Build a header map from `(name, value)` pairs.
fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
    let mut h = HeaderMap::new();
    for (k, v) in pairs {
        let name: reqwest::header::HeaderName =
            k.parse().expect("test header name is a valid header name");
        h.insert(
            name,
            HeaderValue::from_str(v).expect("test header value is valid"),
        );
    }
    h
}

/// Why: `Retry-After` is the server's own instruction and it must win over the
/// fixed ladder that produced the spiral.
#[test]
fn retry_after_is_preferred_over_every_other_hint() {
    let h = headers(&[
        ("retry-after", "7"),
        ("x-ratelimit-remaining", "0"),
        ("x-ratelimit-reset", "99999999999"),
    ]);
    assert_eq!(rate_limit_delay(429, &h), Some(Duration::from_secs(7)));
}

/// Why: a 403 from a token missing a scope will never succeed, and retrying it
/// spends the budget that a genuine rate limit needs.
#[test]
fn a_403_without_rate_limit_evidence_is_not_a_rate_limit() {
    let h = headers(&[("x-ratelimit-remaining", "4999")]);
    assert_eq!(rate_limit_delay(403, &h), None);
}

/// Why: a drained primary limit arrives as 403 with the quota at zero and no
/// `Retry-After` at all; missing it is how the storm starts.
#[test]
fn a_drained_quota_makes_a_403_a_rate_limit() {
    let h = headers(&[("x-ratelimit-remaining", "0")]);
    assert!(rate_limit_delay(403, &h).is_some());
}

/// Why: with no `Retry-After`, the reset epoch is the only delay GitHub gives.
#[test]
fn a_reset_epoch_is_used_when_retry_after_is_absent() {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the epoch")
        .as_secs();
    let h = headers(&[
        ("x-ratelimit-remaining", "0"),
        ("x-ratelimit-reset", &(now + 5).to_string()),
    ]);
    let delay = rate_limit_delay(403, &h).expect("a drained quota is a rate limit");
    // The clock advances between building the header and reading it.
    assert!(
        delay <= Duration::from_secs(5) && delay >= Duration::from_secs(3),
        "expected ~5s, got {delay:?}"
    );
}

/// Why: GitHub can name an hour-long reset. Sleeping that long inside a
/// collection run is indistinguishable from a hang.
#[test]
fn a_very_long_retry_after_is_clamped() {
    let h = headers(&[("retry-after", "3600")]);
    assert_eq!(rate_limit_delay(429, &h), Some(MAX_RETRY_AFTER));
}

/// Why: a rate-limited response with no usable hint still needs a delay, not a
/// tight loop.
#[test]
fn a_429_with_no_hints_falls_back_to_the_default_delay() {
    assert_eq!(
        rate_limit_delay(429, &HeaderMap::new()),
        Some(DEFAULT_RATE_LIMIT_DELAY)
    );
}

/// Why: a success must never be classified as throttled.
#[test]
fn a_success_is_never_a_rate_limit() {
    let h = headers(&[("retry-after", "5")]);
    assert_eq!(rate_limit_delay(200, &h), None);
}

/// Why: this is the bound the whole fix rests on — the run stops paying for
/// waits once the allowance is gone.
#[test]
fn the_budget_latches_after_the_allowance_is_spent() {
    let budget = FetchBudget::with_sleep_budget(Duration::from_secs(3));
    assert!(budget.reserve(Duration::from_secs(2), 429).is_ok());
    assert!(budget.tripped_error().is_none());

    let refused = budget.reserve(Duration::from_secs(2), 429);
    assert!(
        matches!(refused, Err(CollectError::Throttled { status: 429, .. })),
        "expected Throttled, got {refused:?}"
    );
    assert!(budget.tripped_error().is_some());
}

/// Why: the latch is what makes every remaining call fail without a request.
/// A budget that recovered would restart the storm.
#[test]
fn a_latched_budget_refuses_every_later_reservation() {
    let budget = FetchBudget::with_sleep_budget(Duration::from_millis(1));
    assert!(budget.reserve(Duration::from_secs(1), 429).is_err());
    for _ in 0..5 {
        assert!(budget.reserve(Duration::from_millis(0), 429).is_err());
    }
}

/// Why: a request that used all its attempts and is still rate-limited proves
/// the limit outlives one request, so nothing after it should try.
#[test]
fn tripping_on_the_attempt_cap_latches_the_breaker() {
    let budget = FetchBudget::new();
    let err = budget.trip(429, Some(Duration::from_secs(2)));
    assert!(matches!(err, CollectError::Throttled { status: 429, .. }));
    assert!(budget.tripped_error().is_some());
}

/// Why: a cap that trims results silently is the failure this ledger exists to
/// prevent.
#[test]
fn truncation_notices_are_recorded_in_order() {
    let budget = FetchBudget::new();
    assert!(budget.notices().is_empty());
    budget.note_truncation("first");
    budget.note_truncation("second");
    assert_eq!(budget.notices(), vec!["first", "second"]);
}

/// Why: the shipped constants are the operator-visible contract; a silent
/// change to any of them changes what a run will and will not do.
#[test]
fn the_shipped_bounds_are_the_documented_ones() {
    assert_eq!(MAX_PAGES, 100);
    assert_eq!(MAX_REFERENCE_LOOKUPS, 500);
    assert_eq!(RATE_LIMIT_SLEEP_BUDGET, Duration::from_secs(120));
    assert_eq!(MAX_RETRY_AFTER, Duration::from_secs(60));
}

/// Why (#6565): the allowance now bounds a whole run, so a long multi-org sweep
/// that legitimately needs more than 120 s of total waiting needs a way to say
/// so without a rebuild.
/// What: a positive integer of seconds wins over the constant.
/// Test: itself.
#[test]
fn run_sleep_budget_honours_a_valid_override() {
    assert_eq!(run_sleep_budget_from(Some("300")), Duration::from_secs(300));
    assert_eq!(run_sleep_budget_from(Some(" 45 ")), Duration::from_secs(45));
}

/// Why: a zero or unparseable value must never shorten the allowance to nothing
/// — the breaker would latch on the first rate-limited response and the run
/// would report itself throttled before it had waited at all.
/// What: unset, empty, `0`, and junk all fall back to the constant.
/// Test: itself.
#[test]
fn run_sleep_budget_ignores_junk_overrides() {
    for value in [None, Some(""), Some("0"), Some("-5"), Some("soon")] {
        assert_eq!(
            run_sleep_budget_from(value),
            RATE_LIMIT_SLEEP_BUDGET,
            "{value:?} must fall back to the shipped allowance"
        );
    }
}

/// Why (#6565): sharing is what makes the ceiling per-run; cloning a handle must
/// share the underlying allowance and constructing a new one must not.
/// What: two clones of one `RunBudget` draw down together; two separately
/// constructed ones do not.
/// Test: itself.
#[test]
fn a_cloned_run_budget_shares_its_allowance() {
    let run = RunBudget::with_sleep_budget(Duration::from_secs(3));
    let clone = run.clone();

    run.shared()
        .reserve(Duration::from_secs(2), 429)
        .expect("2s fits");
    assert!(
        clone.shared().reserve(Duration::from_secs(2), 429).is_err(),
        "a clone shares the allowance, so only 1s remains"
    );

    let separate = RunBudget::with_sleep_budget(Duration::from_secs(3));
    separate
        .shared()
        .reserve(Duration::from_secs(3), 429)
        .expect("a separate run budget starts full");
}
