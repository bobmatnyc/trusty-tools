//! Markdown/prompt rendering for the investigation pass (wave 3, #2357).
//!
//! Why: the Dependency Inventory and Investigation Coverage sections and the
//! synthesis-prompt coverage summary are pure `model → string` transforms; homing
//! them here keeps `mod.rs` orchestration-only and `reporter.rs` under the SLOC
//! cap.  Coverage HONESTY lives here: the sections state exactly what was examined
//! vs skipped, which dimensions were reached, and how many findings were rejected.
//! What: [`report_sections`] renders the two appended sections (measured
//! dependency table + coverage record); [`coverage_prompt_summary`] renders the
//! compact block the synthesizer injects so the exec summary can only claim a real
//! data gap.
//! Test: `render_tests.rs` covers the dependency table (cap + overflow), the
//! coverage section (examined/total, dimensions, rejected), and the prompt summary.

use crate::report::model::ReportModel;
use crate::report::provenance::{INFERRED_TAG, MEASURED_TAG};

use super::{Coverage, DependencyInventory, Investigation, InvestigationStatus, RepoInvestigation};

/// Render the appended Dependency Inventory + Investigation Coverage sections.
///
/// Returns `""` when the model has no investigation, so a non-investigated run is
/// byte-identical.
pub fn report_sections(model: &ReportModel) -> String {
    let Some(inv) = &model.investigation else {
        return String::new();
    };
    let mut out = String::new();
    out.push_str(&dependency_section(inv));
    out.push_str(&coverage_section(inv));
    out
}

/// Render the measured "Dependency Inventory" section across all repos.
fn dependency_section(inv: &Investigation) -> String {
    let any = inv.repos.iter().any(|r| !r.deps.is_empty());
    let mut out = String::from("\n\n## Dependency Inventory\n\n");
    if !any {
        out.push_str(
            "_No manifest-declared dependencies were found in the inspected repositories._\n",
        );
        return out;
    }
    out.push_str(&format!(
        "_Parsed deterministically from manifests + lockfiles{MEASURED_TAG}; staleness judgements, where present, are inferred{INFERRED_TAG}._\n\n",
    ));
    for repo in &inv.repos {
        if repo.deps.is_empty() {
            continue;
        }
        out.push_str(&format!("### {}\n\n", repo.name));
        out.push_str(&dependency_table(&repo.deps));
        out.push('\n');
    }
    out
}

/// Render one repository's dependency table (capped, with an "and N more" line).
fn dependency_table(inv: &DependencyInventory) -> String {
    let mut out = String::from("| Package | Ecosystem | Declared | Locked |\n|---|---|---|---|\n");
    for d in &inv.deps {
        let spec = if d.spec.is_empty() { "—" } else { &d.spec };
        let locked = d.locked.as_deref().unwrap_or("—");
        out.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            d.name, d.ecosystem, spec, locked
        ));
    }
    let overflow = inv.overflow();
    if overflow > 0 {
        out.push_str(&format!("| … and {overflow} more | | | |\n"));
    }
    out
}

/// Render the "Investigation Coverage" honesty section across all repos.
fn coverage_section(inv: &Investigation) -> String {
    let mut out = String::from("\n\n## Investigation Coverage\n\n");
    out.push_str(&format!(
        "_What the repo-evidence pass actually inspected{MEASURED_TAG}._\n\n",
    ));
    for repo in &inv.repos {
        out.push_str(&coverage_lines(repo));
    }
    out
}

