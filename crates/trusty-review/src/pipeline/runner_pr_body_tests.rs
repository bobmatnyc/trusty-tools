//! `include_pr_body` and the source ledger, through the real pipeline (#9192).
//!
//! Why: the fetched PR body never reached a reviewer prompt; with the flag on
//! it must reach the reviewer and the verifier, capped on its own, ahead of
//! the caller's text, and the refs corpus must use the capped text. The
//! ledger must report what happened, and only when a new input was asked for.
//! What: reuses the fixture run from `runner_optional_context_off_tests.rs`
//! with the request on, and inspects the recorded prompts, the result and
//! `ReviewOutcome::context_sources`.
//! Test: this module.

use super::optional_context_off::{
    BODY, FakePrSource, Observed, billing_diff, legacy_caller, mapreduce_diff, observe,
};
use super::*;
use crate::models::{ContextSourceRecord, SourceState};
use crate::pipeline::optional_context::OptionalContextRequest;

/// A sentence only the PR body carries.
const BODY_SENTINEL: &str = "BODY_SENTINEL_9192 the session token rotates on login.";

fn with_pr_body() -> ReviewOptions {
    ReviewOptions::new(OptionalContextRequest::default().with_pr_body(true))
}

async fn observe_on(body: &str, caller: CallerContext) -> Observed {
    observe(
        FakePrSource::new(body, &billing_diff()),
        caller,
        with_pr_body(),
    )
    .await
}

fn reviewer_user(seen: &Observed) -> String {
    seen.reviewer.requests().remove(0).1
}

fn verifier_user(seen: &Observed) -> String {
    seen.verifier
        .requests()
        .first()
        .map(|(_, user)| user.clone())
        .unwrap_or_default()
}

fn row<'a>(seen: &'a Observed, source: &str) -> &'a ContextSourceRecord {
    seen.outcome
        .context_sources
        .iter()
        .find(|r| r.source == source)
        .unwrap_or_else(|| panic!("no `{source}` row in {:?}", seen.outcome.context_sources))
}

/// A body of `len` chars: `#42` first, `sentence` at char `at`, filler elsewhere.
fn long_body(len: usize, at: usize, sentence: &str) -> String {
    let mut body = String::from("Follows up #42.");
    body.push_str(&"x".repeat(at - body.len()));
    body.push_str(sentence);
    body.push_str(&"y".repeat(len - body.len()));
    body
}

/// #9192: the request is off by default and on only with the PR body asked for.
#[test]
fn requested_new_is_off_by_default_and_on_with_pr_body() {
    assert!(!OptionalContextRequest::default().requested_new());
    assert!(!ReviewOptions::default().request.requested_new());
    assert!(
        OptionalContextRequest::default()
            .with_pr_body(true)
            .requested_new()
    );
}

/// #9192: the body reaches the reviewer's PR description and the verifier's
/// author rationale.
#[tokio::test]
async fn include_pr_body_reaches_reviewer_and_verifier_prompts() {
    let body = format!("{BODY}\n\n{BODY_SENTINEL}");
    let seen = observe_on(&body, CallerContext::default()).await;
    let reviewer = reviewer_user(&seen);
    let section = reviewer
        .split("## PR Description")
        .nth(1)
        .unwrap_or_default();
    assert!(
        section.contains(BODY_SENTINEL),
        "reviewer prompt:\n{reviewer}"
    );
    assert!(
        verifier_user(&seen).contains(BODY_SENTINEL),
        "verifier lacks the body"
    );
}

/// #9192: caller text follows the body under its own heading; neither is lost.
#[tokio::test]
async fn caller_text_follows_the_fetched_body_not_over_it() {
    let seen = observe_on(BODY_SENTINEL, legacy_caller()).await;
    let reviewer = reviewer_user(&seen);
    let body_at = reviewer.find(BODY_SENTINEL).expect("body in prompt");
    let heading_at = reviewer
        .find("### Additional description from caller")
        .expect("caller heading in prompt");
    let caller_at = reviewer
        .find("Caller note: totals are summed in cents.")
        .expect("caller text in prompt");
    assert!(body_at < heading_at && heading_at < caller_at, "{reviewer}");
}

