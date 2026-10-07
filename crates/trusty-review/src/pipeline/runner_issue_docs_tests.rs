//! Caller `issue_docs` through the real pipeline (#9197, B2a).
//!
//! Why: issue text must reach the reviewer prompt on both review paths, stay
//! out of the verifier's request (Ruling A), and be citable only as far as
//! the reviewer saw it (#9188 rule). A wrong implementation that rendered no
//! block, fed the verifier, widened the corpus past the prompt, or built the
//! corpus from the raw body would pass the unit tests and fail here.
//! What: reuses the fixture run of `runner_optional_context_off_tests.rs` with
//! a reviewer whose one finding cites an issue, and reads the recorded
//! prompts, the result and the source ledger. `TRUSTY_SEARCH_SOCKET` is
//! pinned to a missing path, so no run can reach a live trusty-search.
//! Test: this module.

use super::optional_context_off::{
    CountingSearch, FakePrSource, Observed, RecordingLlm, billing_diff, github_input,
    hermetic_config, legacy_caller, mapreduce_diff,
};
use super::*;
use crate::integrations::search_transport::fixture::EnvGuard;
use crate::models::SourceState;
use crate::pipeline::optional_context::{IssueDoc, OptionalContextRequest};
use trusty_common::search_rpc::TRUSTY_SEARCH_SOCKET_ENV;

/// A PR body that carries no issue id and no excerpt.
const PLAIN_BODY: &str = "Adds an invoice total.";
/// The supplied issue's body; only it carries [`EXCERPT`].
const ISSUE_BODY: &str =
    "ISSUE_SENTINEL_9197: totals must never wrap on overflow; use checked_add.";
/// A sentence of [`ISSUE_BODY`], long enough to be checked as an excerpt.
const EXCERPT: &str = "totals must never wrap on overflow";

/// One grounded finding on `src/billing.rs` that rests on a context
/// citation; APPROVE for every other prompt.
struct CitingReviewer(String);

#[async_trait]
impl LlmProvider for CitingReviewer {
    fn name(&self) -> &str {
        "citing-reviewer-9197"
    }

    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        let user: String = req.messages.iter().map(|m| m.content.as_str()).collect();
        let text = if user.contains("+++ b/src/billing.rs") {
            format!(
                "```json\n{{\"verdict\":\"REQUEST_CHANGES\",\"grade\":\"C\",\"summary\":\
                 \"Overflow risk.\",\"findings\":[{{\"title\":\"overflow\",\"body\":\
                 \"`amounts.iter().sum::<u64>()` can overflow {}.\",\"severity\":\"medium\",\
                 \"confidence\":0.9,\"file\":\"src/billing.rs\",\"line\":10}}]}}\n```",
                self.0
            )
        } else {
            "```json\n{\"verdict\":\"APPROVE\",\"summary\":\"ok\",\"findings\":[]}\n```".to_string()
        };
        Ok(LlmResponse {
            text,
            model: req.model.clone(),
            input_tokens: 100,
            output_tokens: 400,
            latency_ms: 42,
            cost_usd: 0.000042,
            finish_reason: Some("stop".to_string()),
        })
    }
}

/// `[gh: <id> — "<excerpt>"]`, escaped for the reviewer's JSON reply.
fn citation(id: &str, excerpt: &str) -> String {
    format!("[gh: {id} — \\\"{excerpt}\\\"]")
}

fn issue(id: &str, body: &str) -> IssueDoc {
    IssueDoc::new(id, Some("Invoice totals overflow"), body, None).expect("valid doc")
}

fn with_docs(docs: Vec<IssueDoc>) -> ReviewOptions {
    ReviewOptions::new(OptionalContextRequest::default().with_issue_docs(docs))
}

