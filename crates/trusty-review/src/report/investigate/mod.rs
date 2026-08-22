//! Repo-evidence findings investigation (wave 3, epic #2312 / #2357).
//!
//! Why: a generated exec summary once said "no evidence-based conclusions can be
//! drawn … requiring a full manual code review" while the repository sat readable
//! on disk.  That outcome must be structurally impossible: when the code is
//! available the tool inspects it and produces findings itself.  This module is
//! that inspection — it runs when `--synthesize` is on and the repository is a
//! local checkout, selects the evidence-bearing files, asks the reviewer-role LLM
//! (in bounded batches, see [`batch`]) for findings, and admits ONLY findings
//! whose evidence mechanically verifies.
//! What: [`run_investigation`] drives selection → batching → LLM → verification
//! per local repository and also builds a deterministic dependency inventory and
//! coverage record (including per-batch outcomes); [`apply_investigation`]
//! injects the verified findings into the existing findings pipeline;
//! [`merge_investigation_prose`] overlays their verified (measured-evidence)
//! prose onto synthesis; [`report_sections`] renders the Dependency Inventory and
//! Investigation Coverage sections.  Fail-closed at two granularities: a whole
//! repository is `Unavailable` only if every batch failed; a single failed batch
//! is recorded in coverage while its siblings' findings still survive (#2357
//! wave-3.1 — see [`batch`] for the incident this structural fix replaces).
//! Test: submodule unit tests + `tests/report_investigate.rs` e2e (mock provider).

pub mod analyze;
mod batch;
pub mod deps;
mod render;
pub mod select;
pub mod trace;
pub mod trace_client;
pub mod trace_symbol;
pub mod verify;

use std::sync::Arc;

use serde::Serialize;

use crate::llm::LlmProvider;
use crate::report::metrics::{AnalyzeMetrics, MetricFinding, Severity};
use crate::report::model::ReportModel;
use crate::report::synthesize::{FindingProse, Synthesis};

pub use batch::BatchStatus;
pub use deps::{Dependency, DependencyInventory};
pub use select::{
    Budget, DimensionCoverage, RiskSignals, SCALABILITY_DIMENSION, SECURITY_DIMENSION, Selection,
};
pub use trace::{FindingTrace, TraceAnchor, TraceLimits, TraceSet};
pub use verify::VerifiedFinding;

// ─── Result types ─────────────────────────────────────────────────────────────

/// Outcome of one repository's investigation attempt.
///
/// Why: the coverage section must name WHY a repo produced no findings — remote
/// (never investigated), no readable source, or a provider failure — so a data
/// gap is always explained rather than implied.
/// What: `Available` when the LLM call completed (even with zero findings);
/// `Skipped(reason)` for remote / empty repos not sent to the LLM;
/// `Unavailable(reason)` for a provider/parse failure.
/// Test: `tests/report_investigate.rs` asserts the available + unavailable paths.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "state", content = "reason", rename_all = "snake_case")]
pub enum InvestigationStatus {
    /// The LLM inspected the selected files (findings may still be empty).
    Available,
    /// The repository was not investigated (remote, or no readable source).
    Skipped(String),
    /// The investigation failed closed (provider/parse error).
    Unavailable(String),
}

/// One batch's recorded fate, for the coverage section (#2357 wave-3.1).
///
/// Why: a truncated/failed batch must be named — which files it carried and why
/// it failed — so the report can say "sent but analysis truncated/failed (batch
/// N)" instead of silently omitting those files' findings.
/// What: `index`/`batch_count` place it ("batch 2 of 4"); `files` are the
/// repository-relative paths it carried; `reason` is empty for a completed batch
/// or the failure/truncation reason otherwise.
/// Test: `render_tests::coverage_section_reports_batch_failure`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct BatchNote {
    /// 1-based batch position.
    pub index: usize,
    /// Total batch count for this repository.
    pub batch_count: usize,
    /// Repository-relative paths carried by this batch.
    pub files: Vec<String>,
    /// Empty when completed; the failure/truncation reason otherwise.
    pub reason: String,
}

