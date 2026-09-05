//! Filling a built [`ReportModel`](crate::report::model::ReportModel) from a live
//! analyze fetch, and naming every repository the fetch could not populate.
//!
//! Why: split out of `analyze_adapter.rs` when #6712 pushed that file past the
//! 500-SLOC cap. The seam it splits on is the one the module already had —
//! everything here walks the report MODEL, while what stays behind speaks to the
//! daemon and maps its JSON. `analyze_findings.rs` was carved off the same file
//! on the same principle.
//!
//! What: [`enrich_with_analyze`] and [`enrich_with_analyze_gaps`], moved
//! verbatim, plus [`enrich_with_analyze_outcome`], which returns the same lines
//! WITH the lane's own attempted/succeeded counts (#6811). All three are
//! re-exported from [`crate::report::analyze_adapter`], so every existing path
//! to them still resolves.
//!
//! Test: `analyze_adapter_tests.rs::{enrich_names_unreachable_repositories,
//! enrich_reports_no_gaps_when_every_repo_is_populated,
//! enrich_reports_caveats_for_partially_answered_repositories}`; end to end by
//! `tests/report_analyze_e2e.rs`.

use super::analyze_adapter::{AnalyzeCaveat, AnalyzeFetch, AnalyzeGap, AnalyzeMetricsSource};
use super::analyze_findings::relativize_components;
use super::index_registry::resolve_report_index;

/// How many repositories the analyze lane was attempted on, and how many it
/// populated (#6811).
///
/// Why: the per-reason gap lines name the repositories that failed but state no
/// denominator, so a report where the lane worked for 58 of 59 repositories and one
/// where it worked for NONE of them carry the same shape of line. In the field the
/// second case shipped: an estate-wide refusal (#6783) left every CAST health
/// factor reading "not stated in source data" in all 59 repositories, and
/// downstream readers concluded static analysis had run and found nothing. Per-repo
/// fail-open is still correct (DOC-67 §9); what was missing is the lane's own
/// outcome as a recorded fact.
/// What: two counters and [`Self::record`], which turns them into at most one
/// report line. A lane that populated every repository it attempted records
/// nothing — there is no degradation to state.
/// Test: `analyze_adapter_tests::{a_lane_that_failed_every_repository_is_recorded,
/// a_partly_degraded_lane_states_the_denominator,
/// a_fully_successful_lane_records_nothing}`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct AnalyzeLaneCoverage {
    /// Repositories eligible for the fetch, counted before it runs.
    pub attempted: usize,
    /// Repositories whose metrics were accepted onto the model.
    pub succeeded: usize,
}

impl AnalyzeLaneCoverage {
    /// A lane that never got as far as a fetch, over `attempted` eligible
    /// repositories (#6811).
    ///
    /// Why: the client-construction failure in
    /// [`crate::report::run`] is a total collapse with no walk behind it, and it
    /// has to reach the same decision point the walk's own record does — two
    /// spellings of "nothing was assessed" is how one of them ends up not
    /// failing the run.
    /// What: `succeeded` is zero by construction.
    /// Test: `run_tests::a_client_that_will_not_build_counts_every_eligible_repository`.
    #[must_use]
    pub fn never_ran(attempted: usize) -> Self {
        Self {
            attempted,
            succeeded: 0,
        }
    }

    /// Repositories the lane attempted and did not populate.
    #[must_use]
    pub fn failed(&self) -> usize {
        self.attempted.saturating_sub(self.succeeded)
    }

    /// True when the lane was attempted at least once and populated nothing
    /// (#6811).
    ///
    /// Why: this is the exact condition the issue's second closure condition
    /// names, and it is a predicate rather than an inline comparison so the
    /// `--allow-degraded` decision and the report line cannot drift apart. A
    /// lane attempted ZERO times is not a collapse — a manifest of remote-only
    /// repositories has no analyze lane to lose.
    /// What: `attempted > 0 && succeeded == 0`.
    /// Test: `analyze_adapter_tests::a_lane_that_failed_every_repository_is_recorded`,
    /// `run_tests::a_total_analyze_collapse_fails_the_run`.
    #[must_use]
    pub fn is_total_failure(&self) -> bool {
        self.attempted > 0 && self.succeeded == 0
    }

    /// True when the lane populated some but not all of what it attempted
    /// (#6811).
    ///
    /// Why: partial degradation stays a WARNING by default — the report renders,
    /// the coverage line states the denominator, and the run exits 0. That
    /// threshold is deliberate: a 58-of-59 run still carries 58 assessed
    /// applications, and failing it would throw away work over one unreachable
    /// index.
    /// What: `succeeded > 0 && succeeded < attempted`.
    /// Test: `analyze_adapter_tests::a_partly_degraded_lane_states_the_denominator`,
    /// `run_tests::a_partial_analyze_degradation_is_a_warning_not_a_failure`.
    #[must_use]
    pub fn is_partial_failure(&self) -> bool {
        self.succeeded > 0 && self.succeeded < self.attempted
    }

