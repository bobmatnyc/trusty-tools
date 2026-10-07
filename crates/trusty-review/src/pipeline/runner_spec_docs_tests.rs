//! `spec_docs` and `claude_md` through the real pipeline (#9193, B3).
//!
//! Why: docs must be read at the PR head SHA only when asked for, reach the
//! reviewer prompt on both paths, stay out of the flat refs corpus, be
//! citable as `[doc:]` only as far as the reviewer saw them, and never block
//! a review when a read or the search fails.
//! What: reuses the fixture PR of `runner_optional_context_off_tests.rs`
//! with a fake `DocFetcher` that records every `(path, sha)` read and a fake
//! search with set hits. `TRUSTY_SEARCH_SOCKET` is pinned to a missing path.
//! Test: this module.

use std::collections::HashMap;
use std::sync::Mutex;

use super::optional_context_off::{
    FakePrSource, RecordingLlm, billing_diff, github_input, hermetic_config, mapreduce_diff,
};
use super::*;
use crate::integrations::context::contents_at_ref::{DocFetchError, DocFetcher};
use crate::integrations::search_client::IndexStatusResponse;
use crate::integrations::search_transport::fixture::EnvGuard;
use crate::models::{ContextSourceRecord, SourceState};
use crate::pipeline::optional_context::{OptionalContextRequest, seams::PrHead};
use trusty_common::search_rpc::TRUSTY_SEARCH_SOCKET_ENV;

const HEAD: &str = super::optional_context_off::HEAD_SHA;
const ADR: &str = "docs/adr/0001-totals.md";
/// The ADR at the head; only it carries [`EXCERPT`] and the sentinel.
const ADR_TEXT: &str =
    "ADR_SENTINEL_9193\n\nInvoice totals must use checked_add so a sum never wraps.\n";
/// The ADR on the default branch: what a wrong implementation would read.
const STALE_TEXT: &str = "STALE_DEFAULT_BRANCH_TEXT: totals may wrap.";
const EXCERPT: &str = "Invoice totals must use checked_add";
/// The PR body: names the ADR and `#42`, but never [`EXCERPT`].
const BODY: &str = "Adds an invoice total. Follows up #42. Implements docs/adr/0001-totals.md.";

// ── Fakes ────────────────────────────────────────────────────────────────────

/// A [`DocFetcher`] serving `files` at [`HEAD`] and [`STALE_TEXT`] at any
/// other ref; a path it does not hold is a 404.
#[derive(Default)]
pub(super) struct FakeFetcher {
    files: HashMap<String, Result<Option<String>, DocFetchError>>,
    calls: Mutex<Vec<(String, String)>>,
}

impl FakeFetcher {
    pub(super) fn with(files: &[(&str, Result<Option<String>, DocFetchError>)]) -> Arc<Self> {
        Arc::new(Self {
            files: files
                .iter()
                .map(|(p, r)| (p.to_string(), r.clone()))
                .collect(),
            calls: Mutex::new(Vec::new()),
        })
    }

    fn adr() -> Arc<Self> {
        Self::with(&[(ADR, Ok(Some(ADR_TEXT.to_string())))])
    }

    pub(super) fn calls(&self) -> Vec<(String, String)> {
        self.calls.lock().expect("lock").clone()
    }

    fn paths(&self) -> Vec<String> {
        self.calls().into_iter().map(|(p, _)| p).collect()
    }
}

#[async_trait]
impl DocFetcher for FakeFetcher {
    async fn fetch(&self, path: &str, sha: &str) -> Result<Option<String>, DocFetchError> {
        self.calls
            .lock()
            .expect("lock")
            .push((path.to_string(), sha.to_string()));
        if sha != HEAD {
            return Ok(Some(STALE_TEXT.to_string()));
        }
        self.files.get(path).cloned().unwrap_or(Ok(None))
    }
}

/// A search client whose `search` returns `hits` (or fails), and whose index
/// `main` is rooted at `/srv/repo`.
#[derive(Default)]
struct DocSearch {
    hits: Vec<(String, String)>,
    fail: bool,
}

#[async_trait]
impl SearchClient for DocSearch {
    async fn index_status(&self, index_id: &str) -> Result<IndexStatusResponse, SearchClientError> {
        FakeSearch.index_status(index_id).await
    }