/// Coverage-honesty record for one investigated repository.
///
/// Why: #2357 mandates the report state exactly what was examined vs skipped and
/// which DD dimensions were reached; serialising this keeps the JSON twin a
/// faithful audit trail.  Wave-3.1 extends this with per-batch accounting so a
/// truncated/failed batch is named rather than silently lowering the finding
/// count.
/// What: the file/byte counts, the dimension coverage split, the rejected-finding
/// count, the budget in force, and the batch totals + failed-batch notes.
/// Test: `render_tests` (coverage section) and the select-derived counts.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Coverage {
    /// Files actually sent to the LLM (across all batches).
    pub files_examined: usize,
    /// Total tracked files in the repo (the denominator).
    pub total_files: usize,
    /// Files not examined (budget-capped or ranked out).
    pub skipped: usize,
    /// Total content bytes sent (across all batches).
    pub bytes_sent: usize,
    /// DD dimensions at least one examined file (or a present test dir) covered.
    pub dimensions_covered: Vec<String>,
    /// DD dimensions no examined file reached.
    pub dimensions_absent: Vec<String>,
    /// Per-dimension coverage of the examined set (#6082).
    pub per_dimension: Vec<select::DimensionCoverage>,
    /// Examined files the manifest named as evidence, with a reason (#6082).
    pub attributed_files: usize,
    /// True when selection was restricted to manifest-declared paths (#6082).
    pub attributed_only: bool,
    /// Findings rejected for unverifiable evidence (across all batches).
    pub rejected: usize,
    /// The file/byte budget in force for this run.
    pub budget: Budget,
    /// Total batches this repository's selection was split into.
    pub batches_total: usize,
    /// Batches that completed (parsed cleanly; findings may still be zero).
    pub batches_succeeded: usize,
    /// Batches that truncated (even after retry) or errored, with their notes.
    pub batches_failed: Vec<BatchNote>,
}

impl Coverage {
    /// Build a coverage record from a selection outcome, rejection count, the
    /// per-batch outcomes, and the verified findings.
    ///
    /// #6080: `dimensions_covered` came from file selection alone, so a
    /// dimension the model raised findings in but no selected file matched by
    /// name was listed as NOT investigated — while its findings sat in §5. A
    /// dimension that produced a verified finding was covered by definition, so
    /// findings' dimensions fold in here and drop out of `dimensions_absent`.
    fn build(
        sel: &Selection,
        rejected: usize,
        budget: Budget,
        batches: &[batch::BatchOutcome],
        findings: &[VerifiedFinding],
    ) -> Self {
        let batches_failed: Vec<BatchNote> = batches
            .iter()
            .filter(|b| b.status.is_failure())
            .map(|b| BatchNote {
                index: b.index,
                batch_count: b.batch_count,
                files: b.files.clone(),
                reason: b.status.reason(),
            })
            .collect();
        let mut dimensions_covered = sel.dimensions_covered.clone();
        for d in findings
            .iter()
            .map(|f| f.dimension.trim())
            .filter(|d| !d.is_empty())
        {
            if !dimensions_covered.iter().any(|c| c == d) {
                dimensions_covered.push(d.to_string());
            }
        }
        let dimensions_absent: Vec<String> = sel
            .dimensions_absent
            .iter()
            .filter(|d| !dimensions_covered.contains(d))
            .cloned()
            .collect();
        Coverage {
            files_examined: sel.files.len(),
            total_files: sel.total_files,
            skipped: sel.skipped,
            bytes_sent: sel.bytes_sent,
            dimensions_covered,
            dimensions_absent,
            per_dimension: sel.per_dimension.clone(),
            attributed_files: sel.attributed_files,
            attributed_only: sel.attributed_only,
            rejected,
            budget,
            batches_total: batches.len(),
            batches_succeeded: batches.len() - batches_failed.len(),
            batches_failed,
        }
    }