/// Render one repository's coverage bullet block.
fn coverage_lines(repo: &RepoInvestigation) -> String {
    let mut out = format!("### {}\n\n", repo.name);
    match &repo.status {
        InvestigationStatus::Skipped(reason) => {
            out.push_str(&format!("- investigation: skipped ({reason})\n"));
        }
        InvestigationStatus::Unavailable(reason) => {
            out.push_str(&format!("- investigation: unavailable ({reason})\n"));
        }
        InvestigationStatus::Available => {
            out.push_str("- investigation: available\n");
        }
    }
    let c: &Coverage = &repo.coverage;
    out.push_str(&format!(
        "- files examined: {} of {} tracked ({} skipped); {} bytes sent (budget {}/{})\n",
        c.files_examined,
        c.total_files,
        c.skipped,
        c.bytes_sent,
        c.budget.max_files,
        c.budget.max_bytes,
    ));
    out.push_str(&format!(
        "- dimensions covered: {}\n",
        join_or_none(&c.dimensions_covered),
    ));
    out.push_str(&format!(
        "- dimensions NOT investigated: {}\n",
        join_or_none(&c.dimensions_absent),
    ));
    if c.rejected > 0 {
        out.push_str(&format!(
            "- investigation: {} finding(s) rejected (unverifiable evidence)\n",
            c.rejected
        ));
    }
    let verified = repo.findings.iter().filter(|f| !f.file.is_empty()).count();
    out.push_str(&format!(
        "- verified evidence-backed findings: {verified}\n"
    ));
    out.push_str(&batch_lines(c));
    out
}

/// Render the batch accounting + any failed-batch notes (wave-3.1, #2357).
///
/// Why: a truncated/failed batch must be NAMED — which files it carried and why —
/// so a reader never mistakes a lower finding count for a clean, complete scan.
/// What: a "batches: T total, S succeeded, F truncated/failed" line, followed by
/// one bullet per failed batch naming its position, reason, and file list.
/// Renders nothing when there was at most one batch and it succeeded (the common
/// case), keeping small-repo reports unchanged.
fn batch_lines(c: &Coverage) -> String {
    if c.batches_total <= 1 && c.batches_failed.is_empty() {
        return String::new();
    }
    let failed = c.batches_failed.len();
    let mut out = format!(
        "- batches: {} total, {} succeeded, {failed} truncated/failed\n",
        c.batches_total, c.batches_succeeded,
    );
    for b in &c.batches_failed {
        out.push_str(&format!(
            "  - batch {} of {}: sent but analysis truncated/failed ({}) — files: {}\n",
            b.index,
            b.batch_count,
            b.reason,
            b.files.join(", "),
        ));
    }
    out
}

/// Join a list with `, `, or render `none` when empty.
fn join_or_none(items: &[String]) -> String {
    if items.is_empty() {
        "none".to_string()
    } else {
        items.join(", ")
    }
}

/// Render the coverage block injected into the synthesis prompt.
///
/// One line per available repo (examined/total, rejected, dimensions not reached,
/// and any batch failures), then a standing mandate: synthesise from the
/// findings; only claim a data gap for a listed uninvestigated dimension or a
/// named failed batch, and say why.
pub fn coverage_prompt_summary(inv: &Investigation) -> String {
    let mut out = String::from("## Investigation coverage (repo-evidence pass)\n\n");
    for repo in &inv.repos {
        let c = &repo.coverage;
        match &repo.status {
            InvestigationStatus::Available => {
                out.push_str(&format!(
                    "- {}: examined {} of {} files; {} verified finding(s); {} rejected; not investigated: {}\n",
                    repo.name,
                    c.files_examined,
                    c.total_files,
                    repo.findings.len(),
                    c.rejected,
                    join_or_none(&c.dimensions_absent),
                ));
                for b in &c.batches_failed {
                    out.push_str(&format!(
                        "  - batch {} of {} truncated/failed ({}); other batches' findings above still stand\n",
                        b.index, b.batch_count, b.reason,
                    ));
                }
            }
            InvestigationStatus::Skipped(reason) | InvestigationStatus::Unavailable(reason) => {
                out.push_str(&format!("- {}: not investigated ({reason})\n", repo.name));
            }
        }
    }
    out.push_str(
        "\nThe findings above were produced by inspecting the actual repository code. Synthesise the executive summary FROM those findings. Do NOT claim that a dimension could not be assessed unless it is listed as \"not investigated\" for that application, or a specific batch is named as truncated/failed above — and if you do, name it and say why (remote-only, budget-limited, or that specific batch).\n",
    );
    out
}

#[cfg(test)]
#[path = "render_tests.rs"]
mod tests;