/// Run the fixture PR over `diff` with a reviewer citing `cite`.
async fn run(
    diff: &str,
    caller: CallerContext,
    mut options: ReviewOptions,
    cite: &str,
) -> Observed {
    let missing = std::env::temp_dir().join(format!(
        "trusty-review-9197-no-search-{}.sock",
        std::process::id()
    ));
    let _pin = EnvGuard::set(TRUSTY_SEARCH_SOCKET_ENV, &missing.to_string_lossy());
    let source = Arc::new(FakePrSource::new(PLAIN_BODY, diff));
    let reviewer = RecordingLlm::new(Arc::new(CitingReviewer(cite.to_string())));
    let verifier = RecordingLlm::new(Arc::new(FakeVerifier {
        judgment: "CONFIRMED",
    }));
    let search = Arc::new(CountingSearch::default());
    let deps = ReviewDeps {
        search: search.clone(),
        ..ready_deps(reviewer.clone(), Some(verifier.clone()))
    };
    options.pr_source = Some(source.clone());
    let outcome = run_review_with(&hermetic_config(), github_input(caller), deps, options).await;
    Observed {
        outcome,
        reviewer,
        verifier,
        search,
        source,
    }
}

fn reviewer_user(seen: &Observed) -> String {
    seen.reviewer.requests().remove(0).1
}

/// #9197: a supplied doc reaches the reviewer prompt as `## Linked issues`,
/// after the caller's sections and before the retrieved context.
#[serial_test::serial]
#[tokio::test]
async fn supplied_issue_doc_reaches_the_reviewer_prompt() {
    let options = with_docs(vec![issue("#77", ISSUE_BODY)]);
    let seen = run(
        &billing_diff(),
        legacy_caller(),
        options,
        &citation("#77", EXCERPT),
    )
    .await;
    let user = reviewer_user(&seen);
    let at = |needle: &str| {
        user.find(needle)
            .unwrap_or_else(|| panic!("{needle:?} missing from the reviewer prompt:\n{user}"))
    };
    assert!(at("## Referenced Code") < at("## Linked issues"));
    assert!(at("## Linked issues") < at("### Issue #77 — Invoice totals overflow"));
    assert!(at("### Issue #77 — Invoice totals overflow") < at("ISSUE_SENTINEL_9197"));
    assert!(at("ISSUE_SENTINEL_9197") < at("## Related code"));
}

/// #9197 (Ruling A): the verifier checks the finding but never sees the
/// issue text or its section.
#[serial_test::serial]
#[tokio::test]
async fn issue_doc_never_reaches_the_verifier_request() {
    let options = with_docs(vec![issue("#77", ISSUE_BODY)]);
    let seen = run(
        &billing_diff(),
        legacy_caller(),
        options,
        &citation("#77", EXCERPT),
    )
    .await;
    let verifier = seen.verifier.requests();
    assert!(!verifier.is_empty(), "the finding must reach the verifier");
    for (system, user) in &verifier {
        for text in [system, user] {
            assert!(!text.contains("ISSUE_SENTINEL_9197"), "issue text: {text}");
            assert!(!text.contains("## Linked issues"), "issue section: {text}");
        }
    }
}

/// #9197: a `[gh:]` citation of a supplied doc's id and text resolves; with
/// no doc supplied the same citation is withheld.
#[serial_test::serial]
#[tokio::test]
async fn a_gh_citation_to_a_supplied_issue_resolves() {
    let cite = citation("#77", EXCERPT);
    let options = with_docs(vec![issue("#77", ISSUE_BODY)]);
    let seen = run(&billing_diff(), CallerContext::default(), options, &cite).await;
    let result = &seen.outcome.result;
    assert_eq!(result.findings.len(), 1, "{:?}", result.withheld_findings);

    let seen = run(
        &billing_diff(),
        CallerContext::default(),
        ReviewOptions::default(),
        &cite,
    )
    .await;
    let result = &seen.outcome.result;
    assert!(result.findings.is_empty(), "no doc, nothing to cite");
    assert!(!result.withheld_findings.is_empty());
}

/// #9197: the corpus is no wider than the prompt — an id the caller did not
/// supply is withheld even when the excerpt is in a supplied doc.
#[serial_test::serial]
#[tokio::test]
async fn a_gh_citation_to_an_unsupplied_issue_is_withheld() {
    let options = with_docs(vec![issue("#77", ISSUE_BODY)]);
    let seen = run(
        &billing_diff(),
        CallerContext::default(),
        options,
        &citation("#78", EXCERPT),
    )
    .await;
    let result = &seen.outcome.result;
    assert!(result.findings.is_empty(), "{:?}", result.findings);
    assert!(!result.withheld_findings.is_empty());
}