    /// A coverage record for a repository whose selection was empty (no batches
    /// were ever attempted).
    fn empty(sel: &Selection, budget: Budget) -> Self {
        Coverage::build(sel, 0, budget, &[], &[])
    }
}

/// One repository's complete investigation result.
///
/// Deliberately NOT `#[non_exhaustive]`, unlike the three report types #5762
/// marked: this crate's own `trusty-review` binary is a separate compilation
/// unit and builds one with a struct literal (`cli_report.rs`), which
/// `#[non_exhaustive]` forbids. Adding a field here is a MINOR bump for a
/// `0.y.z` crate — #6166 paid it for `traces`.
#[derive(Debug, Clone, Serialize)]
pub struct RepoInvestigation {
    /// Repository slug (matches the model's `RepositoryReport::slug`).
    pub slug: String,
    /// Human-readable application name.
    pub name: String,
    /// The attempt status (available / skipped / unavailable).
    pub status: InvestigationStatus,
    /// Verified findings (RED/AMBER with measured evidence; GREEN topics).
    pub findings: Vec<VerifiedFinding>,
    /// Deterministic dependency inventory (measured; always attempted).
    pub deps: DependencyInventory,
    /// Coverage-honesty record.
    pub coverage: Coverage,
    /// Symbol-graph anchors for the candidate findings (#6166).
    ///
    /// `None` when the trace pass did not run for this repository (no findings
    /// to trace). A `Some` set always holds one record per candidate, including
    /// the refused ones.
    pub traces: Option<TraceSet>,
}

/// The aggregate investigation result recorded on the [`ReportModel`].
#[derive(Debug, Clone, Serialize)]
pub struct Investigation {
    /// One entry per local repository investigated (or skipped with a reason).
    pub repos: Vec<RepoInvestigation>,
}

impl Investigation {
    /// The coverage summary injected into the synthesis prompt so the exec summary
    /// can only claim a data gap that truly exists — and must name it.
    ///
    /// Why: the owner defect was an exec summary claiming ignorance while code was
    /// readable; feeding synthesis the real coverage forecloses that.
    /// What: one line per available repo (examined/total, rejected, dimensions not
    /// investigated) plus a standing mandate to synthesise from the findings.
    /// Test: `render_tests::coverage_prompt_summary_lists_gaps`.
    pub fn coverage_prompt_summary(&self) -> String {
        render::coverage_prompt_summary(self)
    }
}

// ─── Orchestration ────────────────────────────────────────────────────────────

/// Run the investigation across every local repository in the model.
///
/// Why: the single entry point the CLI awaits under `--synthesize`; it drives
/// select → batch → LLM → verify per local repo.  Batching (#2357 wave-3.1) means
/// a single batch's truncation/failure no longer discards the whole repository's
/// findings — only a whole-repository failure (every batch failed) is
/// `Unavailable`.
/// What: for each `local_path` repository it lists tracked files, selects within
/// `budget`, builds the deterministic dependency inventory, partitions the
/// selection into size-bounded batches ([`batch::partition_batches`]), and (when
/// files were selected) runs them via [`batch::run_batches`].  Remote or empty
/// repos are recorded `Skipped`.  Returns `None` when no repository is a local
/// checkout (nothing to investigate).
/// Test: `tests/report_investigate.rs` (mock provider, verifiable + unverifiable
/// + multi-batch partial failure).
pub async fn run_investigation(
    provider: Arc<dyn LlmProvider>,
    llm_model: &str,
    model: &ReportModel,
    budget: Budget,
    attributed_only: bool,
) -> Option<Investigation> {
    let instructions = model.instructions.as_deref();
    let mut repos = Vec::new();
    let mut any_local = false;

    for repo in &model.repositories {
        let Some(path) = &repo.local_path else {
            continue; // remote → not investigated at all
        };
        any_local = true;
        let deps = deps::build_inventory(path);

        let files = crate::report::scan::list_tracked_files(path);
        // #6078: the manifest's declared ranking and the trusty-analyze findings
        // already on this repo steer which files get read; absent both, the
        // selection is byte-identical to the pre-#6078 one.
        let signals = select::RiskSignals {
            priorities: &repo.inspect_priority,
            metrics: repo.metrics.as_ref(),
            // #6082: only meaningful when the manifest declared something —
            // an empty priority list under this flag would examine nothing.
            attributed_only: attributed_only && !repo.inspect_priority.is_empty(),
        };
        let selection = select::select_files(path, &files, instructions, budget, signals);
        if selection.is_empty() {
            repos.push(RepoInvestigation {
                slug: repo.slug.clone(),
                name: repo.name.clone(),
                status: InvestigationStatus::Skipped("no readable source files".to_string()),
                findings: Vec::new(),
                coverage: Coverage::empty(&selection, budget),
                deps,
                traces: None,
            });
            continue;
        }

        let batches = batch::partition_batches(&selection.files);
        let (findings, rejected, outcomes) = batch::run_batches(
            provider.clone(),
            llm_model,
            &repo.name,
            &batches,
            selection.total_files,
            instructions,
            &selection,
        )
        .await;

        let status = repo_status(&outcomes);
        let coverage = Coverage::build(&selection, rejected, budget, &outcomes, &findings);
        let traces = trace_repo(path, &findings).await;
        repos.push(RepoInvestigation {
            slug: repo.slug.clone(),
            name: repo.name.clone(),
            status,
            findings,
            coverage,
            deps,
            traces,
        });
    }

    if !any_local {
        return None;
    }
    Some(Investigation { repos })
}

