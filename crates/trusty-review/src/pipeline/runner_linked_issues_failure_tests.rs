//! `fetch_linked_issues` failure branches through the real pipeline (#9197,
//! B2b F2, F5, F6, F8, F9, F12, F13, F16).
//!
//! Why: every failure must leave the review completing with a row or item,
//! and the only way error text leaves the review is an item detail that
//! `ContextLedger::finish` caps and redacts (amendment 11), so these asserts
//! read `ReviewOutcome::context_sources`, never a unit stop before `finish`.
//! What: drives `run_review_with` through `runner_linked_issues_tests.rs`'s
//! `review` harness with failing fetchers, a failing PR source, a local
//! diff, a blank token and hostile PR bodies.
//! Test: this module.

use super::linked_issues::{ISSUE_BODY, LINKING_BODY, Seen, issue_12, on, review, review_body};
use super::optional_context_off::{FakePrSource, billing_diff, github_input, mapreduce_diff};
use super::*;
use crate::models::{ContextItemRecord, SourceState};
use crate::pipeline::optional_context::{
    IssueDoc,
    linked_issues::tests::{Answer, FakeFetcher},
    probes::MAX_DETAIL_CHARS,
};

/// A supplied doc that must render whatever the fetch step does.
fn supplied() -> Vec<IssueDoc> {
    vec![IssueDoc::new("#3", None, "SUPPLIED_3", None).expect("valid")]
}

/// The item `id` of the `issues` row.
fn item<'a>(seen: &'a Seen, id: &str) -> &'a ContextItemRecord {
    let found = seen.issues().items.iter().find(|i| i.id == id);
    found.unwrap_or_else(|| panic!("no {id} in {:?}", seen.issues().items))
}

/// The review ran to a verdict.
fn completed(seen: &Seen) {
    let result = &seen.outcome.result;
    assert!(result.error.is_none(), "{:?}", result.error);
    assert!(!seen.reviewer.requests().is_empty(), "the reviewer ran");
}

/// F6, AC12: a local diff records `unavailable`, makes no call and still
/// shows the supplied docs.
#[serial_test::serial]
#[tokio::test]
async fn local_diff_records_unavailable_and_keeps_supplied_docs() {
    let (diff_source, _tmp) = local_diff_source(&billing_diff());
    let input = ReviewInput {
        diff_source,
        ..github_input(CallerContext::default())
    };
    let source = FakePrSource::new(LINKING_BODY, &billing_diff());
    let seen = review(
        source,
        on().with_issue_docs(supplied()),
        Some(issue_12()),
        "",
        input,
    )
    .await;
    completed(&seen);
    assert!(seen.calls.is_empty());
    let row = seen.issues();
    assert_eq!(
        (row.state, row.detail.as_deref()),
        (SourceState::Unavailable, Some("local diff has no PR"))
    );
    assert!(seen.prompt().contains("SUPPLIED_3"));
}

/// F5, AC12: a failed PR metadata read records `unavailable` with its
/// error, makes no call and still shows the supplied docs.
#[serial_test::serial]
#[tokio::test]
async fn meta_failure_skips_fetch_and_keeps_supplied_docs() {
    let mut source = FakePrSource::new(LINKING_BODY, &billing_diff());
    source.fail_meta = true;
    let input = github_input(CallerContext::default());
    let seen = review(
        source,
        on().with_issue_docs(supplied()),
        Some(issue_12()),
        "",
        input,
    )
    .await;
    completed(&seen);
    assert!(seen.calls.is_empty());
    let row = seen.issues();
    assert_eq!(row.state, SourceState::Unavailable);
    let detail = row.detail.as_deref().unwrap_or_default();
    assert!(
        detail.starts_with("PR metadata fetch failed: ") && detail.contains("unreachable"),
        "{detail}"
    );
    assert!(seen.prompt().contains("SUPPLIED_3"));
}

/// F2, F13: the production fetcher with a blank token answers `NoToken`
/// (no `gh` fallback, no socket); the item and row are `unavailable`.
#[serial_test::serial]
#[tokio::test]
async fn a_blank_token_is_unavailable_and_the_review_completes() {
    let mut input = github_input(CallerContext::default());
    if let DiffSource::Github { token, .. } = &mut input.diff_source {
        *token = "   ".to_string();
    }
    let source = FakePrSource::new(LINKING_BODY, &billing_diff());
    let seen = review(source, on(), None, "", input).await;
    completed(&seen);
    let twelve = item(&seen, "#12");
    assert_eq!(twelve.state, SourceState::Unavailable);
    assert!(
        twelve
            .detail
            .as_deref()
            .unwrap_or("")
            .contains("no auth token"),
        "{twelve:?}"
    );
    assert_eq!(seen.issues().state, SourceState::Unavailable);
}

