//! Coverage for the bounded `fetch_on_reference` lookup path (#6084).
//!
//! This is the second of the two paths in the live repro: one Issues-API call
//! per unique `#N` in commit history — 3681 of them on the repository that
//! reproduced the spiral. Every test drives a local `wiremock` server.

use super::*;

use std::collections::HashMap as StdHashMap;

use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::collect::errors::CollectError;

/// A config whose `bug` label maps to a category, so a fetched issue produces
/// a signal and an unfetched one visibly does not.
fn config() -> GithubIssuesSourceConfig {
    let mut label_mappings = StdHashMap::new();
    label_mappings.insert("bug".to_string(), "bug_fix".to_string());
    GithubIssuesSourceConfig {
        repo: "acme/widgets".to_string(),
        // A name no test environment exports, so the lookup runs unauthenticated
        // and never depends on the developer's real GITHUB_TOKEN.
        token_env: "TGA_TEST_GITHUB_TOKEN_ABSENT".to_string(),
        label_mappings,
    }
}

/// `n` bare references, `#1` through `#n`.
fn refs(n: u64) -> Vec<GitHubRef> {
    (1..=n)
        .map(|number| GitHubRef { repo: None, number })
        .collect()
}

/// How many requests the server actually received.
async fn request_count(server: &MockServer) -> usize {
    server
        .received_requests()
        .await
        .map(|r| r.len())
        .unwrap_or_default()
}

/// Why: this is the exact fail-open the issue names. Before #6084 a 429 was
/// folded into `None`, the resolver cached that as "this issue carries no
/// classification signal", and the run kept issuing rejected requests for
/// every remaining reference. A throttle is an unknown answer and must not be
/// cached, and it must stop the batch.
/// What: the server always answers `429`, with a `Retry-After` larger than the
/// run's whole allowance so no test waits on it. Asserts the batch stops, says
/// why, and records NOTHING — not even a `None` — for the reference it could
/// not resolve.
#[tokio::test]
async fn a_rate_limited_batch_stops_and_caches_nothing() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "300"))
        .mount(&server)
        .await;

    let budget = FetchBudget::with_sleep_budget(std::time::Duration::from_secs(1));
    let batch = fetch_issues_batch(
        &reqwest::Client::new(),
        &config(),
        &refs(50),
        Some(&server.uri()),
        &budget,
    )
    .await;

    assert!(
        batch.signals.is_empty(),
        "a throttled lookup is an unknown answer; caching it as `no signal` is the bug"
    );
    let reason = batch
        .stopped_early
        .expect("a batch cut short by a rate limit must say so");
    assert!(
        reason.contains("UNCLASSIFIED"),
        "the stop must name what is missing, got: {reason}"
    );
    assert_eq!(
        request_count(&server).await,
        1,
        "50 references must not cost 50 rejected lookups once the budget is spent"
    );
}

/// Why: the reference count scales with commit history, not with anything the
/// operator chose, so the batch needs a ceiling of its own even when GitHub is
/// answering normally.
#[tokio::test]
async fn the_batch_stops_at_the_reference_lookup_cap() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let budget = FetchBudget::new();
    let over_cap = (MAX_REFERENCE_LOOKUPS + 25) as u64;
    let batch = fetch_issues_batch(
        &reqwest::Client::new(),
        &config(),
        &refs(over_cap),
        Some(&server.uri()),
        &budget,
    )
    .await;

    assert_eq!(batch.signals.len(), MAX_REFERENCE_LOOKUPS);
    assert_eq!(request_count(&server).await, MAX_REFERENCE_LOOKUPS);
    let reason = batch
        .stopped_early
        .expect("hitting the lookup cap must be reported, never silent");
    assert!(reason.contains("UNCLASSIFIED"), "got: {reason}");
    assert_eq!(
        budget.notices().len(),
        1,
        "the cap must also reach the run's truncation ledger"
    );
}

/// Why: bounding the path must not cost the ordinary case. A label that maps
/// to a category still has to classify.
#[tokio::test]
async fn a_labelled_issue_still_classifies() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(r#"{"number":7,"labels":[{"name":"bug"}]}"#),
        )
        .mount(&server)
        .await;

    let budget = FetchBudget::new();
    let batch = fetch_issues_batch(
        &reqwest::Client::new(),
        &config(),
        &refs(1),
        Some(&server.uri()),
        &budget,
    )
    .await;

    assert!(batch.stopped_early.is_none());
    let signal = batch
        .signals
        .get("acme/widgets#1")
        .expect("the reference was looked up")
        .as_ref()
        .expect("a `bug` label maps to a category");
    assert_eq!(signal.category, "bug_fix");
}

/// Why: a genuinely missing issue is a real, cacheable answer — folding it in
/// with a throttle would make the run re-ask for it forever.
#[tokio::test]
async fn a_missing_issue_is_a_cacheable_no_signal() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let budget = FetchBudget::new();
    let issue = fetch_issue(
        &reqwest::Client::new(),
        &config(),
        "acme/widgets",
        404,
        Some(&server.uri()),
        &budget,
    )
    .await
    .expect("a 404 is an answer, not a failure");
    assert!(issue.is_none());
    assert!(budget.tripped_error().is_none());
}

/// Why: the error arm is what the resolver keys its don't-cache decision on,
/// so it has to be a `Throttled` rather than a generic transport error.
#[tokio::test]
async fn a_rate_limited_single_fetch_returns_throttled() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
        .mount(&server)
        .await;

    let budget = FetchBudget::new();
    let err = fetch_issue(
        &reqwest::Client::new(),
        &config(),
        "acme/widgets",
        1,
        Some(&server.uri()),
        &budget,
    )
    .await
    .expect_err("a persistent rate limit must not read as `no signal`");
    assert!(
        matches!(err, CollectError::Throttled { status: 429, .. }),
        "got {err:?}"
    );
}