/// Trace one repository's candidate findings against the live symbol graph
/// (#6166).
///
/// Why: this is where the trace pass gets its live [`trace_client::TraceSource`]
/// — `run_investigation`'s signature stays as it was, and the unit tests reach
/// [`trace::assemble_traces`] directly with a stub instead.
/// What: `None` when the repository produced no findings (nothing to anchor) or
/// when the loopback HTTP client will not build, which the pass treats the same
/// way an unreachable daemon would. Otherwise the full record set, refusals
/// included.
/// Test: `trace_tests.rs` covers `assemble_traces`; this wrapper is exercised by
/// `tests/report_investigate.rs`, where no daemon runs and every candidate
/// records the unreachable reason.
async fn trace_repo(path: &std::path::Path, findings: &[VerifiedFinding]) -> Option<TraceSet> {
    if findings.is_empty() {
        return None;
    }
    let source = trace_client::HttpTraceSource::resolved()?;
    Some(trace::assemble_traces(path, findings, &source, TraceLimits::default()).await)
}

/// Roll up per-batch outcomes into one repository-level [`InvestigationStatus`].
///
/// Why: the repository is only truly `Unavailable` when EVERY batch failed —
/// that is the only case with zero verified findings by construction (a
/// completed batch may legitimately find nothing, which is still `Available`).
/// A single failed batch among several successes still yields `Available`; the
/// coverage section names the gap.
/// What: `Available` when at least one batch completed; `Unavailable` (naming
/// the first batch's failure reason) when every batch failed or there were no
/// batches at all (should not happen — callers only invoke this for a non-empty
/// selection).
/// Test: `tests/report_investigate.rs::investigation_survives_one_truncated_batch_of_three`.
fn repo_status(outcomes: &[batch::BatchOutcome]) -> InvestigationStatus {
    let succeeded = outcomes.iter().filter(|o| !o.status.is_failure()).count();
    if succeeded > 0 {
        return InvestigationStatus::Available;
    }
    let reason = outcomes
        .first()
        .map(|o| o.status.reason())
        .unwrap_or_else(|| "no batches attempted".to_string());
    InvestigationStatus::Unavailable(format!(
        "all {} batch(es) failed ({reason})",
        outcomes.len()
    ))
}

// ─── Integration into the findings pipeline ───────────────────────────────────

