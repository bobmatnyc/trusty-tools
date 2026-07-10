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
pub mod verify;

use std::sync::Arc;

use serde::Serialize;

use crate::llm::LlmProvider;
use crate::report::metrics::{AnalyzeMetrics, MetricFinding, Severity};
use crate::report::model::ReportModel;
use crate::report::synthesize::{FindingProse, Synthesis, SynthesisStatus};

pub use batch::BatchStatus;
pub use deps::{Dependency, DependencyInventory};
pub use select::{Budget, Selection};
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
    /// Build a coverage record from a selection outcome, rejection count, and the
    /// per-batch outcomes.
    fn build(
        sel: &Selection,
        rejected: usize,
        budget: Budget,
        batches: &[batch::BatchOutcome],
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
        Coverage {
            files_examined: sel.files.len(),
            total_files: sel.total_files,
            skipped: sel.skipped,
            bytes_sent: sel.bytes_sent,
            dimensions_covered: sel.dimensions_covered.clone(),
            dimensions_absent: sel.dimensions_absent.clone(),
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
        Coverage::build(sel, 0, budget, &[])
    }
}

/// One repository's complete investigation result.
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
        let selection = select::select_files(path, &files, instructions, budget);
        if selection.is_empty() {
            repos.push(RepoInvestigation {
                slug: repo.slug.clone(),
                name: repo.name.clone(),
                status: InvestigationStatus::Skipped("no readable source files".to_string()),
                findings: Vec::new(),
                coverage: Coverage::empty(&selection, budget),
                deps,
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
        repos.push(RepoInvestigation {
            slug: repo.slug.clone(),
            name: repo.name.clone(),
            status,
            findings,
            coverage: Coverage::build(&selection, rejected, budget, &outcomes),
            deps,
        });
    }

    if !any_local {
        return None;
    }
    Some(Investigation { repos })
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
/// What: appends a `MetricFinding {title, severity, category=dimension,
/// component=file:line}` to each repo's metrics (creating one if absent), then
/// sets `model.investigation`.
/// Test: `tests/report_investigate.rs` asserts the finding renders in the band.
pub fn apply_investigation(model: &mut ReportModel, inv: &Investigation) {
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
            });
        }
    }
    model.investigation = Some(inv.clone());
}

/// Render a finding's `component` as `file:line` (or the bare file / empty).
fn component_ref(f: &VerifiedFinding) -> String {
    match (f.file.is_empty(), f.line) {
        (true, _) => String::new(),
        (false, Some(line)) => format!("{}:{line}", f.file),
        (false, None) => f.file.clone(),
    }
}

/// Overlay verified investigation prose onto the synthesis result.
///
/// Why: the RED/AMBER finding rows render prose from `Synthesis::findings`; the
/// investigation's prose is the trustworthy one (its evidence is guardrail-
/// verified verbatim), so it must WIN over any synthesis prose for the same
/// finding and carry the measured-evidence flag.  Marking the synthesis available
/// when investigation produced findings ensures they render even if the synthesis
/// narrative pass itself failed closed.
/// What: for each verified RED/AMBER finding, removes any synthesis finding with
/// the same `(slug, title)` and pushes a `FindingProse` with `evidence_measured =
/// true`; flips the status to `Available` when any finding was added.
/// Test: `tests/report_investigate.rs` asserts measured evidence + inferred prose.
pub fn merge_investigation_prose(synthesis: &mut Synthesis, inv: &Investigation) {
    let mut added = false;
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
            synthesis.findings.push(FindingProse {
                app_slug: repo_inv.slug.clone(),
                title: f.title.clone(),
                severity: band.to_string(),
                description: f.description.clone(),
                evidence: f.evidence_quote.clone(),
                component: component_ref(f),
                business_impact: f.business_impact.clone(),
                remediation: f.remediation.clone(),
                cost_effort: f.cost_effort.clone(),
                evidence_measured: true,
            });
            added = true;
        }
    }
    if added && !matches!(synthesis.status, SynthesisStatus::Available) {
        synthesis.status = SynthesisStatus::Available;
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
