//! `changed_files` through the real pipeline (#9195, B4).
//!
//! Why: the changed files must reach the unified prompt and each map-reduce
//! chunk prompt as the ledger says, leave the prompts byte-identical when off
//! or at budget 0 or with no fetcher, and never become citable (ruling Q2).
//! What: reuses the fixture PR of `runner_optional_context_off_tests.rs` and
//! the recording `FakeFetcher` of `runner_spec_docs_tests.rs`, with a
//! reviewer that quotes a chosen line. `TRUSTY_SEARCH_SOCKET` is pinned to a
//! missing path.
//! Test: this module.

use super::optional_context_off::{
    FakePrSource, RecordingLlm, billing_diff, github_input, hermetic_config, mapreduce_diff,
};
use super::spec_docs::FakeFetcher;
use super::*;
use crate::integrations::search_transport::fixture::EnvGuard;
use crate::models::{ContextSourceRecord, SourceState};
use crate::pipeline::optional_context::{OptionalContextRequest, seams::PrHead};
use trusty_common::search_rpc::TRUSTY_SEARCH_SOCKET_ENV;

const HEAD: &str = super::optional_context_off::HEAD_SHA;
const BILLING: &str = "src/billing.rs";
/// `src/billing.rs` at the head: the diff's lines plus one the diff never
/// shows, which only the full file carries.
const BILLING_AT_HEAD: &str = "fn total(amounts: &[u64], input: u64) -> u64 {\n    \
    let total = amounts.iter().sum::<u64>();\n    total + step_2(input)\n}\n\n\
    fn rate(input: u64) -> u64 {\n    let rate = unchecked_rate_9195(input);\n    rate\n}\n";
/// The line only the full file carries.
const OUTSIDE_DIFF: &str = "let rate = unchecked_rate_9195(input);";

/// One grounded finding on `src/billing.rs` quoting `quote` at `line`, or
/// APPROVE when `quote` is empty or the prompt is not the billing chunk.
pub(super) struct QuotingReviewer {
    pub(super) quote: &'static str,
    pub(super) line: u32,
}