/// Inject verified investigation findings into the model's findings pipeline and
/// record the investigation on the model.
///
/// Why: #2357 requires verified findings to flow through the SAME `FindingRow`
/// path as trusty-analyze metric findings — severity bands, greens → topic list —
/// so injecting a `MetricFinding` per verified finding reuses the whole reporter
/// path with no special-casing.  Recording the investigation on the model (before
/// synthesis) also lets the synthesis prompt read the coverage summary.
/// What: scrubs a local copy of `inv` (#5323), appends a `MetricFinding {title,
/// severity, category=dimension, component=file:line}` to each repo's metrics
/// (creating one if absent) from that copy, then records the scrubbed copy as
/// `model.investigation` — which is what `reporter.rs` serialises into the JSON
/// twin and what `render.rs` renders the coverage sections from.
/// Test: `tests/report_investigate.rs` asserts the finding renders in the band;
/// `apply_investigation_scrubs_configured_credentials` asserts the scrub.
pub fn apply_investigation(model: &mut ReportModel, inv: &Investigation) {
    // #5323: verified-finding prose is LLM-authored over repository text, the
    // likeliest producer to quote something raw. Scrub the record once here so
    // the derived findings AND the copy stored on the model are both clean.
    let secrets = super::redact::report_secrets();
    let mut inv = inv.clone();
    super::redact::scrub_investigation(&mut inv, &secrets);
    let inv = &inv;

    for repo_inv in &inv.repos {
        let Some(repo) = model
            .repositories
            .iter_mut()
            .find(|r| r.slug == repo_inv.slug)
        else {
            continue;
        };
        if repo_inv.findings.is_empty() {
            continue;
        }
        let metrics = repo.metrics.get_or_insert_with(AnalyzeMetrics::default);
        for f in &repo_inv.findings {
            metrics.findings.push(MetricFinding {
                title: f.title.clone(),
                severity: f.severity,
                category: f.dimension.clone(),
                component: component_ref(f),
                // #5317: a verified finding already carries its own one-line
                // description and remediation — carrying them through means the
                // rendered entry states something even before synthesis runs.
                description: f.description.clone(),
                remediation: f.remediation.clone(),
            });
        }
    }
    model.investigation = Some(inv.clone());
}

/// Scrub one verified finding's prose on its way into [`FindingProse`] (#5323).
///
/// Why: `merge_investigation_prose` is the second, independent route an
/// investigation finding takes to the page, and `FindingRow::merge_prose`
/// overwrites the metrics-derived (already scrubbed) prose with it. Without this
/// the scrub in [`apply_investigation`] is discarded before render on every run
/// that produces a RED/AMBER finding.
/// What: builds the row then applies [`super::redact::scrub_prose`].
/// Test: `investigation_credentials_never_reach_the_rendered_report`.
fn scrubbed_prose(slug: &str, band: &str, f: &VerifiedFinding, secrets: &[String]) -> FindingProse {
    let mut prose = FindingProse {
        app_slug: slug.to_string(),
        title: f.title.clone(),
        severity: band.to_string(),
        description: f.description.clone(),
        evidence: f.evidence_quote.clone(),
        component: component_ref(f),
        business_impact: f.business_impact.clone(),
        remediation: f.remediation.clone(),
        cost_effort: f.cost_effort.clone(),
        evidence_measured: true,
    };
    super::redact::scrub_prose(&mut prose, secrets);
    prose
}

/// Render a finding's `component` as `file:line` (or the bare file / empty).
///
/// #6082: a citation is rendered ONLY when a verified evidence quote backs it.
/// The Security Posture preamble claims every finding's quote was mechanically
/// verified, and a `file:line` with no surviving quote is exactly the shape that
/// claim cannot cover — the reader has a pointer and no way to check it. An
/// unquoted finding still renders; it renders WITHOUT a citation, which is the
/// same path an uncited GREEN has always taken and what keeps it out of the
/// clean-signals list (`reporter::green_topic_line`).
/// Test: `verify_tests::green_keeps_its_verified_quote`,
/// `reporter_tests::an_unquoted_green_renders_without_a_citation`.
fn component_ref(f: &VerifiedFinding) -> String {
    if f.file.is_empty() || f.evidence_quote.trim().is_empty() {
        return String::new();
    }
    match f.line {
        Some(line) => format!("{}:{line}", f.file),
        None => f.file.clone(),
    }
}

