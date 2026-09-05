//! The `--analyze` lane's half of the report pipeline (#6811).
//!
//! Why: split out of `run.rs` when #6811 pushed that file past the 500-SLOC cap.
//! The seam is the one the pipeline already had — everything here concerns ONE
//! optional lane (its per-request budget, its fetch, and whether its outcome may
//! ship), while what stays behind orchestrates the run as a whole.
//! `analyze_enrich.rs` was carved off `analyze_adapter.rs` on the same
//! principle.
//!
//! What: [`resolve_analyze_budget`], [`enrich_from_analyze`],
//! [`analyze_eligible_repositories`] and [`analyze_lane_verdict`], moved
//! verbatim apart from the imports. All four are `pub(super)` — `run.rs` is the
//! only caller, and the decision they encode is the pipeline's, not a public
//! API.
//!
//! Test: `run_tests::{a_total_analyze_collapse_fails_the_run,
//! allow_degraded_lets_a_total_collapse_ship,
//! a_partial_analyze_degradation_is_a_warning_not_a_failure,
//! an_unattempted_analyze_lane_is_not_a_failure,
//! a_client_that_will_not_build_counts_every_eligible_repository,
//! a_dead_analyze_client_reads_as_a_dead_lane_downstream,
//! the_request_analyze_timeout_beats_the_manifest}`.

use anyhow::Result;

use crate::config::ReviewConfig;
use crate::report::manifest::Manifest;
use crate::report::model::ReportModel;

use super::run::ReportRequest;

/// The per-request budget the corpus-scanning analyze endpoints get (#6712).
///
/// Why: a free function so the precedence is tested directly rather than
/// restated inside the fetch, exactly as `resolve_code_only_from` is.
/// What: the request's value, else the manifest's, else the default; a `0` at
/// either tier reads as absent, since zero would time out every request.
/// Test: `run_tests::the_request_analyze_timeout_beats_the_manifest`.
pub(super) fn resolve_analyze_budget(
    req: &ReportRequest,
    manifest: &Manifest,
) -> std::time::Duration {
    crate::report::analyze_endpoints::corpus_budget_from_secs(
        req.analyze_timeout_secs
            .or(manifest.report.analyze_timeout_secs),
    )
}

/// Run the opt-in deterministic analyze fetch, recording what it could not
/// reach (#2445, #5239).
///
/// #6712: the corpus-scanning endpoints take their per-request budget from
/// `--analyze-timeout-secs`, else the manifest's `[report].analyze_timeout_secs`,
/// else the default — the same request-beats-manifest precedence `--code-only`
/// and `--no-mermaid` already follow.
/// Test: `run_tests::the_request_analyze_timeout_beats_the_manifest`.
pub(super) async fn enrich_from_analyze(
    model: &mut ReportModel,
    req: &ReportRequest,
    manifest: &Manifest,
    config: &ReviewConfig,
) -> crate::report::AnalyzeLaneCoverage {
    let analyze_socket = manifest
        .report
        .analyze_socket
        .clone()
        .map_or_else(|| config.analyzer_socket.clone(), std::path::PathBuf::from);
    let corpus_budget = resolve_analyze_budget(req, manifest);
    eprintln!(
        "[trusty-review report] --analyze: fetching over {} (corpus endpoints allowed {corpus_budget:?} each)",
        analyze_socket.display()
    );
    match crate::report::HttpAnalyzeMetricsSource::new(analyze_socket) {
        Ok(source) => {
            let source = source.with_corpus_budget(corpus_budget);
            // #5239: every repo the fetch could not populate is named in the
            // report, not only warned about on stderr — a dimension missing
            // because the daemon was down must not read as a clean pass.
            let outcome = crate::report::enrich_with_analyze_outcome(model, &source).await;
            for gap in &outcome.lines {
                eprintln!("[trusty-review report] --analyze gap: {gap}");
            }
            model.gaps.extend(outcome.lines);
            outcome.coverage
        }
        Err(e) => {
            eprintln!(
                "[trusty-review report] --analyze: could not build HTTP client ({e}); \
                 falling back to scan"
            );
            // #5239: the client never existed, so no repo was assessed — that is
            // a whole-report gap, not a per-repo one.
            // #6784: it leads with the shared headline, so the bundle index
            // counts it as the dead lane it is.
            model.gaps.push(client_build_failure_gap());
            // #6811: no client means no repository was assessed, which is the
            // same total collapse the walk reports — counted over the
            // repositories the walk WOULD have attempted, so the verdict below
            // sees one condition rather than two.
            crate::report::AnalyzeLaneCoverage::never_ran(analyze_eligible_repositories(model))
        }
    }
}