/// #9192: a 70,000-char body and a 70,000-char caller description are each cut
/// at 64,000 with their own marker, and the ledger records both cuts.
#[tokio::test]
async fn body_and_caller_field_are_capped_separately() {
    let caller = CallerContext {
        pr_description: Some("c".repeat(70_000)),
        ..CallerContext::default()
    };
    let seen = observe_on(&"b".repeat(70_000), caller).await;
    let reviewer = reviewer_user(&seen);
    assert!(
        reviewer.contains(
            "[... truncated: 6000 more characters omitted; the PR body is capped at 64000"
        )
    );
    assert!(reviewer.contains(
        "[... truncated: 6000 more characters omitted; caller context is capped at 64000"
    ));
    assert!(reviewer.contains(&"b".repeat(64_000)) && !reviewer.contains(&"b".repeat(64_001)));
    assert!(reviewer.contains(&"c".repeat(64_000)) && !reviewer.contains(&"c".repeat(64_001)));
    let body = row(&seen, "pr_body");
    assert_eq!(
        (body.state, body.chars, body.chars_omitted),
        (SourceState::Truncated, 64_000, 6_000)
    );
    let caller = row(&seen, "caller_context");
    assert_eq!(caller.state, SourceState::Truncated);
    assert_eq!(caller.items[0].chars_omitted, 6_000);
}

/// #9192: with the flag on, a citation of the body's cut-off tail is withheld,
/// because the corpus holds the capped body, not the raw one.
#[tokio::test]
async fn refs_use_the_capped_body_when_on() {
    let body = long_body(70_000, 66_000, "overflow fixed by checked_add in billing.");
    let off = observe(
        FakePrSource::new(&body, &billing_diff()),
        CallerContext::default(),
        ReviewOptions::default(),
    )
    .await;
    assert_eq!(
        off.outcome.result.findings.len(),
        1,
        "off: the raw body resolves"
    );
    let on = observe_on(&body, CallerContext::default()).await;
    assert!(
        on.outcome.result.findings.is_empty(),
        "on: a quote from the cut tail must be withheld: {:?}",
        on.outcome.result.findings
    );
    assert_eq!(on.outcome.result.withheld_findings.len(), 1);
}

/// #9192: with the flag on, a citation of text inside the cap still resolves.
#[tokio::test]
async fn refs_resolve_text_inside_the_cap_when_on() {
    let seen = observe_on(BODY, CallerContext::default()).await;
    assert_eq!(
        seen.outcome.result.findings.len(),
        1,
        "{:?}",
        seen.outcome.result.withheld_findings
    );
}

/// #9192: on the map-reduce path, every per-file prompt carries the body.
#[tokio::test]
async fn include_pr_body_reaches_every_chunk_prompt() {
    let body = format!("{BODY}\n\n{BODY_SENTINEL}");
    let source = FakePrSource::new(&body, &mapreduce_diff());
    let seen = observe(source, CallerContext::default(), with_pr_body()).await;
    let maps: Vec<String> = seen
        .reviewer
        .requests()
        .into_iter()
        .map(|(_, user)| user)
        .filter(|u| u.contains("## Unified diff"))
        .collect();
    assert!(maps.len() > 1, "the fixture must fan out per file");
    for prompt in maps {
        assert!(
            prompt.contains(BODY_SENTINEL),
            "a chunk prompt lacks the body"
        );
    }
}

