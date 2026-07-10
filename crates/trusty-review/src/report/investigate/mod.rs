//! Repo-evidence findings investigation (wave 3, epic #2312 / #2357).
//!
//! Why: a generated exec summary once said "no evidence-based conclusions can be
//! drawn … requiring a full manual code review" while the repository sat readable
//! on disk.  That outcome must be structurally impossible: when the code is
//! available the tool inspects it and produces findings itself.  This module is
//! that inspection — it runs when `--synthesize` is on and the repository is a
//! local checkout, selects the evidence-bearing files, asks the reviewer-role LLM
//! for findings, and admits ONLY findings whose evidence mechanically verifies.
//! What: [`run_investigation`] drives selection → LLM → verification per local
//! repository and also builds a deterministic dependency inventory and coverage
//! record; [`apply_investigation`] injects the verified findings into the existing
//! findings pipeline; [`merge_investigation_prose`] overlays their verified
//! (measured-evidence) prose onto synthesis; [`report_sections`] renders the
//! Dependency Inventory and Investigation Coverage sections.  Fail-closed: any
//! provider/parse failure yields an `Unavailable` status the report names.
//! Test: submodule unit tests + `tests/report_investigate.rs` e2e (mock provider).

pub mod analyze;
pub mod deps;
mod render;
pub mod select;
pub mod verify;

use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tracing::{debug, warn};

use crate::llm::LlmProvider;
use crate::report::metrics::{AnalyzeMetrics, MetricFinding, Severity};
use crate::report::model::ReportModel;
use crate::report::synthesize::{FindingProse, Synthesis, SynthesisStatus};

pub use deps::{Dependency, DependencyInventory};
pub use select::{Budget, Selection};
pub use verify::VerifiedFinding;

/// Default wall-clock ceiling for one repository's investigation LLM call.
const INVESTIGATION_TIMEOUT: Duration = Duration::from_secs(180);

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

/// Coverage-honesty record for one investigated repository.
///
/// Why: #2357 mandates the report state exactly what was examined vs skipped and
/// which DD dimensions were reached; serialising this keeps the JSON twin a
/// faithful audit trail.
/// What: the file/byte counts, the dimension coverage split, the rejected-finding
/// count, and the budget in force.
/// Test: `render_tests` (coverage section) and the select-derived counts.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Coverage {
    /// Files actually sent to the LLM.
    pub files_examined: usize,
    /// Total tracked files in the repo (the denominator).
    pub total_files: usize,
    /// Files not examined (budget-capped or ranked out).
    pub skipped: usize,
    /// Total content bytes sent.
    pub bytes_sent: usize,
    /// DD dimensions at least one examined file (or a present test dir) covered.
    pub dimensions_covered: Vec<String>,
    /// DD dimensions no examined file reached.
    pub dimensions_absent: Vec<String>,
    /// Findings rejected for unverifiable evidence.
    pub rejected: usize,
    /// The file/byte budget in force for this run.
    pub budget: Budget,
}

impl Coverage {
    /// Build a coverage record from a selection outcome + rejection count.
    fn from_selection(sel: &Selection, rejected: usize, budget: Budget) -> Self {
        Coverage {
            files_examined: sel.files.len(),
            total_files: sel.total_files,
            skipped: sel.skipped,
            bytes_sent: sel.bytes_sent,
            dimensions_covered: sel.dimensions_covered.clone(),
            dimensions_absent: sel.dimensions_absent.clone(),
            rejected,
            budget,
        }
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
/// select → LLM → verify per local repo and fails closed per repo so one bad repo
/// never aborts the report.
/// What: for each `local_path` repository it lists tracked files, selects within
/// `budget`, builds the deterministic dependency inventory, and (when files were
/// selected) calls `provider` under a timeout, parses, and verifies.  Remote or
/// empty repos are recorded `Skipped`.  Returns `None` when no repository is a
/// local checkout (nothing to investigate).
/// Test: `tests/report_investigate.rs` (mock provider, verifiable + unverifiable).
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
                coverage: Coverage::from_selection(&selection, 0, budget),
                deps,
            });
            continue;
        }

        let (status, findings, rejected) = investigate_one(
            provider.clone(),
            llm_model,
            &repo.name,
            &selection,
            instructions,
        )
        .await;

        repos.push(RepoInvestigation {
            slug: repo.slug.clone(),
            name: repo.name.clone(),
            status,
            findings,
            coverage: Coverage::from_selection(&selection, rejected, budget),
            deps,
        });
    }

    if !any_local {
        return None;
    }
    Some(Investigation { repos })
}

/// Investigate one repository's selection: call the LLM, parse, verify.
///
/// Returns the status, the verified findings, and the rejected count.  Fails
/// closed (empty findings + `Unavailable`) on any provider/parse failure.
async fn investigate_one(
    provider: Arc<dyn LlmProvider>,
    llm_model: &str,
    app_name: &str,
    selection: &Selection,
    instructions: Option<&str>,
) -> (InvestigationStatus, Vec<VerifiedFinding>, usize) {
    let req = analyze::build_request(app_name, selection, instructions, llm_model);
    let resp = match tokio::time::timeout(INVESTIGATION_TIMEOUT, provider.complete(req)).await {
        Err(_) => {
            warn!("investigation: provider timed out — failing closed");
            return (
                InvestigationStatus::Unavailable("provider timeout".to_string()),
                Vec::new(),
                0,
            );
        }
        Ok(Err(e)) => {
            warn!(error = %e, "investigation: provider error — failing closed");
            return (
                InvestigationStatus::Unavailable(format!("provider error: {e}")),
                Vec::new(),
                0,
            );
        }
        Ok(Ok(r)) => r,
    };

    if matches!(
        resp.finish_reason.as_deref(),
        Some("length") | Some("max_tokens")
    ) {
        warn!("investigation: response truncated — failing closed");
        return (
            InvestigationStatus::Unavailable("truncated response".to_string()),
            Vec::new(),
            0,
        );
    }

    let Some(raw) = analyze::parse_findings(&resp.text) else {
        warn!("investigation: unparseable response — failing closed");
        return (
            InvestigationStatus::Unavailable("unparseable response".to_string()),
            Vec::new(),
            0,
        );
    };

    debug!(
        input_tokens = resp.input_tokens,
        output_tokens = resp.output_tokens,
        findings = raw.findings.len(),
        "investigation: provider call complete"
    );

    let outcome = verify::verify_findings(raw.findings, selection);
    (
        InvestigationStatus::Available,
        outcome.verified,
        outcome.rejected,
    )
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
