//! The context-source ledger through the real pipeline (#9194).
//!
//! Why: AC1-AC3 of #9194: reporting is opt-in and changes no prompt; when on,
//! every source has a row in canonical order; a source that returned nothing
//! reads `absent` and one that failed reads `unavailable`, so a
//! context-starved APPROVE is distinguishable from a fully-informed one.
//! What: drives `run_review_with` down the GitHub path with scripted search
//! and analyze clients that count their calls, and reads
//! `ReviewOutcome::context_sources`.
//! Test: this module.

use std::sync::atomic::{AtomicUsize, Ordering};

use super::optional_context_off::{
    BODY, FakePrSource, RecordingLlm, assert_golden, billing_diff, github_input, hermetic_config,
    legacy_caller, mapreduce_diff, observe, render_requests, render_result,
};
use super::*;
use crate::integrations::search_client::IndexStatusResponse;
use crate::models::{ContextSourceRecord, SourceState};
use crate::pipeline::optional_context::{
    ExternalSources, OptionalContextRequest, ledger::tests::CANONICAL,
};

// ── Fakes ────────────────────────────────────────────────────────────────────

/// A search client with a scripted health, index probe and query answer,
/// counting each call.
#[derive(Default)]
pub(super) struct ScriptSearch {
    pub(super) down: bool,
    pub(super) index_fails: bool,
    pub(super) hits: usize,
    pub(super) fail_status: Option<u16>,
    pub(super) health_calls: AtomicUsize,
    pub(super) status_calls: AtomicUsize,
    pub(super) queries: AtomicUsize,
}

impl ScriptSearch {
    pub(super) fn hits(hits: usize) -> Self {
        Self {
            hits,
            ..Self::default()
        }
    }

    pub(super) fn failing(status: u16) -> Self {
        Self {
            fail_status: Some(status),
            ..Self::default()
        }
    }
}

#[async_trait]
impl SearchClient for ScriptSearch {
    async fn index_status(&self, index_id: &str) -> Result<IndexStatusResponse, SearchClientError> {
        self.status_calls.fetch_add(1, Ordering::SeqCst);
        if self.index_fails {
            return Err(SearchClientError::Transport("connection reset".into()));
        }
        Ok(IndexStatusResponse::ready(index_id))
    }

    async fn health(&self) -> Result<HealthResponse, SearchClientError> {
        self.health_calls.fetch_add(1, Ordering::SeqCst);
        if self.down {
            return Err(SearchClientError::Unavailable("daemon down".into()));
        }
        FakeSearch.health().await
    }

    async fn list_indexes(&self) -> Result<Vec<IndexInfo>, SearchClientError> {
        Ok(Vec::new())
    }

    async fn search(
        &self,
        _: &str,
        _: &str,
        _: Option<u32>,
    ) -> Result<Vec<SearchResult>, SearchClientError> {
        self.queries.fetch_add(1, Ordering::SeqCst);
        if let Some(status) = self.fail_status {
            let body = if status == 404 {
                r#"{"error":"unknown index: main"}"#
            } else {
                "boom"
            };
            return Err(SearchClientError::Api {
                status,
                body: body.to_string(),
            });
        }
        Ok((0..self.hits)
            .map(|i| SearchResult {
                file: format!("src/hit{i}.rs"),
                snippet: Some("pub fn hit() {}".to_string()),
                score: 0.9,
                start_line: None,
                end_line: None,
            })
            .collect())
    }
}

/// An analyze client with scripted readiness (one answer per call, the last
/// repeating), hotspots and smells, counting each call.
pub(super) struct ScriptAnalyze {
    pub(super) ready: Vec<bool>,
    pub(super) hotspots: Result<Vec<&'static str>, &'static str>,
    pub(super) smells: Result<Vec<&'static str>, &'static str>,
    pub(super) ready_calls: AtomicUsize,
    pub(super) hotspot_calls: AtomicUsize,
    pub(super) smell_calls: AtomicUsize,
}