/// #9197: `#919` does not match inside `#9192`; the id is a whole token.
#[serial_test::serial]
#[tokio::test]
async fn a_gh_id_matches_only_as_a_whole_token_in_issue_docs() {
    let options = with_docs(vec![issue("#9192", ISSUE_BODY)]);
    let seen = run(
        &billing_diff(),
        CallerContext::default(),
        options,
        &citation("#919", EXCERPT),
    )
    .await;
    assert!(seen.outcome.result.findings.is_empty());
    assert!(!seen.outcome.result.withheld_findings.is_empty());
}

/// #9197: text past a doc's 16,000-char cap never reached the reviewer, so a
/// citation of it is withheld; text inside the cap still resolves.
#[serial_test::serial]
#[tokio::test]
async fn a_cut_off_issue_excerpt_is_withheld() {
    let head = "HEAD_9197 totals are summed in cents.";
    let tail = "TAIL_9197 totals wrap past the cap.";
    let mut body = String::from(head);
    body.push_str(&"x".repeat(18_000 - body.len()));
    body.push_str(tail);
    body.push_str(&"y".repeat(20_000 - body.len()));
    for (excerpt, survives) in [(tail, false), (head, true)] {
        let options = with_docs(vec![issue("#77", &body)]);
        let seen = run(
            &billing_diff(),
            CallerContext::default(),
            options,
            &citation("#77", excerpt),
        )
        .await;
        assert_eq!(
            seen.outcome.result.findings.len(),
            usize::from(survives),
            "{excerpt}: {:?}",
            seen.outcome.result.withheld_findings
        );
    }
}

/// #9197: on the map-reduce path every chunk prompt carries the issue block,
/// and a citation of the doc resolves there too.
#[serial_test::serial]
#[tokio::test]
async fn mapreduce_chunk_prompts_carry_the_issue_block() {
    let options = with_docs(vec![issue("#77", ISSUE_BODY)]);
    let seen = run(
        &mapreduce_diff(),
        CallerContext::default(),
        options,
        &citation("#77", EXCERPT),
    )
    .await;
    let result = &seen.outcome.result;
    assert!(
        result.review_body.contains("Map-reduce review:"),
        "map-reduce path"
    );
    let chunks: Vec<String> = seen
        .reviewer
        .requests()
        .into_iter()
        .map(|(_, user)| user)
        .filter(|user| user.contains("## Unified diff"))
        .collect();
    assert!(chunks.len() > 1, "several chunk prompts: {}", chunks.len());
    for chunk in &chunks {
        assert!(
            chunk.contains("ISSUE_SENTINEL_9197"),
            "chunk without the issue:\n{chunk}"
        );
    }
    assert_eq!(result.findings.len(), 1, "{:?}", result.withheld_findings);
}

/// #9197: the `issues` row follows the caller row, one item per doc.
#[serial_test::serial]
#[tokio::test]
async fn ledger_records_the_issues_row_after_the_caller_row() {
    let options = with_docs(vec![issue("77", ISSUE_BODY)]);
    let seen = run(
        &billing_diff(),
        legacy_caller(),
        options,
        &citation("#77", EXCERPT),
    )
    .await;
    let sources = &seen.outcome.context_sources;
    let names: Vec<&str> = sources.iter().map(|r| r.source.as_str()).collect();
    // #9194: every row is listed; `issues` still follows the caller row.
    let at = |name: &str| names.iter().position(|n| *n == name);
    assert!(at("caller_context") < at("issues"), "{names:?}");
    let issues = &sources[at("issues").expect("an issues row")];
    assert_eq!(issues.state, SourceState::Used);
    assert_eq!(issues.chars, ISSUE_BODY.chars().count());
    assert_eq!(issues.items.len(), 1);
    assert_eq!(issues.items[0].id, "#77");
    assert_eq!(issues.items[0].state, SourceState::Used);
}