/// The Gaps & Caveats line a client that would not build earns (#6784).
///
/// Why: this is the second of `trusty-review`'s two total-collapse paths, and
/// it used to lead with "trusty-analyze data unavailable" while the other led
/// with the phrase `trusty-audit`'s bundle index matches on. Under
/// `--allow-degraded` a client-build collapse therefore shipped a report the
/// index counted as a lane that RAN. Both paths now lead with the same shared
/// headline, and the detail that distinguishes them follows it.
/// It is a named function rather than an inline `format!` so the regression test
/// can assert on the string this path actually produces rather than on a copy of
/// it.
/// What: [`trusty_common::review_gap_contract::ANALYZE_LANE_DEAD_HEADLINE`],
/// then what this particular collapse was.
/// Test: `run_tests::a_dead_analyze_client_reads_as_a_dead_lane_downstream`.
pub(super) fn client_build_failure_gap() -> String {
    format!(
        "{headline} — the analysis client could not be built, so no application in this report \
         was assessed against trusty-analyze. Findings, complexity, and health factors are not \
         assessed, not clean (issue #6784).",
        headline = trusty_common::review_gap_contract::ANALYZE_LANE_DEAD_HEADLINE,
    )
}

/// Repositories the analyze walk would attempt, counted without walking (#6811).
///
/// Why: the client-construction failure arm never reaches
/// `enrich_with_analyze_outcome`, so it has no denominator of its own — and a
/// total collapse with no denominator cannot be told from a manifest that had
/// nothing to assess. This applies the walk's own eligibility rule minus the
/// index-registry lookup a dead client cannot perform, so it can only
/// over-count (a checkout whose path derives to no index id), never under-count.
/// What: local checkouts that declared no metrics file.
/// Test: `run_tests::a_client_that_will_not_build_counts_every_eligible_repository`.
pub(super) fn analyze_eligible_repositories(model: &ReportModel) -> usize {
    model
        .repositories
        .iter()
        .filter(|repo| repo.metrics.is_none() && repo.local_path.is_some())
        .count()
}

/// Whether an analyze lane's outcome may ship (#6811).
///
/// Why: DOC-67 §9's fail-open contract is per repository and stays that way.
/// What it never covered is the lane failing for EVERY repository, which #6783
/// shipped: 59 repositories all reading "not stated in source data", which a
/// downstream reader took for "static analysis ran and found nothing". A report
/// nothing was measured for is not a degraded report, so the run fails unless
/// the operator says otherwise.
///
/// The threshold: only a TOTAL collapse (`0 of N` assessed, `N > 0`) fails.
/// Partial degradation (`M of N`, `M > 0`) stays a warning at every setting — a
/// 58-of-59 run carries 58 assessed applications, and throwing that away over
/// one unreachable index costs more than it protects. A lane attempted zero
/// times is not a failure either: a manifest of remote-only repositories has no
/// analyze lane to lose.
///
/// # Errors
///
/// When the lane assessed nothing and `allow_degraded` is false. The message
/// names the lane, both counts, and the flag that overrides it.
///
/// Test: `run_tests::{a_total_analyze_collapse_fails_the_run,
/// allow_degraded_lets_a_total_collapse_ship,
/// a_partial_analyze_degradation_is_a_warning_not_a_failure,
/// an_unattempted_analyze_lane_is_not_a_failure}`.
pub(super) fn analyze_lane_verdict(
    coverage: crate::report::AnalyzeLaneCoverage,
    allow_degraded: bool,
) -> Result<()> {
    if !coverage.is_total_failure() {
        if coverage.is_partial_failure() {
            eprintln!(
                "[trusty-review report] --analyze: lane partially degraded — {} of {} \
                 application(s) assessed, {} failed; the report states the coverage and the run \
                 continues (issue #6811)",
                coverage.succeeded,
                coverage.attempted,
                coverage.failed(),
            );
        }
        return Ok(());
    }
    if allow_degraded {
        eprintln!(
            "[trusty-review report] --analyze: lane DID NOT RUN — 0 of {} application(s) \
             assessed; --allow-degraded was passed, so the report is written with that stated \
             (issue #6811)",
            coverage.attempted,
        );
        return Ok(());
    }
    anyhow::bail!(
        "the trusty-analyze lane DID NOT RUN — 0 of {attempted} application(s) assessed, \
         {failed} failed. Every finding count, complexity figure and health factor this report \
         would carry is UNASSESSED, not clean, so it is not written. Fix the analyze lane (is \
         `trusty-analyze` running, and is each checkout indexed?) and re-run, or pass \
         --allow-degraded to ship the report with that fact stated in it.",
        attempted = coverage.attempted,
        failed = coverage.failed(),
    )
}
