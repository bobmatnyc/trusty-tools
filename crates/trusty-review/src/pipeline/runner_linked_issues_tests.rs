//! `fetch_linked_issues` through the real pipeline (#9197, B2b).
//!
//! Why: a fetched issue must reach the reviewer prompt and the refs corpus on
//! both review paths, never the verifier, and only from the raw PR body; a
//! wrong implementation that skipped the corpus, fed the verifier, parsed the
//! caller's text, or left two `issues` rows would pass the unit tests.
//! What: reuses the fixture run of `runner_optional_context_off_tests.rs`
//! with a reviewer whose one finding cites an issue and a [`FakeFetcher`]
//! seam, and reads the recorded prompts, the result and the source ledger.
//! `TRUSTY_SEARCH_SOCKET` is pinned to a missing path. The failure branches
//! are in `runner_linked_issues_failure_tests.rs`.
//! Test: this module.

use super::optional_context_off::{
    CountingSearch, FakePrSource, RecordingLlm, billing_diff, github_input, hermetic_config,
    legacy_caller, mapreduce_diff,
};
use super::*;
use crate::integrations::search_transport::fixture::EnvGuard;
use crate::models::{ContextSourceRecord, SourceState};
use crate::pipeline::optional_context::{
    IssueDoc, OptionalContextRequest,
    linked_issues::tests::{Answer, FakeFetcher},
};
use trusty_common::intent_source::TicketFetcher;
use trusty_common::search_rpc::TRUSTY_SEARCH_SOCKET_ENV;

/// The fetched issue's body; only it carries [`EXCERPT`].
pub(super) const ISSUE_BODY: &str =
    "LINKED_SENTINEL_9197: totals must never wrap on overflow; use checked_add.";
/// A sentence of [`ISSUE_BODY`], long enough to be checked as an excerpt.
pub(super) const EXCERPT: &str = "totals must never wrap on overflow";
/// A PR body that links `#12`.
pub(super) const LINKING_BODY: &str = "Adds an invoice total.\n\nRefs #12";

/// One grounded finding on `src/billing.rs` citing `.0`; APPROVE otherwise.
struct CitingReviewer(String);

#[async_trait]
impl LlmProvider for CitingReviewer {
    fn name(&self) -> &str {
        "citing-reviewer-9197-b2b"
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
        let (model, finish_reason) = (req.model.clone(), Some("stop".to_string()));
        Ok(LlmResponse {
            text,
            model,
            input_tokens: 100,
            output_tokens: 400,
            latency_ms: 42,
            cost_usd: 0.000042,
            finish_reason,
        })
    }
}

/// `[gh: <id> — "<excerpt>"]`, escaped for the reviewer's JSON reply.
pub(super) fn citation(id: &str, excerpt: &str) -> String {
    format!("[gh: {id} — \\\"{excerpt}\\\"]")
}

/// A fetcher answering `#12` with [`ISSUE_BODY`].
pub(super) fn issue_12() -> FakeFetcher {
    let doc = Answer::Doc("Invoice totals overflow".into(), ISSUE_BODY.into());
    FakeFetcher::default().answer(12, doc)
}

/// The flag on.
pub(super) fn on() -> OptionalContextRequest {
    OptionalContextRequest::default().with_fetch_linked_issues(true)
}

/// Everything one run observed.
pub(super) struct Seen {
    pub(super) outcome: ReviewOutcome,
    pub(super) reviewer: Arc<RecordingLlm>,
    pub(super) verifier: Arc<RecordingLlm>,
    pub(super) calls: Vec<String>,
}

impl Seen {
    /// The first reviewer prompt's user text.
    pub(super) fn prompt(&self) -> String {
        self.reviewer.requests().remove(0).1
    }

    /// The single `issues` row.
    pub(super) fn issues(&self) -> &ContextSourceRecord {
        let mut rows = self
            .outcome
            .context_sources
            .iter()
            .filter(|r| r.source == "issues");
        let row = rows.next().expect("an issues row");
        assert!(rows.next().is_none(), "one issues row");
        row
    }
}

/// Run `source` with `request`, the `fetcher` seam (none: the production
/// fetcher) and a reviewer citing `cite`, over `input`.
pub(super) async fn review(
    source: FakePrSource,
    request: OptionalContextRequest,
    fetcher: Option<FakeFetcher>,
    cite: &str,
    input: ReviewInput,
) -> Seen {
    let missing = std::env::temp_dir().join(format!(
        "trusty-review-9197-b2b-no-search-{}.sock",
        std::process::id()
    ));
    let _pin = EnvGuard::set(TRUSTY_SEARCH_SOCKET_ENV, &missing.to_string_lossy());
    let reviewer = RecordingLlm::new(Arc::new(CitingReviewer(cite.to_string())));
    let verifier = RecordingLlm::new(Arc::new(FakeVerifier {
        judgment: "CONFIRMED",
    }));
    let deps = ReviewDeps {
        search: Arc::new(CountingSearch::default()),
        ..ready_deps(reviewer.clone(), Some(verifier.clone()))
    };
    let mut options = ReviewOptions::new(request);
    options.pr_source = Some(Arc::new(source));
    let fetcher = fetcher.map(Arc::new);
    options.ticket_fetcher = fetcher.clone().map(|f| f as Arc<dyn TicketFetcher>);
    let outcome = run_review_with(&hermetic_config(), input, deps, options).await;
    let calls = fetcher.map(|f| f.ids()).unwrap_or_default();
    Seen {
        outcome,
        reviewer,
        verifier,
        calls,
    }
}