    /// The one Gaps & Caveats line this lane's outcome earns, if any.
    ///
    /// Why: a total collapse and a partial one need different words. "No
    /// application in this report was assessed" is a statement about the whole
    /// artifact and belongs at the head of the gap list; "3 of 59" is a coverage
    /// figure. Both state attempted/succeeded/failed explicitly, which is what
    /// separates unassessed from clean.
    /// What: `None` when nothing was attempted (nothing to say) or when every
    /// attempt succeeded; otherwise one sentence carrying all three counts.
    /// Test: see [`AnalyzeLaneCoverage`].
    fn record(&self) -> Option<String> {
        let failed = self.failed();
        if self.attempted == 0 || failed == 0 {
            return None;
        }
        if self.succeeded == 0 {
            return Some(format!(
                "trusty-analyze lane DID NOT RUN — 0 of {attempted} application(s) assessed, \
                 {failed} failed. No static-analysis pass contributed to this report, so every \
                 finding count, complexity figure, and health factor in it is UNASSESSED, not \
                 clean. Treat the report as covering the repository scan only (issue #6811).",
                attempted = self.attempted,
                failed = failed,
            ));
        }
        Some(format!(
            "trusty-analyze lane partially degraded — {succeeded} of {attempted} \
             application(s) assessed, {failed} failed. The failed applications' findings, \
             complexity, and health factors are unassessed, not clean (issue #6811).",
            succeeded = self.succeeded,
            attempted = self.attempted,
            failed = failed,
        ))
    }
}

/// Fill live analyze metrics into a built [`ReportModel`](crate::report::ReportModel), honouring the
/// fail-open precedence: declared metrics file > `--analyze` live fetch > None.
///
/// Why: `--analyze` must populate the complexity chart + finding bands for a
/// bare run, but must NEVER override a hand-authored metrics JSON, and must
/// never abort the report — an unindexed repo or an unreachable daemon simply
/// leaves the repo at its declared/scan state.
/// What: for each repository that has NO declared metrics AND is a local
/// checkout (remote repos are never indexed locally), derives the index id from
/// the checkout path and fetches via `source`; a `Some` result populates
/// `repo.metrics`. Repos with declared metrics or no local path are skipped.
/// Test: `report_analyze_e2e.rs` drives this against an in-process HTTP mock.
pub async fn enrich_with_analyze(
    model: &mut super::model::ReportModel,
    source: &dyn AnalyzeMetricsSource,
) {
    let _ = enrich_with_analyze_gaps(model, source).await;
}

/// Same enrichment, returning one Gaps & Caveats line per degraded condition
/// (#5239, DOC-67 §9).
///
/// Why: fail-open is the right contract and fail-SILENT is not. A findings
/// table that renders empty because the daemon was down is indistinguishable,
/// on the page, from a codebase with no findings — so every repository the
/// fetch could not populate is named, grouped by reason, in the report itself.
/// The fetch contract is unchanged: nothing here aborts, and the report still
/// renders from the built-in scan.
/// What: walks the same repositories [`enrich_with_analyze`] does, using
/// [`AnalyzeMetricsSource::fetch_named`]; returns at most one line per
/// [`AnalyzeGap`] kind and one per [`AnalyzeCaveat`] kind, each naming the
/// affected repositories in model order so two runs over the same state produce
/// identical lines. Repositories with a declared metrics file, and remote
/// entries, are skipped — neither is a gap. Returns an empty vec when every
/// eligible repository was populated completely.
/// Test: `analyze_adapter_tests.rs::{enrich_names_unreachable_repositories,
/// enrich_reports_no_gaps_when_every_repo_is_populated,
/// enrich_reports_caveats_for_partially_answered_repositories}`, plus
/// `redact_tests.rs::enrich_scrubs_configured_credentials_from_findings` for
/// the #5323 redaction boundary.
pub async fn enrich_with_analyze_gaps(
    model: &mut super::model::ReportModel,
    source: &dyn AnalyzeMetricsSource,
) -> Vec<String> {
    enrich_with_analyze_outcome(model, source).await.lines
}

/// What one analyze enrichment produced: the report lines, and the lane's own
/// attempted/succeeded counts (#6811).
///
/// Why: `enrich_with_analyze_gaps` returns prose, and a caller deciding whether
/// to FAIL the run cannot make that decision by matching on a sentence. The
/// counts the walk already keeps are what the decision needs, so they leave the
/// function as data rather than being re-derived from the line's wording.
/// What: `lines` is byte-identical to what `enrich_with_analyze_gaps` returns;
/// `coverage` is the same record that produced its first line.
/// Test: `analyze_adapter_tests::the_outcome_carries_the_lane_counts`.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct AnalyzeLaneOutcome {
    /// One Gaps & Caveats line per degraded condition, coverage line first.
    pub lines: Vec<String>,
    /// How many repositories the lane attempted, and how many it populated.
    pub coverage: AnalyzeLaneCoverage,
}