/// #9192: `pr_body` reads used, truncated, absent and unavailable, in turn.
#[tokio::test]
async fn ledger_records_pr_body_used_truncated_absent_unavailable() {
    let used = observe_on(BODY, CallerContext::default()).await;
    let r = row(&used, "pr_body");
    assert_eq!(
        (r.state, r.chars),
        (SourceState::Used, BODY.chars().count())
    );

    let cut = observe_on(&"z".repeat(64_001), CallerContext::default()).await;
    let r = row(&cut, "pr_body");
    assert_eq!(
        (r.state, r.chars, r.chars_omitted),
        (SourceState::Truncated, 64_000, 1)
    );

    let empty = observe_on("  \n", CallerContext::default()).await;
    assert_eq!(row(&empty, "pr_body").state, SourceState::Absent);

    let mut failing = FakePrSource::new(BODY, &billing_diff());
    failing.fail_meta = true;
    let down = observe(failing, CallerContext::default(), with_pr_body()).await;
    let r = row(&down, "pr_body");
    assert_eq!(r.state, SourceState::Unavailable);
    let detail = r.detail.as_deref().unwrap_or_default();
    assert!(detail.contains("fixture: metadata unreachable"), "{detail}");
}

/// #9192: a local diff has no PR body; the ledger says so and the review runs.
#[tokio::test]
async fn ledger_marks_a_local_diff_unavailable() {
    let (source, _tmp) = local_diff_source(&billing_diff());
    let input = ReviewInput {
        diff_source: source,
        ..super::optional_context_off::github_input(legacy_caller())
    };
    let deps = ready_deps(Arc::new(FakeLlm::approves()), None);
    let outcome = run_review_with(&default_config(), input, deps, with_pr_body()).await;
    assert!(outcome.result.error.is_none(), "{:?}", outcome.result.error);
    let row = &outcome.context_sources[0];
    assert_eq!(
        (row.source.as_str(), row.state),
        ("pr_body", SourceState::Unavailable)
    );
    assert_eq!(row.detail.as_deref(), Some("local diff has no PR"));
}

/// #9192: a review that asked for no new input builds no ledger.
#[tokio::test]
async fn context_sources_absent_unless_requested() {
    let seen = observe(
        FakePrSource::new(BODY, &billing_diff()),
        legacy_caller(),
        ReviewOptions::default(),
    )
    .await;
    assert!(seen.outcome.context_sources.is_empty());
}

/// #9192: asking for a new input turns the ledger on, listing the caller
/// fields as items.
#[tokio::test]
async fn a_new_input_turns_the_ledger_on() {
    let seen = observe_on(BODY, legacy_caller()).await;
    let sources: Vec<&str> = seen
        .outcome
        .context_sources
        .iter()
        .map(|r| r.source.as_str())
        .collect();
    assert_eq!(sources, ["pr_body", "caller_context"]);
    let caller = row(&seen, "caller_context");
    assert_eq!(caller.state, SourceState::Used);
    let ids: Vec<&str> = caller.items.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(ids, ["pr_description", "pr_discussion", "referenced_code"]);
}

/// #9192: a `# Context:` preamble and a caller description are legacy inputs;
/// they reach the prompt as before and never turn the ledger on.
#[tokio::test]
async fn stdin_context_and_pr_description_never_report() {
    let diff = format!("# Context: preamble context for #9192\n{}", billing_diff());
    let (source, _tmp) = local_diff_source(&diff);
    let llm = super::optional_context_off::RecordingLlm::new(Arc::new(FakeLlm::approves()));
    let input = ReviewInput {
        diff_source: source,
        caller_context: CallerContext {
            pr_description: Some("x".to_string()),
            ..CallerContext::default()
        },
        ..super::optional_context_off::github_input(CallerContext::default())
    };
    let deps = ready_deps(llm.clone(), None);
    let outcome = run_review_with(&default_config(), input, deps, ReviewOptions::default()).await;
    assert!(outcome.context_sources.is_empty());
    let user = llm.requests().remove(0).1;
    assert!(user.contains("preamble context for #9192"), "{user}");
}