impl ScriptAnalyze {
    /// Ready, with nothing to report.
    pub(super) fn ready() -> Self {
        Self {
            ready: vec![true],
            hotspots: Ok(Vec::new()),
            smells: Ok(Vec::new()),
            ready_calls: AtomicUsize::new(0),
            hotspot_calls: AtomicUsize::new(0),
            smell_calls: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl AnalyzeClient for ScriptAnalyze {
    async fn health(&self) -> Result<AnalyzeHealthResponse, AnalyzeClientError> {
        ReadyAnalyze.health().await
    }

    async fn has_analysis(&self, _: &str) -> bool {
        let n = self.ready_calls.fetch_add(1, Ordering::SeqCst);
        *self.ready.get(n).or(self.ready.last()).unwrap_or(&false)
    }

    async fn complexity_hotspots(
        &self,
        _: &str,
        _: Option<u32>,
    ) -> Result<Vec<ComplexityHotspot>, AnalyzeClientError> {
        self.hotspot_calls.fetch_add(1, Ordering::SeqCst);
        let files = self
            .hotspots
            .clone()
            .map_err(|e| AnalyzeClientError::Transport(e.to_string()))?;
        Ok(files
            .into_iter()
            .map(|file| ComplexityHotspot {
                file: file.to_string(),
                function_name: None,
                cyclomatic: 30,
                cognitive: 30,
            })
            .collect())
    }

    async fn smells(&self, _: &str) -> Result<Vec<Smell>, AnalyzeClientError> {
        self.smell_calls.fetch_add(1, Ordering::SeqCst);
        let files = self
            .smells
            .clone()
            .map_err(|e| AnalyzeClientError::Transport(e.to_string()))?;
        Ok(files
            .into_iter()
            .map(|file| Smell {
                file: file.to_string(),
                category: "long_function".to_string(),
                severity: "warning".to_string(),
                line: None,
            })
            .collect())
    }
}

// ── Fixture run ──────────────────────────────────────────────────────────────

/// One review's inputs.
pub(super) struct Run {
    pub(super) search: ScriptSearch,
    pub(super) analyze: Option<ScriptAnalyze>,
    pub(super) request: OptionalContextRequest,
    pub(super) diff: String,
    pub(super) body: String,
    pub(super) config: ReviewConfig,
    /// The external sources the review gathers from (seam); `None` builds
    /// them from `config`, which [`hermetic_config`] turns all off.
    pub(super) external: Option<ExternalSources>,
    /// A diff source to review instead of the billing PR.
    pub(super) source: Option<DiffSource>,
}

/// What one review did.
pub(super) struct Ran {
    pub(super) outcome: ReviewOutcome,
    pub(super) search: Arc<ScriptSearch>,
    pub(super) analyze: Option<Arc<ScriptAnalyze>>,
    pub(super) reviewer: Arc<RecordingLlm>,
}

impl Run {
    /// The billing PR with `request`, healthy search (one hit), ready analyze,
    /// no external source, and both dependencies opted out of the skip.
    pub(super) fn new(request: OptionalContextRequest) -> Self {
        let mut config = hermetic_config();
        config.context.require_search = Some(false);
        config.context.require_analyze = false;
        Self {
            search: ScriptSearch::hits(1),
            analyze: Some(ScriptAnalyze::ready()),
            request,
            diff: billing_diff(),
            body: BODY.to_string(),
            config,
            external: None,
            source: None,
        }
    }

    pub(super) async fn go(self) -> Ran {
        let search = Arc::new(self.search);
        let analyze = self.analyze.map(Arc::new);
        let reviewer = RecordingLlm::new(Arc::new(FakeLlm::approves()));
        let deps = ReviewDeps {
            llm: reviewer.clone(),
            verifier: None,
            search: search.clone(),
            analyze: analyze
                .clone()
                .map(|a| a as Arc<dyn crate::integrations::analyze_client::AnalyzeClient>),
            dedup: None,
        };
        let mut options = ReviewOptions::new(self.request);
        options.pr_source = Some(Arc::new(FakePrSource::new(&self.body, &self.diff)));
        options.external_sources = self.external;
        let mut input = github_input(legacy_caller());
        if let Some(source) = self.source {
            input.diff_source = source;
        }
        let outcome = run_review_with(&self.config, input, deps, options).await;
        Ran {
            outcome,
            search,
            analyze,
            reviewer,
        }
    }
}

fn report() -> OptionalContextRequest {
    OptionalContextRequest::default().with_report_context(true)
}

/// The row named `source`.
pub(super) fn row<'a>(outcome: &'a ReviewOutcome, source: &str) -> &'a ContextSourceRecord {
    outcome
        .context_sources
        .iter()
        .find(|r| r.source == source)
        .unwrap_or_else(|| panic!("no `{source}` row in {:?}", outcome.context_sources))
}

pub(super) fn names(outcome: &ReviewOutcome) -> Vec<&str> {
    outcome
        .context_sources
        .iter()
        .map(|r| r.source.as_str())
        .collect()
}

fn detail(row: &ContextSourceRecord) -> &str {
    row.detail.as_deref().unwrap_or_default()
}

// ── AC1: opt-in, prompts unchanged ───────────────────────────────────────────

/// #9194 AC1: `report_context` alone changes no prompt and no result: the
/// recorded requests and the result match the #9192 goldens.
#[tokio::test]
async fn report_context_leaves_prompts_byte_identical() {
    let seen = observe(
        FakePrSource::new(BODY, &billing_diff()),
        legacy_caller(),
        ReviewOptions::new(report()),
    )
    .await;
    assert!(!seen.outcome.context_sources.is_empty(), "reporting is on");
    assert_golden(
        "unified-reviewer.txt",
        &render_requests(&seen.reviewer.requests()),
    );
    assert_golden(
        "unified-verifier.txt",
        &render_requests(&seen.verifier.requests()),
    );
    assert_golden("unified-result.json", &render_result(&seen.outcome.result));
}

/// #9194 AC1: a default review reports nothing.
#[tokio::test]
async fn default_review_has_no_context_sources_key() {
    let ran = Run::new(OptionalContextRequest::default()).go().await;
    assert!(ran.outcome.context_sources.is_empty());
}

/// #9194 AC1: any new input turns on every row, without `report_context`.
#[tokio::test]
async fn a_new_input_turns_on_every_row_without_report_context() {
    let request = OptionalContextRequest::default().with_pr_body(true);
    let ran = Run::new(request).go().await;
    assert_eq!(names(&ran.outcome), CANONICAL);
    assert_eq!(row(&ran.outcome, "pr_body").state, SourceState::Used);
}

/// #9194 AC2: with `report_context`, all ten rows in canonical order (#9195,
/// #9196: `symbol_context` after `analyze`).
#[tokio::test]
async fn report_context_lists_every_source_in_canonical_order() {
    let ran = Run::new(report()).go().await;
    let o = &ran.outcome;
    assert_eq!(names(o), CANONICAL);
    let states: Vec<SourceState> = o.context_sources.iter().map(|r| r.state).collect();
    assert_eq!(
        states,
        [
            SourceState::NotRequested,
            SourceState::Used,
            SourceState::NotRequested,
            SourceState::NotRequested,
            SourceState::NotRequested,
            SourceState::NotRequested, // #9195: `changed_files`
            SourceState::Used,
            SourceState::Absent,
            SourceState::NotRequested, // #9196: `symbol_context`
            SourceState::NotRequested,
        ]
    );
}

/// #9194 AC2: `pr_body` truncation keeps its counts in the full row set.
#[tokio::test]
async fn truncation_is_reported_with_chars_omitted() {
    let mut run = Run::new(OptionalContextRequest::default().with_pr_body(true));
    run.body = "z".repeat(64_001);
    let ran = run.go().await;
    assert_eq!(names(&ran.outcome), CANONICAL);
    let body = row(&ran.outcome, "pr_body");
    assert_eq!(
        (body.state, body.chars, body.chars_omitted),
        (SourceState::Truncated, 64_000, 1)
    );
}

// ── AC3: search ──────────────────────────────────────────────────────────────

/// #9194 S1: a failed query is `unavailable`, not `absent`.
#[tokio::test]
async fn search_failure_is_unavailable_not_absent() {
    let mut run = Run::new(report());
    run.search = ScriptSearch::failing(500);
    let ran = run.go().await;
    let search = row(&ran.outcome, "search");
    assert_eq!(search.state, SourceState::Unavailable);
    assert!(
        detail(search).starts_with("trusty-search query failed:"),
        "{search:?}"
    );
    assert!(detail(search).contains("500"), "{search:?}");
}

/// #9194 S2: a query with no hits is `absent`.
#[tokio::test]
async fn search_with_no_hits_is_absent() {
    let mut run = Run::new(report());
    run.search = ScriptSearch::hits(0);
    let ran = run.go().await;
    let search = row(&ran.outcome, "search");
    assert_eq!(
        (search.state, detail(search)),
        (SourceState::Absent, "no hits for the query")
    );
}

/// #9194 S3: a diff with no identifiers and no title sends no query.
#[tokio::test]
async fn search_with_an_empty_query_is_absent() {
    let diff = "diff --git a/README.md b/README.md\n--- a/README.md\n+++ b/README.md\n\
                @@ -1 +1 @@\n-old words\n+new words\n";
    let (source, _tmp) = local_diff_source(diff);
    let mut run = Run::new(report());
    run.config.search_index = "main".into();
    let search = Arc::new(run.search);
    let deps = ReviewDeps {
        search: search.clone(),
        ..ready_deps(Arc::new(FakeLlm::approves()), None)
    };
    let input = ReviewInput {
        diff_source: source,
        ..github_input(CallerContext::default())
    };
    let outcome = run_review_with(&run.config, input, deps, ReviewOptions::new(report())).await;
    assert_eq!(
        search.queries.load(Ordering::SeqCst),
        0,
        "no query was sent"
    );
    let row = row(&outcome, "search");
    assert_eq!(
        (row.state, detail(row)),
        (SourceState::Absent, "no identifiers or title to query")
    );
}

/// #9194 S4 (#8411): no index covers the checkout: `unavailable`.
#[tokio::test]
async fn search_with_no_index_is_unavailable() {
    let mut run = Run::new(report());
    run.config.search_index = String::new();
    let ran = run.go().await;
    let search = row(&ran.outcome, "search");
    assert_eq!(search.state, SourceState::Unavailable);
    assert!(
        detail(search).contains("covers this checkout"),
        "{search:?}"
    );
}

/// #9194 AC2: hits that arrived make the row `used`.
#[tokio::test]
async fn search_used_when_hits_arrive() {
    let ran = Run::new(report()).go().await;
    assert_eq!(row(&ran.outcome, "search").state, SourceState::Used);
}

/// #9194 S6 (ruling Q5): a degraded index marks `search` unavailable with
/// the gate's reason, although hits arrived.
#[tokio::test]
async fn degraded_gate_reason_marks_search_unavailable() {
    let mut run = Run::new(report());
    run.search.index_fails = true;
    let ran = run.go().await;
    assert_eq!(ran.search.queries.load(Ordering::SeqCst), 1, "hits arrived");
    let search = row(&ran.outcome, "search");
    assert_eq!(search.state, SourceState::Unavailable);
    assert!(detail(search).contains("could not be read"), "{search:?}");
    assert_eq!(ran.outcome.result.status, ReviewStatus::Degraded);
}

// ── AC3: analyze ─────────────────────────────────────────────────────────────

/// #9194 A1: no analyze client: `unavailable`.
#[tokio::test]
async fn analyze_without_a_client_is_unavailable() {
    let mut run = Run::new(report());
    run.analyze = None;
    let ran = run.go().await;
    let analyze = row(&ran.outcome, "analyze");
    assert_eq!(analyze.state, SourceState::Unavailable);
    assert!(
        detail(analyze).contains("analyze client absent"),
        "{analyze:?}"
    );
}

/// #9194 A2: no analysis for the index: `unavailable`, naming the index.
#[tokio::test]
async fn analyze_without_an_index_is_unavailable() {
    let mut run = Run::new(report());
    run.analyze = Some(ScriptAnalyze {
        ready: vec![false],
        ..ScriptAnalyze::ready()
    });
    let ran = run.go().await;
    let analyze = row(&ran.outcome, "analyze");
    assert_eq!(analyze.state, SourceState::Unavailable);
    assert!(
        detail(analyze).contains("no analysis for index `main`"),
        "{analyze:?}"
    );
}

/// #9194 A3: a failed hotspot call makes its item and the row `unavailable`.
#[tokio::test]
async fn analyze_hotspot_error_makes_the_row_unavailable() {
    let mut run = Run::new(report());
    run.analyze = Some(ScriptAnalyze {
        hotspots: Err("hotspot socket closed"),
        smells: Ok(vec!["src/billing.rs"]),
        ..ScriptAnalyze::ready()
    });
    let ran = run.go().await;
    let analyze = row(&ran.outcome, "analyze");
    assert_eq!(analyze.state, SourceState::Unavailable);
    let item = &analyze.items[0];
    assert_eq!(
        (item.id.as_str(), item.state),
        ("hotspots", SourceState::Unavailable)
    );
    let item_detail = item.detail.as_deref().unwrap_or_default();
    assert!(
        item_detail.starts_with("complexity_hotspots failed:"),
        "{item:?}"
    );
    assert!(item_detail.contains("hotspot socket closed"), "{item:?}");
    assert_eq!(analyze.items[1].state, SourceState::Used);
}

/// #9194 A4: a failed smells call makes its item and the row `unavailable`.
#[tokio::test]
async fn analyze_smells_error_makes_the_row_unavailable() {
    let mut run = Run::new(report());
    run.analyze = Some(ScriptAnalyze {
        smells: Err("smell socket closed"),
        ..ScriptAnalyze::ready()
    });
    let ran = run.go().await;
    let analyze = row(&ran.outcome, "analyze");
    assert_eq!(analyze.state, SourceState::Unavailable);
    let smells = &analyze.items[1];
    assert_eq!(
        (smells.id.as_str(), smells.state),
        ("smells", SourceState::Unavailable)
    );
    assert!(
        smells
            .detail
            .as_deref()
            .unwrap_or_default()
            .starts_with("smells failed:"),
        "{smells:?}"
    );
}

/// #9194 A5: both calls answered, nothing in the changed files: `absent`.
#[tokio::test]
async fn analyze_with_nothing_in_the_changed_files_is_absent() {
    let mut run = Run::new(report());
    run.analyze = Some(ScriptAnalyze {
        hotspots: Ok(vec!["src/elsewhere.rs"]),
        smells: Ok(vec!["src/elsewhere.rs"]),
        ..ScriptAnalyze::ready()
    });
    let ran = run.go().await;
    let analyze = row(&ran.outcome, "analyze");
    assert_eq!(
        (analyze.state, detail(analyze)),
        (
            SourceState::Absent,
            "no hotspots or smells in the changed files"
        )
    );
}

/// #9194 AC2: a hotspot in a changed file makes the row `used`.
#[tokio::test]
async fn analyze_used_when_hotspots_are_in_the_changed_files() {
    let mut run = Run::new(report());
    run.analyze = Some(ScriptAnalyze {
        hotspots: Ok(vec!["src/billing.rs"]),
        ..ScriptAnalyze::ready()
    });
    let ran = run.go().await;
    let analyze = row(&ran.outcome, "analyze");
    assert_eq!(analyze.state, SourceState::Used);
    assert_eq!(analyze.items[0].state, SourceState::Used);
    assert_eq!(analyze.items[1].state, SourceState::Absent);
}

/// #9194 A6 (ruling Q5): the gate found analyze unavailable; the row says
/// so with the gate's reason, although a later probe answered.
#[tokio::test]
async fn degraded_gate_reason_marks_analyze_unavailable() {
    let mut run = Run::new(report());
    run.analyze = Some(ScriptAnalyze {
        ready: vec![false, true],
        hotspots: Ok(vec!["src/billing.rs"]),
        ..ScriptAnalyze::ready()
    });
    let ran = run.go().await;
    let analyze = row(&ran.outcome, "analyze");
    assert_eq!(analyze.state, SourceState::Unavailable);
    assert!(
        detail(analyze).starts_with("trusty-analyze unavailable"),
        "{analyze:?}"
    );
}

/// #9194 amendment 6: search down and no analyze client give exactly one
/// `analyze` row, `unavailable`.
#[tokio::test]
async fn search_down_without_an_analyze_client_reports_one_analyze_row() {
    let mut run = Run::new(report());
    run.search.down = true;
    run.analyze = None;
    let ran = run.go().await;
    let rows: Vec<&ContextSourceRecord> = ran
        .outcome
        .context_sources
        .iter()
        .filter(|r| r.source == "analyze")
        .collect();
    assert_eq!(rows.len(), 1, "{:?}", ran.outcome.context_sources);
    assert_eq!(rows[0].state, SourceState::Unavailable);
    assert!(
        detail(rows[0]).contains("analyze client absent"),
        "{:?}",
        rows[0]
    );
    assert_eq!(row(&ran.outcome, "search").state, SourceState::Unavailable);
}

// ── AC3: a starved APPROVE ───────────────────────────────────────────────────

fn verdict_of(outcome: &ReviewOutcome) -> serde_json::Value {
    let r = &outcome.result;
    serde_json::json!([r.verdict, r.grade, r.status, r.findings, r.withheld_count])
}

/// #9194 AC3: search health is fine but the query fails. The APPROVE is the
/// healthy run's APPROVE; only the ledger tells them apart.
#[tokio::test]
async fn a_starved_approve_is_distinguishable() {
    let healthy = Run::new(report()).go().await;
    let mut starved = Run::new(report());
    starved.search = ScriptSearch::failing(503);
    let starved = starved.go().await;
    assert_eq!(verdict_of(&starved.outcome), verdict_of(&healthy.outcome));
    assert_eq!(starved.outcome.result.status, ReviewStatus::Completed);
    assert_eq!(row(&healthy.outcome, "search").state, SourceState::Used);
    assert_eq!(
        row(&starved.outcome, "search").state,
        SourceState::Unavailable
    );
}

/// #9194 AC2/AC3: a down search daemon (opted out) keeps the existing
/// `degraded` status — no new status value — and reads `unavailable`.
#[tokio::test]
async fn a_degraded_starved_approve_keeps_status_degraded_and_no_new_status() {
    let mut run = Run::new(report());
    run.search.down = true;
    let ran = run.go().await;
    let status = serde_json::to_value(ran.outcome.result.status).expect("status serialises");
    assert_eq!(status, serde_json::json!("degraded"));
    let search = row(&ran.outcome, "search");
    assert_eq!(search.state, SourceState::Unavailable);
    assert!(detail(search).contains("daemon down"), "{search:?}");
}

// ── Guards ───────────────────────────────────────────────────────────────────

/// #9194 R2: reporting never changes the verdict, grade, findings or the
/// citations the gate kept or withheld.
#[tokio::test]
async fn report_context_does_not_change_verdict_grade_findings_or_citations() {
    let off = observe(
        FakePrSource::new(BODY, &billing_diff()),
        legacy_caller(),
        ReviewOptions::default(),
    )
    .await;
    let on = observe(
        FakePrSource::new(BODY, &billing_diff()),
        legacy_caller(),
        ReviewOptions::new(report()),
    )
    .await;
    assert_eq!(
        render_result(&on.outcome.result),
        render_result(&off.outcome.result)
    );
    assert_eq!(
        on.outcome.result.findings.len(),
        1,
        "the cited finding survives"
    );
}

/// #9194: reporting makes no extra search, analyze or PR call: the rows come
/// from the calls the review already made.
#[tokio::test]
async fn report_context_adds_no_search_analyze_or_source_calls() {
    let counts = |ran: &Ran| {
        let a = ran.analyze.as_ref().expect("analyze");
        [
            ran.search.health_calls.load(Ordering::SeqCst),
            ran.search.status_calls.load(Ordering::SeqCst),
            ran.search.queries.load(Ordering::SeqCst),
            a.ready_calls.load(Ordering::SeqCst),
            a.hotspot_calls.load(Ordering::SeqCst),
            a.smell_calls.load(Ordering::SeqCst),
            ran.reviewer.requests().len(),
        ]
    };
    let off = Run::new(OptionalContextRequest::default()).go().await;
    let on = Run::new(report()).go().await;
    assert_eq!(counts(&on), counts(&off));
    assert_eq!(counts(&off), [1, 1, 1, 2, 1, 1, 1]);
}

/// #9194 S5/P1 (ruling Q6): a review that stops before its context is
/// gathered reports no rows, and fabricates none.
#[tokio::test]
async fn unknown_index_skip_reports_no_context_rows() {
    let mut run = Run::new(report());
    run.search = ScriptSearch::failing(404);
    let ran = run.go().await;
    assert!(
        ran.outcome.result.status.is_skipped(),
        "{:?}",
        ran.outcome.result
    );
    assert!(ran.outcome.context_sources.is_empty());
}

/// #9194 P4: a map-reduce review reports the same rows as a unified one.
#[tokio::test]
async fn mapreduce_review_reports_the_same_rows() {
    let unified = Run::new(report()).go().await;
    let mut run = Run::new(report());
    run.diff = mapreduce_diff();
    let mapreduce = run.go().await;
    assert!(
        mapreduce
            .outcome
            .result
            .review_body
            .contains("Map-reduce review:"),
        "this fixture must take the map-reduce path"
    );
    let shape = |o: &ReviewOutcome| -> Vec<(String, SourceState)> {
        o.context_sources
            .iter()
            .map(|r| (r.source.clone(), r.state))
            .collect()
    };
    assert_eq!(names(&mapreduce.outcome), CANONICAL);
    assert_eq!(shape(&mapreduce.outcome), shape(&unified.outcome));
}