    async fn health(&self) -> Result<HealthResponse, SearchClientError> {
        FakeSearch.health().await
    }

    async fn list_indexes(&self) -> Result<Vec<IndexInfo>, SearchClientError> {
        Ok(vec![IndexInfo {
            id: "main".to_string(),
            name: None,
            root_path: Some("/srv/repo".to_string()),
        }])
    }

    async fn search(
        &self,
        _index_id: &str,
        _query: &str,
        _top_k: Option<u32>,
    ) -> Result<Vec<SearchResult>, SearchClientError> {
        if self.fail {
            return Err(SearchClientError::Transport("fixture: search down".into()));
        }
        Ok(self
            .hits
            .iter()
            .map(|(file, snippet)| SearchResult {
                file: file.clone(),
                snippet: Some(snippet.clone()),
                score: 0.9,
                start_line: None,
                end_line: None,
            })
            .collect())
    }
}

/// One grounded finding on `src/billing.rs` carrying `cite`; APPROVE elsewhere.
struct CitingReviewer(String);

#[async_trait]
impl LlmProvider for CitingReviewer {
    fn name(&self) -> &str {
        "citing-reviewer-9193"
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

/// `[doc: <path>@<sha> — "<excerpt>"]`, escaped for the reviewer's JSON reply.
fn doc_cite(path: &str, sha: &str, excerpt: &str) -> String {
    format!("[doc: {path}@{sha} — \\\"{excerpt}\\\"]")
}

// ── Fixture run ──────────────────────────────────────────────────────────────

/// One fixture run's inputs.
struct Setup {
    body: &'static str,
    diff: String,
    head: PrHead,
    request: OptionalContextRequest,
    fetcher: Arc<FakeFetcher>,
    search: DocSearch,
    cite: String,
}

impl Setup {
    fn new(request: OptionalContextRequest, fetcher: Arc<FakeFetcher>) -> Self {
        Self {
            body: BODY,
            diff: billing_diff(),
            head: PrHead {
                sha: HEAD.to_string(),
                fork: false,
            },
            request,
            fetcher,
            search: DocSearch::default(),
            cite: String::new(),
        }
    }
}

/// What one fixture run observed.
struct Seen {
    outcome: ReviewOutcome,
    reviewer: Arc<RecordingLlm>,
    verifier: Arc<RecordingLlm>,
}

impl Seen {
    fn reviewer_user(&self) -> String {
        self.reviewer
            .requests()
            .into_iter()
            .map(|(_, u)| u)
            .collect::<Vec<_>>()
            .join("\n=====\n")
    }

    fn row(&self, source: &str) -> &ContextSourceRecord {
        self.outcome
            .context_sources
            .iter()
            .find(|r| r.source == source)
            .unwrap_or_else(|| panic!("no {source} row: {:?}", self.outcome.context_sources))
    }

    fn item(&self, source: &str, id: &str) -> SourceState {
        self.row(source)
            .items
            .iter()
            .find(|i| i.id == id)
            .unwrap_or_else(|| panic!("no {id} item in {:?}", self.row(source)))
            .state
    }
}

fn spec_docs() -> OptionalContextRequest {
    OptionalContextRequest::default().with_spec_docs(true)
}

async fn run(setup: Setup) -> Seen {
    let missing = std::env::temp_dir().join(format!(
        "trusty-review-9193-no-search-{}.sock",
        std::process::id()
    ));
    let _pin = EnvGuard::set(TRUSTY_SEARCH_SOCKET_ENV, &missing.to_string_lossy());
    let mut source = FakePrSource::new(setup.body, &setup.diff);
    source.head = setup.head;
    let reviewer = RecordingLlm::new(Arc::new(CitingReviewer(setup.cite)));
    let verifier = RecordingLlm::new(Arc::new(FakeVerifier {
        judgment: "CONFIRMED",
    }));
    let deps = ReviewDeps {
        search: Arc::new(setup.search),
        ..ready_deps(reviewer.clone(), Some(verifier.clone()))
    };
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

// ── Criterion 1: read at the head ────────────────────────────────────────────

/// #9193 criterion 1: the ADR is read at the head SHA the PR metadata
/// reported, and the reviewer sees the head text, never the default branch's.
#[serial_test::serial]
#[tokio::test]
async fn fake_fetcher_serves_different_text_per_sha() {
    let fetcher = FakeFetcher::adr();
    let seen = run(Setup::new(spec_docs(), fetcher.clone())).await;
    assert_eq!(fetcher.calls(), [(ADR.to_string(), HEAD.to_string())]);
    let user = seen.reviewer_user();
    assert!(user.contains("ADR_SENTINEL_9193"), "{user}");
    assert!(user.contains(&format!("### {ADR}@{HEAD}")));
    assert!(!user.contains("STALE_DEFAULT_BRANCH_TEXT"));
    assert_eq!(seen.item("spec_docs", ADR), SourceState::Used);
    assert!(
        seen.verifier
            .requests()
            .iter()
            .all(|(_, u)| !u.contains("ADR_SENTINEL_9193"))
    );
}

/// Docs reach every map-reduce chunk prompt.
#[serial_test::serial]
#[tokio::test]
async fn docs_reach_every_mapreduce_chunk_prompt() {
    let mut setup = Setup::new(spec_docs(), FakeFetcher::adr());
    setup.diff = mapreduce_diff();
    let seen = run(setup).await;
    let requests = seen.reviewer.requests();
    assert!(requests.len() > 1, "map-reduce ran");
    let chunks: Vec<_> = requests
        .iter()
        .filter(|(_, u)| u.contains("+++ b/"))
        .collect();
    assert!(!chunks.is_empty());
    assert!(chunks.iter().all(|(_, u)| u.contains("ADR_SENTINEL_9193")));
}

// ── Criterion 3: opt-in, default unchanged ───────────────────────────────────

/// #9193 criterion 3: with both flags off, a body naming a doc reads nothing,
/// searches for nothing extra, and records no doc row.
#[serial_test::serial]
#[tokio::test]
async fn spec_docs_off_with_a_doc_path_in_the_body_reads_nothing() {
    let fetcher = FakeFetcher::adr();
    let seen = run(Setup::new(
        OptionalContextRequest::default(),
        fetcher.clone(),
    ))
    .await;
    assert!(fetcher.calls().is_empty());
    assert!(seen.outcome.context_sources.is_empty());
    let user = seen.reviewer_user();
    assert!(!user.contains("## Referenced docs") && !user.contains("CLAUDE.md"));
}

/// #9193 criterion 3: CLAUDE.md is read only when `claude_md` is set.
#[serial_test::serial]
#[tokio::test]
async fn claude_md_off_makes_no_fetch_and_no_section() {
    let fetcher = FakeFetcher::with(&[("CLAUDE.md", Ok(Some("Use thiserror.".into())))]);
    let seen = run(Setup::new(
        OptionalContextRequest::default(),
        fetcher.clone(),
    ))
    .await;
    assert!(fetcher.calls().is_empty());
    assert!(!seen.reviewer_user().contains("## Repository conventions"));
}

/// #9193 amendment 15: the default reviewer system prompt never mentions the
/// `[doc:]` form, and a default run's prompts carry no doc section.
#[serial_test::serial]
#[tokio::test]
async fn default_prompts_never_mention_doc_citations() {
    use crate::pipeline::prompt_templates::{SYSTEM_PROMPT_COVERAGE_GATING, SYSTEM_PROMPT_STOCK};
    assert!(!SYSTEM_PROMPT_STOCK.contains("[doc:"));
    assert!(!SYSTEM_PROMPT_COVERAGE_GATING.contains("[doc:"));
    let seen = run(Setup::new(
        OptionalContextRequest::default(),
        FakeFetcher::adr(),
    ))
    .await;
    for (system, user) in seen.reviewer.requests() {
        assert!(!system.contains("[doc:") && !user.contains("[doc:"));
    }
}

/// #9193 amendment 14: `spec_docs` on with no doc read leaves every reviewer
/// prompt byte-identical to the default run's.
#[serial_test::serial]
#[tokio::test]
async fn spec_docs_on_with_zero_docs_leaves_prompt_byte_identical() {
    let off = run(Setup::new(
        OptionalContextRequest::default(),
        FakeFetcher::with(&[]),
    ))
    .await;
    let on = run(Setup::new(spec_docs(), FakeFetcher::with(&[]))).await;
    assert_eq!(off.reviewer.requests(), on.reviewer.requests());
    assert_eq!(on.item("spec_docs", ADR), SourceState::Absent);
}

/// #9193 amendment 8: `claude_md` alone reads only CLAUDE.md, not the ADR
/// the body names, and renders the conventions section.
#[serial_test::serial]
#[tokio::test]
async fn claude_md_alone_fetches_only_claude_md() {
    let fetcher = FakeFetcher::with(&[
        (
            "CLAUDE.md",
            Ok(Some("CONVENTION_SENTINEL: use thiserror.".into())),
        ),
        (ADR, Ok(Some(ADR_TEXT.into()))),
    ]);
    let request = OptionalContextRequest::default().with_claude_md(true);
    let seen = run(Setup::new(request, fetcher.clone())).await;
    assert_eq!(fetcher.paths(), ["CLAUDE.md"]);
    let user = seen.reviewer_user();
    assert!(user.contains("## Repository conventions (CLAUDE.md)"));
    assert!(user.contains("CONVENTION_SENTINEL") && !user.contains("ADR_SENTINEL_9193"));
    assert!(
        seen.outcome
            .context_sources
            .iter()
            .all(|r| r.source != "spec_docs")
    );
}

/// #9193 amendment 8: `spec_docs` alone never reads CLAUDE.md.
#[serial_test::serial]
#[tokio::test]
async fn spec_docs_alone_never_fetches_claude_md() {
    let fetcher = FakeFetcher::with(&[
        ("CLAUDE.md", Ok(Some("CONVENTION_SENTINEL".into()))),
        (ADR, Ok(Some(ADR_TEXT.into()))),
    ]);
    let seen = run(Setup::new(spec_docs(), fetcher.clone())).await;
    assert_eq!(fetcher.paths(), [ADR]);
    assert!(!seen.reviewer_user().contains("CONVENTION_SENTINEL"));
    assert!(
        seen.outcome
            .context_sources
            .iter()
            .all(|r| r.source != "claude_md")
    );
}

// ── Criterion 4: failures omit the doc; the review completes ─────────────────

/// A completed review: findings judged, no pipeline error.
fn assert_completed(seen: &Seen) {
    assert!(
        seen.outcome.result.error.is_none(),
        "{:?}",
        seen.outcome.result.error
    );
    assert_ne!(seen.outcome.result.verdict, Verdict::Unknown);
}

/// #9193 criterion 4: a path absent at the head is `absent`, renders no
/// block, and the review completes.
#[serial_test::serial]
#[tokio::test]
async fn missing_doc_is_absent_and_review_completes() {
    let seen = run(Setup::new(spec_docs(), FakeFetcher::with(&[]))).await;
    assert_eq!(seen.item("spec_docs", ADR), SourceState::Absent);
    assert!(!seen.reviewer_user().contains("## Referenced docs"));
    assert_completed(&seen);
}

/// #9193 criterion 4, Fail-Open Check: a Contents API error is
/// `unavailable`, never read as text, and the review completes.
#[serial_test::serial]
#[tokio::test]
async fn fetch_500_is_unavailable_and_review_completes() {
    let err = DocFetchError::Api {
        status: 500,
        body: "ADR_SENTINEL_9193 in an error body".into(),
    };
    let seen = run(Setup::new(
        spec_docs(),
        FakeFetcher::with(&[(ADR, Err(err))]),
    ))
    .await;
    assert_eq!(seen.item("spec_docs", ADR), SourceState::Unavailable);
    assert_eq!(seen.row("spec_docs").state, SourceState::Unavailable);
    assert!(!seen.reviewer_user().contains("## Referenced docs"));
    assert_completed(&seen);
}

/// #9193 criterion 4, Fail-Open Check: a directory or an undecodable file is
/// `unavailable` and renders nothing.
#[serial_test::serial]
#[tokio::test]
async fn directory_path_and_binary_are_unavailable() {
    for err in [
        DocFetchError::Directory,
        DocFetchError::Undecodable("binary".into()),
    ] {
        let seen = run(Setup::new(
            spec_docs(),
            FakeFetcher::with(&[(ADR, Err(err))]),
        ))
        .await;
        assert_eq!(seen.item("spec_docs", ADR), SourceState::Unavailable);
        assert!(!seen.reviewer_user().contains("## Referenced docs"));
        assert_completed(&seen);
    }
}

/// #9193 criterion 7, Fail-Open Check: a failed search is a `discovery`
/// item, the row is `unavailable`, and the PR-body doc is still read.
#[serial_test::serial]
#[tokio::test]
async fn search_down_is_unavailable_and_explicit_paths_still_read() {
    let mut setup = Setup::new(spec_docs(), FakeFetcher::adr());
    setup.search.fail = true;
    let seen = run(setup).await;
    assert_eq!(
        seen.item("spec_docs", "discovery"),
        SourceState::Unavailable
    );
    assert_eq!(seen.row("spec_docs").state, SourceState::Unavailable);
    assert_eq!(seen.item("spec_docs", ADR), SourceState::Used);
    assert!(seen.reviewer_user().contains("ADR_SENTINEL_9193"));
    assert_completed(&seen);
}

/// #9193 amendment 5: a search hit adds a candidate after the body's paths;
/// its text comes from the head read, not the search snippet; an absolute
/// hit inside the index root is read by its repository path.
#[serial_test::serial]
#[tokio::test]
async fn search_hit_adds_candidate_and_text_comes_from_head() {
    let fetcher = FakeFetcher::with(&[
        (ADR, Ok(Some(ADR_TEXT.into()))),
        ("docs/specs/found.md", Ok(Some("SPEC_HEAD_TEXT".into()))),
        ("docs/specs/abs.md", Ok(Some("ABS_HEAD_TEXT".into()))),
    ]);
    let mut setup = Setup::new(spec_docs(), fetcher.clone());
    setup.search.hits = vec![
        ("docs/specs/found.md".into(), "SPEC_SNIPPET_STALE".into()),
        (
            "/srv/repo/docs/specs/abs.md".into(),
            "ABS_SNIPPET_STALE".into(),
        ),
    ];
    let seen = run(setup).await;
    assert_eq!(
        fetcher.paths(),
        [ADR, "docs/specs/found.md", "docs/specs/abs.md"]
    );
    let user = seen.reviewer_user();
    assert!(user.contains("SPEC_HEAD_TEXT") && user.contains("ABS_HEAD_TEXT"));
    assert!(!user.contains("SPEC_SNIPPET_STALE") && !user.contains("ABS_SNIPPET_STALE"));
}

/// #9193 amendment 5: a hit outside the doc allowlist, or outside the index
/// root, is never read.
#[serial_test::serial]
#[tokio::test]
async fn search_hit_outside_allowlist_is_ignored() {
    let fetcher = FakeFetcher::adr();
    let mut setup = Setup::new(spec_docs(), fetcher.clone());
    setup.search.hits = [
        "src/auth.rs",
        "README.md",
        "node_modules/a/docs/adr/x.md",
        "/etc/docs/adr/x.md",
    ]
    .iter()
    .map(|f| (f.to_string(), String::new()))
    .collect();
    run(setup).await;
    assert_eq!(fetcher.paths(), [ADR]);
}

// ── Head SHA and fork arms (Fail-Open Check) ─────────────────────────────────

/// #9193 amendment 2: a PR run whose metadata gave no head SHA reads
/// nothing and records the rows `unavailable`; the review completes.
#[serial_test::serial]
#[tokio::test]
async fn empty_head_sha_never_fetches() {
    let fetcher = FakeFetcher::adr();
    let mut setup = Setup::new(spec_docs().with_claude_md(true), fetcher.clone());
    setup.head.sha = String::new();
    let seen = run(setup).await;
    assert!(fetcher.calls().is_empty());
    for source in ["spec_docs", "claude_md"] {
        assert_eq!(seen.row(source).state, SourceState::Unavailable);
    }
    assert!(!seen.reviewer_user().contains("ADR_SENTINEL_9193"));
}

/// #9193 amendment 2: a malformed head SHA (a branch name, upper case, short)
/// is never used as a ref.
#[serial_test::serial]
#[tokio::test]
async fn malformed_sha_never_fetches() {
    for sha in ["main", &HEAD.to_uppercase(), &HEAD[..12]] {
        let fetcher = FakeFetcher::adr();
        let mut setup = Setup::new(spec_docs(), fetcher.clone());
        setup.head.sha = sha.to_string();
        let seen = run(setup).await;
        assert!(fetcher.calls().is_empty(), "{sha}");
        assert_eq!(seen.row("spec_docs").state, SourceState::Unavailable);
    }
}

/// #9193 amendment 6: a fork head whose commit the base repo cannot resolve
/// is `unavailable`, not `absent`: from the API's "no commit found" reply,
/// and from any 404 when the PR's head is in a fork.
#[serial_test::serial]
#[tokio::test]
async fn fork_head_sha_unresolvable_is_unavailable_not_absent() {
    let missing = DocFetchError::MissingCommit("No commit found for the ref".into());
    let seen = run(Setup::new(
        spec_docs(),
        FakeFetcher::with(&[(ADR, Err(missing))]),
    ))
    .await;
    assert_eq!(seen.item("spec_docs", ADR), SourceState::Unavailable);

    let mut setup = Setup::new(spec_docs(), FakeFetcher::with(&[]));
    setup.head.fork = true;
    let seen = run(setup).await;
    assert_eq!(seen.item("spec_docs", ADR), SourceState::Unavailable);
    assert_completed(&seen);
}

// ── Criterion 5 end to end ───────────────────────────────────────────────────

/// #9193 criterion 5: a valid `[doc:]` citation survives the gate, the
/// verifier and the post-verifier re-check at the head.
#[serial_test::serial]
#[tokio::test]
async fn doc_citation_survives_post_verifier_recheck() {
    let mut setup = Setup::new(spec_docs(), FakeFetcher::adr());
    setup.cite = doc_cite(ADR, &HEAD[..7], EXCERPT);
    let seen = run(setup).await;
    assert_eq!(
        seen.outcome.result.findings.len(),
        1,
        "{:?}",
        seen.outcome.result.withheld_findings
    );
    assert!(seen.outcome.result.withheld_findings.is_empty());
}

/// #9193 criterion 5: a fabricated `[doc:]` quote is withheld end to end.
#[serial_test::serial]
#[tokio::test]
async fn fabricated_doc_quote_is_withheld_end_to_end() {
    let mut setup = Setup::new(spec_docs(), FakeFetcher::adr());
    setup.cite = doc_cite(ADR, HEAD, "Invoice totals may wrap on overflow");
    let seen = run(setup).await;
    assert!(seen.outcome.result.findings.is_empty());
    assert_eq!(seen.outcome.result.withheld_findings.len(), 1);
}

/// #9193 amendment 1: doc text never enters the flat refs corpus, so a
/// `[gh:]` citation whose excerpt is only in a doc is withheld.
#[serial_test::serial]
#[tokio::test]
async fn doc_text_is_not_in_the_flat_refs_corpus() {
    let mut setup = Setup::new(spec_docs(), FakeFetcher::adr());
    setup.cite = format!("[gh: #42 — \\\"{EXCERPT}\\\"]");
    let seen = run(setup).await;
    assert!(
        seen.reviewer_user().contains(EXCERPT),
        "the reviewer saw the doc"
    );
    assert!(seen.outcome.result.findings.is_empty());
    assert_eq!(seen.outcome.result.withheld_findings.len(), 1);
}

/// #9193 Architect ruling Q8: a doc the PR itself changes is marked in its
/// heading and ledger item, and stays citable.
#[serial_test::serial]
#[tokio::test]
async fn self_edited_doc_is_marked_in_prompt_and_still_citable() {
    let mut setup = Setup::new(spec_docs(), FakeFetcher::adr());
    setup.diff = format!(
        "{}diff --git a/{ADR} b/{ADR}\n--- a/{ADR}\n+++ b/{ADR}\n@@ -0,0 +1 @@\n+ADR_SENTINEL_9193\n",
        billing_diff()
    );
    setup.cite = doc_cite(ADR, HEAD, EXCERPT);
    let seen = run(setup).await;
    assert!(
        seen.reviewer_user()
            .contains(&format!("### {ADR}@{HEAD} (this PR modifies this doc)"))
    );
    let item = seen
        .row("spec_docs")
        .items
        .iter()
        .find(|i| i.id == ADR)
        .cloned();
    assert_eq!(
        item.and_then(|i| i.detail).as_deref(),
        Some("this PR modifies this doc")
    );
    assert_eq!(
        seen.outcome.result.findings.len(),
        1,
        "{:?}",
        seen.outcome.result.withheld_findings
    );
}