/// [`review`] on the fixture PR (`acme/billing#7`) with PR `body`.
pub(super) async fn review_body(
    body: &str,
    request: OptionalContextRequest,
    fetcher: FakeFetcher,
    cite: &str,
) -> Seen {
    let source = FakePrSource::new(body, &billing_diff());
    review(
        source,
        request,
        Some(fetcher),
        cite,
        github_input(legacy_caller()),
    )
    .await
}

/// AC1, AC9: the fetched issue reaches the prompt as a fenced block and
/// the corpus, so its citation resolves; the ledger item is `used`.
#[serial_test::serial]
#[tokio::test]
async fn fetched_issue_reaches_the_prompt_and_the_corpus() {
    let seen = review_body(LINKING_BODY, on(), issue_12(), &citation("#12", EXCERPT)).await;
    assert_eq!(seen.calls, ["12"]);
    let user = seen.prompt();
    let block =
        "### Issue #12 — Invoice totals overflow\nURL: https://github.com/acme/billing/issues/12";
    assert!(
        user.contains(&format!("{block}\n\n```text\n{ISSUE_BODY}\n```")),
        "{user}"
    );
    let result = &seen.outcome.result;
    assert_eq!(result.findings.len(), 1, "{:?}", result.withheld_findings);
    let issues = seen.issues();
    assert_eq!(
        (issues.state, issues.items[0].id.as_str()),
        (SourceState::Used, "#12")
    );
}

/// AC9: a citation of an issue that was not fetched is withheld; so is the
/// same citation with the flag off.
#[serial_test::serial]
#[tokio::test]
async fn a_gh_citation_to_an_unfetched_issue_is_withheld() {
    let seen = review_body(LINKING_BODY, on(), issue_12(), &citation("#13", EXCERPT)).await;
    assert!(seen.outcome.result.findings.is_empty());
    let off = OptionalContextRequest::default();
    let seen = review_body(LINKING_BODY, off, issue_12(), &citation("#12", EXCERPT)).await;
    assert!(seen.calls.is_empty(), "AC2: no call with the flag off");
    assert!(seen.outcome.result.findings.is_empty());
    assert!(!seen.outcome.result.withheld_findings.is_empty());
}

/// AC10 (Ruling A): the verifier checks the finding but never sees the text.
#[serial_test::serial]
#[tokio::test]
async fn fetched_text_never_reaches_the_verifier() {
    let seen = review_body(LINKING_BODY, on(), issue_12(), &citation("#12", EXCERPT)).await;
    let verifier = seen.verifier.requests();
    assert!(!verifier.is_empty(), "the finding must reach the verifier");
    for (system, user) in &verifier {
        assert!(!system.contains("LINKED_SENTINEL") && !user.contains("LINKED_SENTINEL"));
        assert!(!user.contains("## Linked issues"), "{user}");
    }
}

/// AC3: refs in the caller's fields or the title are never fetched; with
/// `include_pr_body` off the raw body still supplies refs but stays out of
/// the prompt.
#[serial_test::serial]
#[tokio::test]
async fn refs_outside_the_raw_body_are_never_fetched() {
    let caller = CallerContext {
        pr_description: Some("Refs #5".into()),
        pr_discussion: Some("Fixes #6".into()),
        referenced_code: Some("// Closes #8".into()),
    };
    let source = FakePrSource::new("Adds an invoice total.", &billing_diff());
    let seen = review(source, on(), Some(issue_12()), "", github_input(caller)).await;
    assert!(seen.calls.is_empty(), "{:?}", seen.calls);
    let body = "BODY_ONLY_9197 Refs #12";
    let seen = review_body(body, on(), issue_12(), "").await;
    assert_eq!(seen.calls, ["12"]);
    assert!(!seen.prompt().contains("BODY_ONLY_9197"));
}

/// AC17: with supplied docs and the flag, exactly one `issues` row survives
/// `finish`, carrying both origins' items.
#[serial_test::serial]
#[tokio::test]
async fn exactly_one_issues_row_survives_finish() {
    let docs = vec![IssueDoc::new("#3", None, "SUPPLIED_3", None).expect("valid")];
    let seen = review_body(LINKING_BODY, on().with_issue_docs(docs), issue_12(), "").await;
    let ids: Vec<&str> = seen.issues().items.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(ids, ["#3", "#12"]);
    let user = seen.prompt();
    assert!(
        user.find("SUPPLIED_3") < user.find("LINKED_SENTINEL"),
        "supplied first"
    );
}

/// AC14: every map-reduce chunk prompt carries the fetched issue, and its
/// citation resolves there too.
#[serial_test::serial]
#[tokio::test]
async fn mapreduce_chunks_carry_the_fetched_issue() {
    let source = FakePrSource::new(LINKING_BODY, &mapreduce_diff());
    let input = github_input(CallerContext::default());
    let seen = review(
        source,
        on(),
        Some(issue_12()),
        &citation("#12", EXCERPT),
        input,
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
        .map(|(_, u)| u)
        .collect();
    let chunks: Vec<&String> = chunks
        .iter()
        .filter(|u| u.contains("## Unified diff"))
        .collect();
    assert!(chunks.len() > 1, "several chunk prompts: {}", chunks.len());
    assert!(chunks.iter().all(|c| c.contains("LINKED_SENTINEL_9197")));
    assert_eq!(result.findings.len(), 1, "{:?}", result.withheld_findings);
}
