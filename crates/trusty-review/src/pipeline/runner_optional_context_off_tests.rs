//! With every optional input off, a review is byte-identical (#9192).
//!
//! Why: epic #9191 adds inputs a caller must ask for. A caller that asks for
//! none must see exactly the review it saw before, on both review paths.
//! What: drives the real pipeline down the GitHub path through a fake
//! [`PrSource`] whose PR body is non-empty, records every reviewer and
//! verifier request, and compares the prompts, the verifier text, the refs
//! corpus and the `ReviewResult` JSON with goldens captured before any
//! behaviour change. `UPDATE_GOLDEN=1` rewrites the goldens.
//! Test: `off_is_byte_identical_unified`, `off_is_byte_identical_mapreduce`,
//! `off_refs_corpus_is_byte_identical`, `off_keeps_the_raw_body_in_the_refs_corpus`,
//! `off_makes_no_extra_calls`.

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::integrations::github::GithubError;
use crate::integrations::search_client::IndexStatusResponse;
use crate::pipeline::optional_context::{PrSource, assemble::refs_for_gate};
use crate::pipeline::prompt::ReviewPrMeta;

/// The fixture PR body. `#42` and the excerpt are only here, so a citation of
/// them resolves only while the raw body is in the refs corpus.
pub(super) const BODY: &str =
    "Adds an invoice total.\n\nFollows up #42: overflow fixed by checked_add in billing.";
/// The fixture head SHA the fake PR source reports.
pub(super) const HEAD_SHA: &str = "0123456789abcdef0123456789abcdef01234567";
/// The fixture PR title.
pub(super) const TITLE: &str = "Add an invoice total";

// ── Fakes ────────────────────────────────────────────────────────────────────

/// A [`PrSource`] with fixed answers that counts its calls.
pub(super) struct FakePrSource {
    pub(super) body: String,
    pub(super) diff: String,
    pub(super) fail_meta: bool,
    pub(super) meta_calls: AtomicUsize,
    pub(super) diff_calls: AtomicUsize,
}

impl FakePrSource {
    pub(super) fn new(body: &str, diff: &str) -> Self {
        Self {
            body: body.to_string(),
            diff: diff.to_string(),
            fail_meta: false,
            meta_calls: AtomicUsize::new(0),
            diff_calls: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl PrSource for FakePrSource {
    async fn meta(
        &self,
        _config: &ReviewConfig,
        owner: &str,
        repo: &str,
        pr: u64,
        _run_mode: RunMode,
    ) -> Result<(ReviewPrMeta, String), GithubError> {
        self.meta_calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_meta {
            return Err(GithubError::Transport(
                "fixture: metadata unreachable".into(),
            ));
        }
        let meta = ReviewPrMeta {
            title: TITLE.to_string(),
            body: self.body.clone(),
            author: "octocat".to_string(),
            url: format!("https://github.com/{owner}/{repo}/pull/{pr}"),
        };
        Ok((meta, HEAD_SHA.to_string()))
    }

    async fn diff(&self, _: &str, _: &str, _: u64, _: &str) -> Result<String, GithubError> {
        self.diff_calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.diff.clone())
    }
}

/// Wraps a provider and records each request's system and user text.
pub(super) struct RecordingLlm {
    inner: Arc<dyn LlmProvider>,
    seen: Mutex<Vec<(String, String)>>,
}

impl RecordingLlm {
    pub(super) fn new(inner: Arc<dyn LlmProvider>) -> Arc<Self> {
        Arc::new(Self {
            inner,
            seen: Mutex::new(Vec::new()),
        })
    }

    /// Every recorded `(system, user)` pair, sorted (map calls run concurrently).
    pub(super) fn requests(&self) -> Vec<(String, String)> {
        let mut seen = self.seen.lock().expect("lock").clone();
        seen.sort();
        seen
    }
}

#[async_trait]
impl LlmProvider for RecordingLlm {
    fn name(&self) -> &str {
        self.inner.name()
    }

    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        let user = req
            .messages
            .iter()
            .map(|m| m.content.clone())
            .collect::<Vec<_>>()
            .join("\n");
        self.seen
            .lock()
            .expect("lock")
            .push((req.system.clone(), user));
        self.inner.complete(req).await
    }
}

/// [`FakeSearch`] that records every `search` query.
#[derive(Default)]
pub(super) struct CountingSearch {
    pub(super) queries: Mutex<Vec<String>>,
}

#[async_trait]
impl SearchClient for CountingSearch {
    async fn index_status(&self, index_id: &str) -> Result<IndexStatusResponse, SearchClientError> {
        FakeSearch.index_status(index_id).await
    }

    async fn health(&self) -> Result<HealthResponse, SearchClientError> {
        FakeSearch.health().await
    }

    async fn list_indexes(&self) -> Result<Vec<IndexInfo>, SearchClientError> {
        FakeSearch.list_indexes().await
    }