/// Overlay verified investigation prose onto the synthesis result.
///
/// Why: the RED/AMBER finding rows render prose from `Synthesis::findings`; the
/// investigation's prose is the trustworthy one (its evidence is guardrail-
/// verified verbatim), so it must WIN over any synthesis prose for the same
/// finding and carry the measured-evidence flag.
/// What: for each verified RED/AMBER finding, removes any synthesis finding with
/// the same `(slug, title)` and pushes a credential-scrubbed `FindingProse`
/// (#5323, see [`scrubbed_prose`]) with `evidence_measured = true`.  #5454 dropped
/// the status flip this used to perform: a `Synthesis` only exists when the
/// narrative pass succeeded, so there is no failed-closed status left to rescue.
/// Test: `tests/report_investigate.rs` asserts measured evidence + inferred
/// prose; `investigation_credentials_never_reach_the_rendered_report` asserts
/// the scrub on the rendered markdown and the JSON twin.
pub fn merge_investigation_prose(synthesis: &mut Synthesis, inv: &Investigation) {
    // #5323: this sink bypasses the metrics scrub entirely, and `merge_prose`
    // overwrites the metrics prose with what lands here.
    let secrets = super::redact::report_secrets();
    for repo_inv in &inv.repos {
        for f in &repo_inv.findings {
            if f.severity == Severity::Green {
                continue; // greens are title-only topics
            }
            let band = match f.severity {
                Severity::Red => "RED",
                Severity::Amber => "AMBER",
                Severity::Green => unreachable!("greens skipped above"),
            };
            synthesis
                .findings
                .retain(|p| !(p.app_slug == repo_inv.slug && p.title == f.title));
            synthesis
                .findings
                .push(scrubbed_prose(&repo_inv.slug, band, f, &secrets));
        }
    }
}

/// Render the appended Dependency Inventory + Investigation Coverage sections.
///
/// Why: the reporter appends these after polish so their measured/inferred rows
/// are never collapsed by omit-empty; keeping the markup in this module keeps
/// `reporter.rs` under the SLOC cap and the investigation self-contained.
/// What: delegates to [`render::report_sections`]; returns `""` when the model
/// carries no investigation (output byte-identical to a non-investigated run).
/// Test: `render_tests` (section content) + `reporter_tests` (absent → empty).
pub fn report_sections(model: &ReportModel) -> String {
    render::report_sections(model)
}

#[cfg(test)]
mod coverage_tests {
    use super::*;

    fn green(dimension: &str) -> VerifiedFinding {
        VerifiedFinding {
            title: "Clean signal".to_string(),
            severity: Severity::Green,
            dimension: dimension.to_string(),
            file: "src/lib.rs".to_string(),
            line: Some(1),
            evidence_quote: String::new(),
            description: String::new(),
            business_impact: String::new(),
            remediation: String::new(),
            cost_effort: String::new(),
        }
    }

    /// #6080: a dimension that produced findings is a dimension that was
    /// covered.
    ///
    /// Why: `dimensions_covered` came from file selection alone, so a
    /// dimension the model raised findings in but no selected file matched by
    /// name was listed as NOT investigated — while its findings sat in §5 of
    /// the same report.
    /// What: a finding's dimension joins the covered list and leaves the
    /// absent one.
    #[test]
    fn a_dimension_with_findings_is_covered() {
        let sel = select::Selection {
            dimensions_covered: vec!["error handling".to_string()],
            dimensions_absent: vec!["scalability".to_string(), "dependencies".to_string()],
            ..Default::default()
        };
        let c = Coverage::build(&sel, 0, Budget::default(), &[], &[green("scalability")]);
        assert!(c.dimensions_covered.contains(&"scalability".to_string()));
        assert_eq!(c.dimensions_absent, vec!["dependencies".to_string()]);
    }
}
