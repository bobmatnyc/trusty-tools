//! Request-path coverage for bounded GitHub retries (#6084).
//!
//! Every test drives a local `wiremock` server — no live GitHub call, and no
//! test waits on a real backoff longer than the first 1s rung.

use super::*;

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::collect::github::budget::MAX_RETRY_AFTER;

/// How many requests the server actually received.
async fn request_count(server: &MockServer) -> usize {
    server
        .received_requests()
        .await
        .map(|r| r.len())
        .unwrap_or_default()
}

/// Why: this is the spiral from #6084 in miniature. A server that never stops
/// rate-limiting must produce a bounded number of requests and one terminal
/// error, not an open-ended loop.
/// What: every response is `429 Retry-After: 0`, so the wait is real logic but
/// costs no wall-clock. Asserts exactly `MAX_RETRIES + 1` requests and a
/// `Throttled` error.
#[tokio::test]
async fn a_secondary_rate_limit_is_retried_then_terminates() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/issues/1"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
        .mount(&server)
        .await;

    let budget = FetchBudget::new();
    let url = format!("{}/issues/1", server.uri());
    let err = retry_get(&reqwest::Client::new(), &url, &budget)
        .await
        .expect_err("a permanently rate-limited endpoint must not return Ok");

    assert!(
        matches!(err, CollectError::Throttled { status: 429, .. }),
        "expected Throttled, got {err:?}"
    );
    assert_eq!(
        request_count(&server).await,
        (MAX_RETRIES + 1) as usize,
        "the attempt cap must bound how many requests one call makes"
    );
    assert!(
        budget.tripped_error().is_some(),
        "exhausting the attempt cap while still rate-limited must latch the breaker"
    );
}

/// Why: before #6084 the delay came from a fixed 1s/2s/4s ladder and
/// `Retry-After` was never read, so the client kept asking well before GitHub
/// would answer. Proving the header is honoured without sleeping for it: name
/// a delay larger than the run's whole allowance and assert the call stops on
/// the FIRST rate-limited response rather than retrying three more times.
#[tokio::test]
async fn retry_after_is_honoured_instead_of_the_fixed_ladder() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/issues/2"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "45"))
        .mount(&server)
        .await;

    // Smaller than the 45s the server asks for, so the very first wait cannot
    // be afforded.
    let budget = FetchBudget::with_sleep_budget(Duration::from_secs(5));
    let url = format!("{}/issues/2", server.uri());
    let err = retry_get(&reqwest::Client::new(), &url, &budget)
        .await
        .expect_err("a wait the budget cannot afford must terminate the call");

    match err {
        CollectError::Throttled {
            status,
            retry_after,
        } => {
            assert_eq!(status, 429);
            assert_eq!(
                retry_after,
                Some(Duration::from_secs(45)),
                "the error must carry the server's own delay, not the fixed ladder"
            );
        }
        other => panic!("expected Throttled, got {other:?}"),
    }
    assert_eq!(
        request_count(&server).await,
        1,
        "an unaffordable Retry-After must stop the call, not spend three more attempts on it"
    );
}

/// Why: the latch is the whole cure. On the live repro every one of 2625 pull
/// requests paid four rejected requests because nothing carried state between
/// calls; once the budget has tripped, no further request may leave.
#[tokio::test]
async fn a_latched_budget_sends_no_further_requests() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/issues/3"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .mount(&server)
        .await;

    let budget = FetchBudget::new();
    budget.trip(429, Some(Duration::from_secs(30)));

    let url = format!("{}/issues/3", server.uri());
    for _ in 0..25 {
        let err = retry_get(&reqwest::Client::new(), &url, &budget)
            .await
            .expect_err("a latched budget must refuse every call");
        assert!(matches!(err, CollectError::Throttled { .. }));
    }
    assert_eq!(
        request_count(&server).await,
        0,
        "not one request may leave the process after the breaker latches"
    );
}

/// Why: bounding the storm must not cost the ordinary recovery a retry exists
/// for — a single 502 still has to succeed on the next attempt.
#[tokio::test]
async fn a_transient_5xx_still_recovers() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/issues/4"))
        .respond_with(ResponseTemplate::new(502))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/issues/4"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .mount(&server)
        .await;

    let budget = FetchBudget::new();
    let url = format!("{}/issues/4", server.uri());
    let resp = retry_get(&reqwest::Client::new(), &url, &budget)
        .await
        .expect("a single transient 502 must still recover");
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(request_count(&server).await, 2);
}

/// Why: a token missing a scope answers 403 forever. Retrying it spends the
/// allowance a genuine rate limit needs, and the caller's `error_for_status`
/// is what should report it.
#[tokio::test]
async fn a_403_without_rate_limit_evidence_is_returned_immediately() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/issues/5"))
        .respond_with(
            ResponseTemplate::new(403)
                .insert_header("x-ratelimit-remaining", "4999")
                .set_body_string("Resource not accessible by personal access token"),
        )
        .mount(&server)
        .await;

    let budget = FetchBudget::new();
    let url = format!("{}/issues/5", server.uri());
    let resp = retry_get(&reqwest::Client::new(), &url, &budget)
        .await
        .expect("a scope failure is a response, not a retry");
    assert_eq!(resp.status().as_u16(), 403);
    assert_eq!(request_count(&server).await, 1);
    assert!(budget.tripped_error().is_none());
}

/// Why: a drained primary limit arrives as 403 with the quota at zero, which
/// the pre-fix classifier (status 429 only) did not recognise at all.
#[tokio::test]
async fn a_drained_primary_quota_terminates_rather_than_spinning() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/issues/6"))
        .respond_with(
            ResponseTemplate::new(403)
                .insert_header("x-ratelimit-remaining", "0")
                .insert_header("x-ratelimit-reset", "99999999999"),
        )
        .mount(&server)
        .await;

    // A reset far in the future clamps to MAX_RETRY_AFTER, which no run-wide
    // allowance this small can cover.
    let budget = FetchBudget::with_sleep_budget(MAX_RETRY_AFTER / 2);
    let url = format!("{}/issues/6", server.uri());
    let err = retry_get(&reqwest::Client::new(), &url, &budget)
        .await
        .expect_err("a drained quota must terminate the run's GitHub fetching");
    assert!(matches!(err, CollectError::Throttled { status: 403, .. }));
    assert_eq!(request_count(&server).await, 1);
}