    async fn search(
        &self,
        index_id: &str,
        query: &str,
        top_k: Option<u32>,
    ) -> Result<Vec<SearchResult>, SearchClientError> {
        self.queries.lock().expect("lock").push(query.to_string());
        FakeSearch.search(index_id, query, top_k).await
    }
}

/// The reviewer: one grounded finding citing `#42` for the billing file, and
/// an empty APPROVE for every other prompt.
pub(super) struct BillingReviewer;

/// The finding's context citation; its excerpt is a sentence of [`BODY`].
pub(super) const GH_CITATION: &str = "[gh: #42 — \\\"overflow fixed by checked_add in billing\\\"]";

#[async_trait]
impl LlmProvider for BillingReviewer {
    fn name(&self) -> &str {
        "billing-reviewer"
    }

    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        let user: String = req.messages.iter().map(|m| m.content.as_str()).collect();
        let text = if user.contains("+++ b/src/billing.rs") {
            format!(
                "Overflow risk at src/billing.rs:10.\n\n```json\n{{\"verdict\":\"REQUEST_CHANGES\",\
                 \"grade\":\"C\",\"summary\":\"Overflow risk.\",\"findings\":[{{\"title\":\"overflow\",\
                 \"body\":\"`amounts.iter().sum::<u64>()` can overflow {GH_CITATION}.\",\
                 \"severity\":\"medium\",\"confidence\":0.9,\"file\":\"src/billing.rs\",\"line\":10}}]}}\n```"
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

// ── Fixture run ──────────────────────────────────────────────────────────────

/// The billing diff from the citation corpus (#9188).
pub(super) fn billing_diff() -> String {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/citation_corpus/billing.diff"
    );
    std::fs::read_to_string(path).expect("billing.diff is readable")
}

/// [`billing_diff`] plus enough one-line files to route the review to the
/// map-reduce path (more than the default 12-file threshold).
pub(super) fn mapreduce_diff() -> String {
    let mut diff = billing_diff();
    for i in 0..12 {
        diff.push_str(&format!(
            "diff --git a/src/f{i}.rs b/src/f{i}.rs\n--- a/src/f{i}.rs\n+++ b/src/f{i}.rs\n\
             @@ -0,0 +1 @@\n+pub fn f{i}() {{}}\n"
        ));
    }
    diff
}

/// Caller context through the legacy `CallerContext` route.
pub(super) fn legacy_caller() -> CallerContext {
    CallerContext {
        pr_description: Some("Caller note: totals are summed in cents.".to_string()),
        pr_discussion: Some(
            "Reviewer asked about overflow; author says inputs are bounded.".into(),
        ),
        referenced_code: Some("fn step_2(input: u64) -> u64 { input }".to_string()),
    }
}

/// A config with no external context source, so no fixture run dials out.
pub(super) fn hermetic_config() -> ReviewConfig {
    let mut config = ReviewConfig::from_env_and_file(None, None);
    let off = crate::integrations::context::SourceConfig {
        enabled: Some(false),
        ..Default::default()
    };
    let sources = &mut config.context_sources;
    sources.jira = off.clone();
    sources.confluence = off.clone();
    sources.github_issues = off.clone();
    sources.pr_history = off.clone();
    sources.conformance.base = off;
    config
}

/// A GitHub-path input for `acme/billing#7` that can never post.
pub(super) fn github_input(caller: CallerContext) -> ReviewInput {
    ReviewInput {
        diff_source: DiffSource::Github {
            owner: "acme".to_string(),
            repo: "billing".to_string(),
            pr: 7,
            token: "fixture-token".to_string(),
        },
        reviewer_model: "stub/golden-reviewer".to_string(),
        write_log: false,
        print_result: false,
        trigger: TriggerDecision::ForceDryRun,
        run_mode: RunMode::Cli,
        allow_posting: false,
        caller_context: caller,
        surface: InvocationSurface::Interactive,
    }
}

/// Everything one fixture run observed.
pub(super) struct Observed {
    pub(super) outcome: ReviewOutcome,
    pub(super) reviewer: Arc<RecordingLlm>,
    pub(super) verifier: Arc<RecordingLlm>,
    pub(super) search: Arc<CountingSearch>,
    pub(super) source: Arc<FakePrSource>,
}

/// Run the pipeline over `source` with `options` and record what it did.
pub(super) async fn observe(
    source: FakePrSource,
    caller: CallerContext,
    mut options: ReviewOptions,
) -> Observed {
    let source = Arc::new(source);
    let reviewer = RecordingLlm::new(Arc::new(BillingReviewer));
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

// ── Golden rendering ─────────────────────────────────────────────────────────

/// Recorded requests as text: each distinct system prompt once, then each
/// request's user text naming the system prompt it carried.
pub(super) fn render_requests(requests: &[(String, String)]) -> String {
    let mut systems: Vec<&str> = Vec::new();
    for (system, _) in requests {
        if !systems.contains(&system.as_str()) {
            systems.push(system);
        }
    }
    let mut out = String::new();
    for (i, system) in systems.iter().enumerate() {
        out.push_str(&format!("===== system {i} =====\n{system}\n"));
    }
    for (i, (system, user)) in requests.iter().enumerate() {
        let at = systems.iter().position(|s| s == system).unwrap_or(0);
        out.push_str(&format!("===== request {i} (system {at}) =====\n{user}\n"));
    }
    out
}

/// The result's JSON with the two run-dependent fields pinned.
pub(super) fn render_result(result: &ReviewResult) -> String {
    let mut value = serde_json::to_value(result).expect("result serialises");
    if let Some(obj) = value.as_object_mut() {
        obj.insert("timestamp".into(), "<timestamp>".into());
        obj.insert("review_version".into(), "<review_version>".into());
    }
    serde_json::to_string_pretty(&value).expect("value serialises") + "\n"
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/optional_context_off")
        .join(name)
}

/// Compare `actual` with the golden `name`, or rewrite it under `UPDATE_GOLDEN`.
pub(super) fn assert_golden(name: &str, actual: &str) {
    let path = golden_path(name);
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(path.parent().expect("golden dir")).expect("mkdir golden");
        std::fs::write(&path, actual).expect("write golden");
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e} (UPDATE_GOLDEN=1 creates it)", path.display()));
    if expected != actual {
        let line = expected
            .lines()
            .zip(actual.lines())
            .position(|(a, b)| a != b)
            .unwrap_or_else(|| expected.lines().count().min(actual.lines().count()));
        panic!(
            "{name} is not byte-identical to its golden: first difference at line {}\n\
             golden: {:?}\nactual: {:?}",
            line + 1,
            expected.lines().nth(line),
            actual.lines().nth(line),
        );
    }
}

fn assert_run_golden(prefix: &str, seen: &Observed) {
    let reviewer = seen.reviewer.requests();
    let verifier = seen.verifier.requests();
    assert!(!reviewer.is_empty(), "the reviewer must be called");
    assert!(!verifier.is_empty(), "the verifier must be called");
    assert_golden(
        &format!("{prefix}-reviewer.txt"),
        &render_requests(&reviewer),
    );
    assert_golden(
        &format!("{prefix}-verifier.txt"),
        &render_requests(&verifier),
    );
    assert_golden(
        &format!("{prefix}-result.json"),
        &render_result(&seen.outcome.result),
    );
}

// ── Tests ────────────────────────────────────────────────────────────────────

/// #9192: the unified path, every new input off, matches its goldens.
#[tokio::test]
async fn off_is_byte_identical_unified() {
    let source = FakePrSource::new(BODY, &billing_diff());
    let seen = observe(source, legacy_caller(), ReviewOptions::default()).await;
    assert!(
        !seen
            .outcome
            .result
            .review_body
            .contains("Map-reduce review:"),
        "this fixture must take the unified path"
    );
    assert_run_golden("unified", &seen);
}

/// #9192: the map-reduce path, every new input off, matches its goldens.
#[tokio::test]
async fn off_is_byte_identical_mapreduce() {
    let source = FakePrSource::new(BODY, &mapreduce_diff());
    let seen = observe(source, legacy_caller(), ReviewOptions::default()).await;
    assert!(
        seen.outcome
            .result
            .review_body
            .contains("Map-reduce review:"),
        "this fixture must take the map-reduce path"
    );
    assert_run_golden("mapreduce", &seen);
}

/// #9192: the refs corpus for the fixture PR matches its golden.
#[test]
fn off_refs_corpus_is_byte_identical() {
    let caller = legacy_caller();
    let refs = refs_for_gate(
        TITLE,
        Some(BODY),
        "",
        [
            caller.pr_description.as_deref(),
            caller.pr_discussion.as_deref(),
            caller.referenced_code.as_deref(),
        ],
    );
    assert_golden("refs-corpus.txt", &refs);
}

/// #9192: with every input off, a citation of the raw PR body still resolves,
/// although the reviewer never saw the body (the kept legacy asymmetry).
#[tokio::test]
async fn off_keeps_the_raw_body_in_the_refs_corpus() {
    let source = FakePrSource::new(BODY, &billing_diff());
    let seen = observe(source, CallerContext::default(), ReviewOptions::default()).await;
    let result = &seen.outcome.result;
    assert_eq!(
        result.findings.len(),
        1,
        "the finding citing the PR body must survive: {:?}",
        result.withheld_findings
    );
    assert!(result.findings[0].description.contains("#42"));
}

/// #9192: with every input off, the run makes exactly the calls it made before.
#[tokio::test]
async fn off_makes_no_extra_calls() {
    let source = FakePrSource::new(BODY, &billing_diff());
    let seen = observe(source, legacy_caller(), ReviewOptions::default()).await;
    assert_eq!(seen.source.meta_calls.load(Ordering::SeqCst), 1);
    assert_eq!(seen.source.diff_calls.load(Ordering::SeqCst), 1);
    assert_eq!(seen.search.queries.lock().expect("lock").len(), 1);
    assert_eq!(seen.reviewer.requests().len(), 1);
    assert_eq!(seen.verifier.requests().len(), 1);
}