#[async_trait]
impl LlmProvider for QuotingReviewer {
    fn name(&self) -> &str {
        "quoting-reviewer-9195"
    }

    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        let user: String = req.messages.iter().map(|m| m.content.as_str()).collect();
        let text = if !self.quote.is_empty() && user.contains("+++ b/src/billing.rs") {
            format!(
                "```json\n{{\"verdict\":\"REQUEST_CHANGES\",\"grade\":\"C\",\"summary\":\
                 \"Overflow risk.\",\"findings\":[{{\"title\":\"overflow\",\"body\":\
                 \"`{q}` can overflow [code: `src/billing.rs:{l}` — \\\"{q}\\\"].\",\
                 \"severity\":\"medium\",\"confidence\":0.9,\"file\":\"src/billing.rs\",\
                 \"line\":{l}}}]}}\n```",
                q = self.quote,
                l = self.line
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

/// What one fixture run observed.
struct Seen {
    outcome: ReviewOutcome,
    reviewer: Arc<RecordingLlm>,
    verifier: Arc<RecordingLlm>,
}

impl Seen {
    fn reviewer_users(&self) -> Vec<String> {
        self.reviewer
            .requests()
            .into_iter()
            .map(|(_, u)| u)
            .collect()
    }

    fn row(&self) -> &ContextSourceRecord {
        self.outcome
            .context_sources
            .iter()
            .find(|r| r.source == "changed_files")
            .unwrap_or_else(|| panic!("no changed_files row: {:?}", self.outcome.context_sources))
    }

    fn item(&self, id: &str) -> SourceState {
        self.row()
            .items
            .iter()
            .find(|i| i.id == id)
            .unwrap_or_else(|| panic!("no {id} item in {:?}", self.row()))
            .state
    }
}

/// One fixture run's inputs.
struct Setup {
    request: OptionalContextRequest,
    fetcher: Arc<FakeFetcher>,
    diff: String,
    head: PrHead,
    reviewer: QuotingReviewer,
}

impl Setup {
    fn new(request: OptionalContextRequest, fetcher: Arc<FakeFetcher>) -> Self {
        Self {
            request,
            fetcher,
            diff: billing_diff(),
            head: PrHead {
                sha: HEAD.to_string(),
                fork: false,
            },
            reviewer: QuotingReviewer { quote: "", line: 0 },
        }
    }
}

fn on() -> OptionalContextRequest {
    OptionalContextRequest::default().with_changed_files(true)
}

fn billing_fetcher() -> Arc<FakeFetcher> {
    FakeFetcher::with(&[(BILLING, Ok(Some(BILLING_AT_HEAD.to_string())))])
}

async fn run(setup: Setup) -> Seen {
    let missing = std::env::temp_dir().join(format!(
        "trusty-review-9195-no-search-{}.sock",
        std::process::id()
    ));
    let _pin = EnvGuard::set(TRUSTY_SEARCH_SOCKET_ENV, &missing.to_string_lossy());
    let mut source = FakePrSource::new("Adds an invoice total.", &setup.diff);
    source.head = setup.head;
    let reviewer = RecordingLlm::new(Arc::new(setup.reviewer));
    let verifier = RecordingLlm::new(Arc::new(FakeVerifier {
        judgment: "CONFIRMED",
    }));
    let deps = ready_deps(reviewer.clone(), Some(verifier.clone()));
    let mut options = ReviewOptions::new(setup.request);
    options.pr_source = Some(Arc::new(source));
    options.doc_fetcher = Some(setup.fetcher);
    let input = github_input(CallerContext::default());
    let outcome = run_review_with(&hermetic_config(), input, deps, options).await;
    Seen {
        outcome,
        reviewer,
        verifier,
    }
}

// ── AC1 and ruling Q4: both review paths ─────────────────────────────────────

/// #9195 AC1, amendment 8: the file text reaches the unified prompt, never
/// the verifier, and the ledger records it `used`.
#[serial_test::serial]
#[tokio::test]
async fn file_text_reaches_the_unified_prompt_and_the_ledger_as_used() {
    let fetcher = billing_fetcher();
    let seen = run(Setup::new(on(), fetcher.clone())).await;
    assert_eq!(fetcher.calls(), [(BILLING.to_string(), HEAD.to_string())]);
    let users = seen.reviewer_users();
    assert!(users.iter().any(|u| u.contains(OUTSIDE_DIFF)), "{users:?}");
    assert!(
        users
            .iter()
            .any(|u| u.contains("## Changed files (full text at the PR head)"))
    );
    assert_eq!(seen.item(BILLING), SourceState::Used);
    assert!(
        seen.verifier
            .requests()
            .iter()
            .all(|(_, u)| !u.contains(OUTSIDE_DIFF))
    );
}

/// #9195 ruling Q4, amendment 8: on the map-reduce path each chunk prompt
/// carries its own file's text and no other file's; every file is `used`.
#[serial_test::serial]
#[tokio::test]
async fn file_text_reaches_only_its_own_mapreduce_chunk() {
    let mut files: Vec<(String, String)> = (0..12)
        .map(|i| {
            (
                format!("src/f{i}.rs"),
                format!("pub fn f{i}() {{}} // SENT_F{i}_9195\n"),
            )
        })
        .collect();
    files.push((BILLING.to_string(), BILLING_AT_HEAD.to_string()));
    let served: Vec<(&str, Result<Option<String>, _>)> = files
        .iter()
        .map(|(p, t)| (p.as_str(), Ok(Some(t.clone()))))
        .collect();
    let mut setup = Setup::new(on(), FakeFetcher::with(&served));
    setup.diff = mapreduce_diff();
    let seen = run(setup).await;
    let chunks: Vec<String> = seen
        .reviewer_users()
        .into_iter()
        .filter(|u| u.contains("+++ b/"))
        .collect();
    assert!(chunks.len() > 1, "map-reduce ran");
    for i in 0..12 {
        let own = format!("SENT_F{i}_9195");
        let header = format!("+++ b/src/f{i}.rs");
        for chunk in &chunks {
            assert_eq!(
                chunk.contains(&own),
                chunk.contains(&header),
                "f{i}'s text rides only f{i}'s chunk"
            );
        }
        assert_eq!(seen.item(&format!("src/f{i}.rs")), SourceState::Used);
    }
    assert_eq!(seen.item(BILLING), SourceState::Used);
}

// ── AC4: byte identity when off, at budget 0, or with no fetcher ─────────────

/// #9195 AC4: budget 0 leaves every reviewer prompt byte-identical to a run
/// without the flag, reads nothing, and says why in the ledger.
#[serial_test::serial]
#[tokio::test]
async fn budget_zero_prompt_equals_diff_only() {
    let off = run(Setup::new(
        OptionalContextRequest::default(),
        billing_fetcher(),
    ))
    .await;
    let fetcher = billing_fetcher();
    let zero = run(Setup::new(
        on().with_changed_files_budget(0),
        fetcher.clone(),
    ))
    .await;
    assert_eq!(off.reviewer.requests(), zero.reviewer.requests());
    assert!(fetcher.calls().is_empty());
    assert_eq!(zero.row().state, SourceState::Absent);
}

/// #9195 AC4, amendment 1: with no usable head SHA (no fetcher) the prompts
/// are byte-identical to diff-only and the row is `unavailable`.
#[serial_test::serial]
#[tokio::test]
async fn no_fetcher_leaves_prompt_byte_identical_to_diff_only() {
    let mut off = Setup::new(OptionalContextRequest::default(), billing_fetcher());
    off.head.sha = String::new();
    let off = run(off).await;
    let fetcher = billing_fetcher();
    let mut on_setup = Setup::new(on(), fetcher.clone());
    on_setup.head.sha = String::new();
    let seen = run(on_setup).await;
    assert_eq!(off.reviewer.requests(), seen.reviewer.requests());
    assert!(fetcher.calls().is_empty());
    assert_eq!(seen.row().state, SourceState::Unavailable);
}

/// #9195 amendment 10: a budget without the flag changes nothing end to
/// end: same prompts, no read, no ledger.
#[serial_test::serial]
#[tokio::test]
async fn budget_without_flag_is_inert_end_to_end() {
    let off = run(Setup::new(
        OptionalContextRequest::default(),
        billing_fetcher(),
    ))
    .await;
    let fetcher = billing_fetcher();
    let request = OptionalContextRequest::default().with_changed_files_budget(50_000);
    let seen = run(Setup::new(request, fetcher.clone())).await;
    assert_eq!(off.reviewer.requests(), seen.reviewer.requests());
    assert!(fetcher.calls().is_empty());
    assert!(seen.outcome.context_sources.is_empty());
}

// ── Ruling Q2: full-file lines are never citable ─────────────────────────────

/// #9195 ruling Q2: the reviewer saw a line only the full file carries; a
/// finding citing it is withheld, while one citing a diff line survives.
#[serial_test::serial]
#[tokio::test]
async fn a_citation_to_a_line_outside_the_diff_is_still_withheld() {
    let mut setup = Setup::new(on(), billing_fetcher());
    setup.reviewer = QuotingReviewer {
        quote: OUTSIDE_DIFF,
        line: 13,
    };
    let seen = run(setup).await;
    assert!(
        seen.reviewer_users()
            .iter()
            .any(|u| u.contains(OUTSIDE_DIFF)),
        "the reviewer saw the full-file line"
    );
    assert!(
        seen.outcome.result.findings.is_empty(),
        "{:?}",
        seen.outcome.result.findings
    );
    assert_eq!(seen.outcome.result.withheld_findings.len(), 1);

    let mut control = Setup::new(on(), billing_fetcher());
    control.reviewer = QuotingReviewer {
        quote: "let total = amounts.iter().sum::<u64>();",
        line: 10,
    };
    let seen = run(control).await;
    assert_eq!(
        seen.outcome.result.findings.len(),
        1,
        "a diff line stays citable: {:?}",
        seen.outcome.result.withheld_findings
    );
}