/// [`enrich_with_analyze_gaps`], returning the lane's counts alongside its lines
/// (#6811).
///
/// Why: the report pipeline must fail a run whose analyze lane assessed NOTHING
/// unless the operator passed `--allow-degraded`, and the only party that knows
/// the denominator is this walk. Handing the counts back is what lets the
/// decision live at the pipeline's one exit point instead of being inferred from
/// the text.
/// What: identical work and identical lines; the counts are the walk's own,
/// counted before each fetch and incremented on each accepted result.
/// Test: `analyze_adapter_tests::{a_lane_that_failed_every_repository_is_recorded,
/// a_partly_degraded_lane_states_the_denominator,
/// the_outcome_carries_the_lane_counts}`.
pub async fn enrich_with_analyze_outcome(
    model: &mut super::model::ReportModel,
    source: &dyn AnalyzeMetricsSource,
) -> AnalyzeLaneOutcome {
    // BTreeMap, not HashMap: the rendered line order must not depend on hash
    // iteration order (DOC-67 §9's determinism requirement).
    let mut missing: std::collections::BTreeMap<AnalyzeGap, Vec<String>> = Default::default();
    let mut partial: std::collections::BTreeMap<AnalyzeCaveat, Vec<String>> = Default::default();
    // #6137: one line per repository whose index described a different checkout.
    let mut stale: Vec<String> = Vec::new();
    // #6811: the lane's own outcome, counted while the walk runs. The per-reason
    // lines below name repositories but state no denominator, so a run where the
    // lane never worked at all read exactly like one where it mostly did.
    let mut coverage = AnalyzeLaneCoverage::default();

    // #5323: daemon-authored text lands in an acquirer-facing artifact, so it
    // crosses the redaction boundary before it reaches the model. Resolved once
    // per enrichment, not once per repository — it touches the filesystem.
    let secrets = super::redact::report_secrets();
    // #6677: one registry read for the whole walk — resolution needs the
    // daemon's `root_path` values, and they do not change mid-enrichment.
    let indexes = source.registered_indexes().await;

    for repo in &mut model.repositories {
        // Precedence: a declared metrics file always wins.
        if repo.metrics.is_some() {
            continue;
        }
        // Only local checkouts can be served by trusty-analyze/trusty-search.
        let Some(path) = repo.local_path.as_ref() else {
            continue;
        };
        // #6677: the derived id when the daemon holds it, otherwise the index
        // registered at this checkout's root_path; `None` only for a path that
        // derives to nothing, which is the skip this always made.
        let Some(index_id) = resolve_report_index(path, &indexes).into_id() else {
            continue;
        };
        // #6811: attempted is counted here, before the fetch, so a fetch that
        // panics-free-but-fails cannot leave the denominator behind.
        coverage.attempted += 1;
        match source.fetch_named(&index_id).await {
            AnalyzeFetch::Fetched {
                mut metrics,
                caveats,
            } => {
                super::redact::scrub_metrics(&mut metrics, &secrets);
                // #6082: the daemon reports absolute paths; the report cites
                // repository-relative ones everywhere else.
                relativize_components(&mut metrics, path);
                // #6137: an index addressed by directory basename can serve a
                // DIFFERENT checkout of the same repository. Data describing
                // another tree is stale-index evidence, never a measurement of
                // this one.
                match super::analyze_scope::accept(&repo.name, &index_id, path, *metrics) {
                    Ok(m) => {
                        coverage.succeeded += 1;
                        repo.metrics = Some(m);
                        for caveat in caveats {
                            partial.entry(caveat).or_default().push(repo.name.clone());
                        }
                    }
                    Err(gap) => {
                        // #6080: the investigation pass writes into the same
                        // `metrics` struct, so a section reporting an
                        // analyze-only figure needs this marker to tell a
                        // measurement from an artefact of that sharing.
                        repo.analyze_gap =
                            Some(super::analyze_scope::STALE_INDEX_REMEDY.to_string());
                        stale.push(gap);
                    }
                }
            }
            AnalyzeFetch::Missing(gap) => {
                repo.analyze_gap = Some(super::analyze_scope::NO_ANALYZE_DATA_REMEDY.to_string());
                missing.entry(gap).or_default().push(repo.name.clone());
            }
        }
    }

    // #6811: the lane's own record comes FIRST, so a reader meets the denominator
    // before the per-reason detail.
    let mut lines: Vec<String> = coverage.record().into_iter().collect();
    lines.extend(missing.into_iter().map(|(gap, repos)| {
        format!(
            "{} — no analysis pass ran for: {}. \
                 Those applications are described from the repository scan alone; \
                 their findings, complexity, and health factors are not assessed, \
                 not clean.",
            gap.as_str(),
            repos.join(", ")
        )
    }));
    lines.extend(
        partial
            .into_iter()
            .map(|(caveat, repos)| format!("{caveat} — affects: {}.", repos.join(", "))),
    );
    lines.extend(stale);
    AnalyzeLaneOutcome { lines, coverage }
}