/// Assert `#12`'s detail is one capped line holding none of `secrets`.
fn assert_capped(seen: &Seen, secrets: &[&str]) {
    completed(seen);
    let detail = item(seen, "#12").detail.clone().unwrap_or_default();
    assert!(
        detail.chars().count() <= MAX_DETAIL_CHARS && !detail.contains('\n'),
        "{detail}"
    );
    let json = serde_json::to_string(&seen.outcome.context_sources).expect("serialize");
    for secret in secrets {
        assert!(!json.contains(secret), "{secret} leaked: {json}");
    }
    assert!(json.len() < 4_096, "{} bytes", json.len());
}

/// F8: an error that echoes a token and a 10 kB response body reaches the
/// ledger redacted, on one line, capped.
#[serial_test::serial]
#[tokio::test]
async fn fetch_error_detail_is_redacted_and_capped() {
    let token = concat!("gh", "p_0123456789abcdefABCDEF0123456789abcd");
    let text = format!(
        "github API failed: 500: Authorization: Bearer {token}\n{}",
        "z\n".repeat(5_000)
    );
    let f = FakeFetcher::default().answer(12, Answer::Fail(text));
    let seen = review_body(LINKING_BODY, on(), f, "").await;
    assert_capped(&seen, &[token]);
}

/// F16: a 200 answer with an HTML body (`parse json: <whole body>`) is
/// `unavailable`, redacted and capped.
#[serial_test::serial]
#[tokio::test]
async fn a_non_json_answer_is_unavailable_redacted_and_capped() {
    let html = format!(
        "<!DOCTYPE html><html>{}token=SECRET_9197_VALUE</html>",
        "<p>x</p>\n".repeat(1_200)
    );
    let f = FakeFetcher::default().answer(12, Answer::Fail(format!("parse json: {html}")));
    let seen = review_body(LINKING_BODY, on(), f, "").await;
    assert_eq!(item(&seen, "#12").state, SourceState::Unavailable);
    assert_capped(&seen, &["SECRET_9197_VALUE"]);
}

/// F9, AC18: 10,000 bare refs make 5 calls and a row of bounded size.
#[serial_test::serial]
#[tokio::test]
async fn ten_thousand_bare_refs_make_five_calls_and_a_bounded_row() {
    let refs: Vec<String> = (100..10_100).map(|n| format!("#{n}")).collect();
    let body = format!("Refs {}", refs.join(", "));
    let seen = review_body(&body, on(), FakeFetcher::default(), "").await;
    completed(&seen);
    assert_eq!(seen.calls, ["100", "101", "102", "103", "104"]);
    let row = seen.issues();
    assert_eq!(row.items.len(), 6, "{:?}", row.items);
    let over = &row.items[5];
    assert_eq!(over.id, "(over the 5-issue fetch limit)");
    assert_eq!(
        over.detail.as_deref(),
        Some("9995 more same-repository refs are not fetched")
    );
    assert!(serde_json::to_string(row).expect("serialize").len() < 2_048);
}

/// F9, AC18: 1,000 distinct long other-repository refs fold into one item
/// whose id carries none of their text, with no call.
#[serial_test::serial]
#[tokio::test]
async fn a_thousand_long_other_repo_refs_fold_into_one_item() {
    let long = "o".repeat(300);
    let refs: Vec<String> = (1..=1_000).map(|n| format!("{long}{n}/r{n}#{n}")).collect();
    let body = format!("Refs {}", refs.join(", "));
    let seen = review_body(&body, on(), FakeFetcher::default(), "").await;
    completed(&seen);
    assert!(seen.calls.is_empty());
    let row = seen.issues();
    let ids: Vec<&str> = row.items.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(ids, ["(other repositories)"]);
    assert_eq!(
        row.items[0].detail.as_deref(),
        Some("1000 refs to other repositories are not fetched")
    );
    let json = serde_json::to_string(row).expect("serialize");
    assert!(json.len() < 512 && !json.contains("ooo"), "{json}");
    assert_eq!(
        row.state,
        SourceState::Truncated,
        "omitted-only is not absent"
    );
}

/// F12: on the map-reduce path a failed fetch leaves the same row and the
/// review completes.
#[serial_test::serial]
#[tokio::test]
async fn mapreduce_review_completes_when_the_fetch_fails() {
    let source = FakePrSource::new(LINKING_BODY, &mapreduce_diff());
    let f = FakeFetcher::default().answer(12, Answer::Fail("github API failed: 503".into()));
    let seen = review(
        source,
        on(),
        Some(f),
        "",
        github_input(CallerContext::default()),
    )
    .await;
    completed(&seen);
    assert!(
        seen.outcome
            .result
            .review_body
            .contains("Map-reduce review:")
    );
    assert_eq!(item(&seen, "#12").state, SourceState::Unavailable);
    assert_eq!(seen.issues().state, SourceState::Unavailable);
    assert!(!seen.prompt().contains(ISSUE_BODY));
}
