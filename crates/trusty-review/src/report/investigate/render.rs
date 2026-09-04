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

use crate::report::metrics::Severity;
use crate::report::model::ReportModel;
use crate::report::provenance::{INFERRED_TAG, MEASURED_TAG};

use super::{
    Coverage, DependencyInventory, ExposureKind, Investigation, InvestigationStatus,
    RepoInvestigation, Verdict,
};

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
    out.push_str(&coverage_section(model, inv));
    out
}

/// Render the measured "Dependency Inventory" section across all repos.
///
/// #6137: an empty inventory is a NAMED GAP, never an assertion of zero. "No
/// manifest-declared dependencies were found" was printed for a workspace with
/// 134 of them — a false clean claim on the one section an acquirer reads to
/// learn what the system is built on. When no repository had a manifest read at
/// all, the section says the manifests were not examined; when manifests WERE
/// read and declared nothing, it names them and says so.
fn dependency_section(inv: &Investigation) -> String {
    let any = inv.repos.iter().any(|r| !r.deps.is_empty());
    let mut out = String::from("\n\n## Dependency Inventory\n\n");
    if !any {
        out.push_str(&empty_inventory_line(inv));
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

/// The line an empty Dependency Inventory renders instead of a clean claim.
///
/// Why/What: see [`dependency_section`]. Distinguishes "no manifest was
/// examined" (a gap in the pass) from "the manifests read declare nothing" (a
/// fact about those manifests), and names the manifests in the second case.
/// Test: `render_tests::{empty_inventory_with_no_manifest_is_a_named_gap,
/// empty_inventory_names_the_manifests_it_read}`.
fn empty_inventory_line(inv: &Investigation) -> String {
    let mut manifests: Vec<&str> = Vec::new();
    for name in inv
        .repos
        .iter()
        .flat_map(|r| r.deps.manifests_examined.iter())
    {
        if !manifests.contains(&name.as_str()) {
            manifests.push(name);
        }
    }
    if manifests.is_empty() {
        return format!(
            "_Dependency manifests not examined{MEASURED_TAG} — no supported manifest \
             (package.json, Cargo.toml, pyproject.toml, go.mod) was read at any inspected \
             repository root, so this report states nothing about what the system depends on. \
             Read this section as unassessed, not as a dependency-free codebase._\n"
        );
    }
    format!(
        "_Examined{MEASURED_TAG} {} and found no directly declared dependencies. Transitive \
         dependencies, per-member manifests below the checkout root, and vendored code are \
         outside this section's scope and are not assessed._\n",
        manifests.join(", ")
    )
}

/// Render one repository's dependency table (capped, with an "and N more" line).
///
/// #6788: the cap lives here, not in the inventory — `inv.deps` carries every
/// row for `investigation.json`, and `rendered()` is the 30-row table view.
/// Test: `render_tests::{dependency_table_caps_and_overflows,
/// dependency_table_draws_only_max_rows_of_a_full_inventory}`.
fn dependency_table(inv: &DependencyInventory) -> String {
    let mut out = String::from("| Package | Ecosystem | Declared | Locked |\n|---|---|---|---|\n");
    for d in inv.rendered() {
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
fn coverage_section(model: &ReportModel, inv: &Investigation) -> String {
    let mut out = String::from("\n\n## Investigation Coverage\n\n");
    out.push_str(&format!(
        "_What the repo-evidence pass actually inspected{MEASURED_TAG}._\n\n",
    ));
    for repo in &inv.repos {
        let rendered = model
            .repositories
            .iter()
            .find(|r| r.slug == repo.slug)
            .and_then(|r| r.metrics.as_ref())
            .map(|m| rendered_counts(&m.findings));
        out.push_str(&coverage_lines(repo, rendered));
    }
    out
}

/// One repository's rendered section-5.2 entries, by band.
#[derive(Clone, Copy, Default)]
struct RenderedCounts {
    red: usize,
    amber: usize,
    green: usize,
}

impl RenderedCounts {
    fn total(self) -> usize {
        self.red + self.amber + self.green
    }
}

/// Count the entries section 5.2 renders for one repository.
///
/// Reads the SAME `metrics.findings` list `push_finding_band` and
/// `push_green_topics` render from, so the reconciliation cannot state a number
/// the page does not carry.
fn rendered_counts(findings: &[crate::report::metrics::MetricFinding]) -> RenderedCounts {
    let mut c = RenderedCounts::default();
    for f in findings {
        match f.severity {
            Severity::Red => c.red += 1,
            Severity::Amber => c.amber += 1,
            Severity::Green => c.green += 1,
        }
    }
    c
}

/// State how the verified-finding count adds up to what section 5.2 renders
/// (#6082 lap 7).
///
/// Why: the coverage section said "verified findings: 198" and section 5.2
/// numbered 5 RED, 145 AMBER and 68 GREEN — 218 entries. Both are correct and
/// nothing on the page connected them, so the only reading available to a
/// diligence reader was that one of the two numbers was wrong. The 20-entry
/// difference is the complexity hotspots trusty-analyze measures mechanically,
/// which never pass through the investigation at all.
/// What: one line stating `verified + mechanically-measured = rendered`, with
/// every number read from the data. Renders nothing when this repository has no
/// rendered metrics, and nothing when the rendered set is SMALLER than the
/// verified set — that is not an arithmetic this function can explain, and a
/// reconciliation that does not reconcile is worse than none.
/// Test: `render_tests::{coverage_reconciles_verified_against_rendered,
/// coverage_omits_the_reconciliation_when_it_would_not_add_up}`.
fn reconciliation_line(repo: &RepoInvestigation, rendered: Option<RenderedCounts>) -> String {
    let Some(r) = rendered else {
        return String::new();
    };
    let verified = repo.findings.len();
    if r.total() < verified {
        return String::new();
    }
    let mechanical = r.total() - verified;
    format!(
        "- section 5.2 reconciliation: {verified} investigation-verified finding(s) + \
         {mechanical} finding(s) trusty-analyze measured mechanically = {} entries rendered \
         ({} RED, {} AMBER, {} GREEN)\n",
        r.total(),
        r.red,
        r.amber,
        r.green,
    )
}

/// Render one repository's coverage bullet block.
fn coverage_lines(repo: &RepoInvestigation, rendered: Option<RenderedCounts>) -> String {
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
    // #6078: "12 of 200" left the reader to divide; the percentage is the figure
    // a due-diligence reader actually weighs the findings against.
    out.push_str(&format!(
        "- files examined: {} of {} tracked ({}% coverage, {} skipped); {} bytes sent (budget {}/{})\n",
        c.files_examined,
        c.total_files,
        coverage_pct(c.files_examined, c.total_files),
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
    out.push_str(&discovery_lines(c));
    if c.rejected > 0 {
        out.push_str(&format!(
            "- investigation: {} finding(s) rejected (unverifiable evidence)\n",
            c.rejected
        ));
    }
    let (total, risk, clean) = finding_counts(repo);
    out.push_str(&format!(
        "- verified findings: {total} ({risk} RED/AMBER evidence-backed, {clean} clean signals)\n"
    ));
    out.push_str(&reconciliation_line(repo, rendered));
    out.push_str(&exposure_lines(repo));
    out.push_str(&trace_line(repo));
    out.push_str(&verdict_lines(repo));
    out.push_str(&batch_lines(c));
    out
}

/// Render the trace-verdict counts, then name every finding the verdict pass
/// cleared or could not check (#6166 leg 2).
///
/// Why: the counts alone would let a reader see that six findings were
/// unverifiable without ever learning which six, and a cleared finding is an
/// owner-visible decision — it changed a severity band, so the reason it changed
/// belongs on the page next to the count rather than only in the JSON twin.
/// What: the summary line, then one sub-bullet per cleared finding and one per
/// unverifiable finding, each naming the title and the one-line reason. Renders
/// nothing at all without a verdict set, so a report from before this pass
/// renders byte-identically.
/// Test: `render_tests::{coverage_section_states_the_verdict_counts,
/// coverage_section_names_every_cleared_and_unverifiable_finding,
/// coverage_section_omits_the_verdict_line_without_verdicts}`.
fn verdict_lines(repo: &RepoInvestigation) -> String {
    let Some(v) = &repo.verdicts else {
        return String::new();
    };
    let mut out = format!("- trace verdicts: {}\n", v.summary_line());
    if !v.model.is_empty() {
        out.push_str(&format!("  - judged by: {}\n", v.model));
    }
    for f in &v.verdicts {
        let label = match f.verdict {
            Verdict::Cleared => "cleared",
            Verdict::Unverifiable => "unverifiable",
            // A confirmed finding carries its verdict at the finding itself, in
            // section 5; repeating all of them here would bury the two that
            // changed something.
            Verdict::Confirmed => continue,
        };
        out.push_str(&format!("  - {label}: {} — {}\n", f.title, f.reason));
    }
    out
}

/// Render how many candidate findings got a symbol-graph anchor (#6166).
///
/// Why: the no-trace count is the honest half. On the engagement that drove
/// this, 13 of 30 candidates resolved to a symbol the graph placed in a
/// DIFFERENT file, and a coverage section that printed only the successes would
/// have read as a complete trace of a third of the findings.
/// What: one line when the repository has a trace set, nothing otherwise, so a
/// report from before this pass renders byte-identically.
/// Test: `render_tests::{coverage_section_states_the_trace_counts,
/// coverage_section_omits_the_trace_line_without_traces}`.
fn trace_line(repo: &RepoInvestigation) -> String {
    let Some(t) = &repo.traces else {
        return String::new();
    };
    format!(
        "- traces assembled: {} of {} candidate findings ({} no-trace)\n",
        t.assembled, t.candidates, t.no_trace,
    )
}

/// One repository's verified findings, split for reporting (#6080).
///
/// Why: the coverage section counted evidence-backed findings while the
/// synthesis prompt counted all of them, so one report's executive summary said
/// "102 verified findings" and its coverage section said "79 verified
/// evidence-backed findings" about the same set. Both surfaces read this
/// function now, so the two numbers cannot disagree again.
/// What: `(total, risk, clean)` — every verified finding, the RED/AMBER ones
/// carrying an evidence quote, and the GREEN clean signals.
/// Test: `render_tests::{coverage_section_states_examined_and_rejected,
/// coverage_prompt_summary_and_section_agree_on_counts}`.
fn finding_counts(repo: &RepoInvestigation) -> (usize, usize, usize) {
    let clean = repo
        .findings
        .iter()
        .filter(|f| f.severity == Severity::Green)
        .count();
    let total = repo.findings.len();
    (total, total - clean, clean)
}

/// Render how the examined set was discovered, and its per-dimension split
/// (#6082).
///
/// Why: a coverage percentage says how much was read; it never says whether the
/// files were CHOSEN for a reason. When the manifest carried search-derived
/// evidence, each dimension names the query that found its files. When it did
/// not — no index, a daemon that would not start, a hand-written manifest — the
/// selection silently degrades to path names, and the degradation is stated
/// here rather than left to be inferred from a thinner report. The cause of
/// that degradation is stated separately, in Gaps & Caveats, by whoever wrote
/// the manifest.
/// What: one discovery line, then one line per covered dimension with its
/// examined-file count and one example naming why that file was read. Renders
/// nothing extra for a repository with no dimensions covered.
/// Test: `render_tests::{coverage_section_names_the_discovery_source,
/// coverage_section_names_the_heuristic_degradation}`.
fn discovery_lines(c: &Coverage) -> String {
    let mut out = if c.attributed_files > 0 {
        format!(
            "- evidence discovery: {} of {} examined file(s) came from manifest-declared evidence queries\n",
            c.attributed_files, c.files_examined,
        )
    } else {
        String::from(
            "- evidence discovery: path-name heuristics only — the manifest declared no \
             search-derived evidence for this repository (see Gaps & Caveats for why)\n",
        )
    };
    // #6082: under attributed-only selection an examined set smaller than the
    // budget is a SHORTFALL of evidence, not spare capacity. Stating it is the
    // whole point of declining the top-up — silence here would read as a full
    // sample.
    if c.attributed_only && c.files_examined < c.budget.max_files {
        out.push_str(&format!(
            "- attributed-only selection: {} of {} budgeted file(s) carried search or complexity \
             evidence; the remaining {} were left unread rather than filled by path-name \
             heuristics\n",
            c.files_examined,
            c.budget.max_files,
            c.budget.max_files - c.files_examined,
        ));
    }
    for d in &c.per_dimension {
        // #6193: the test-coverage row is enumerated, not sampled, so it states
        // its own basis rather than borrowing the "N file(s) examined — e.g."
        // shape of the five rows that ARE sampled.
        if d.dimension == super::select::TEST_DIMENSION {
            out.push_str(&format!("  - {}: {}\n", d.dimension, c.test_census.line()));
            continue;
        }
        out.push_str(&format!(
            "  - {}: {} file(s) examined{}\n",
            d.dimension,
            d.files_examined,
            d.example
                .as_deref()
                .map(|e| format!(" — e.g. {e}"))
                .unwrap_or_default(),
        ));
    }
    out
}

/// Render what the source said about this repository's reachability (#6191).
///
/// Why: the reachability guard now has an evidence source, and a guard whose
/// evidence a reader cannot see is indistinguishable from the text-marker
/// inference it replaced. This line states how much was collected and from where,
/// which is also what makes the bound visible: only examined files carry facts.
/// What: one line with the per-kind counts, and a sub-bullet naming each file
/// the source placed beyond this host — the class that used to be withheld.
/// Nothing at all when no fact was collected, so a repository whose examined
/// files state neither renders exactly as before.
/// Test: `render_tests::{coverage_section_states_the_exposure_evidence,
/// coverage_section_omits_the_exposure_line_without_evidence}`.
fn exposure_lines(repo: &RepoInvestigation) -> String {
    if repo.exposure.is_empty() {
        return String::new();
    }
    let mut counts: Vec<(ExposureKind, usize)> = Vec::new();
    for f in &repo.exposure {
        match counts.iter_mut().find(|(k, _)| *k == f.kind) {
            Some(entry) => entry.1 += 1,
            None => counts.push((f.kind, 1)),
        }
    }
    counts.sort_by_key(|(k, _)| std::cmp::Reverse(*k));
    let parts: Vec<String> = counts
        .iter()
        .map(|(k, n)| format!("{n} {}", k.label()))
        .collect();
    let mut out = format!(
        "- reachability evidence: {} file(s) of the examined set state a bind address or an \
         outbound call in their own source ({})\n",
        repo.exposure.len(),
        parts.join(", "),
    );
    for f in repo.exposure.iter().filter(|f| f.kind.is_beyond_host()) {
        out.push_str(&format!(
            "  - beyond this host: {} ({}) — `{}`\n",
            f.file,
            f.kind.label(),
            f.evidence,
        ));
    }
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

/// Format `examined / total` as a one-decimal percentage (#6078).
///
/// Why: both coverage surfaces — the rendered section and the synthesis prompt —
/// must state the SAME figure, so it is computed once here.
/// What: one decimal place; `0.0` when nothing is tracked, which is the only
/// division-by-zero case and reads correctly (nothing was examined).
/// Test: `render_tests::{coverage_section_states_examined_and_rejected,
/// coverage_percentage_handles_empty_repo}`.
fn coverage_pct(examined: usize, total: usize) -> String {
    if total == 0 {
        return "0.0".to_string();
    }
    format!("{:.1}", (examined as f64 / total as f64) * 100.0)
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
                // #6080: the same three counts, with the same labels, the
                // rendered coverage section states — see `finding_counts`.
                let (total, risk, clean) = finding_counts(repo);
                out.push_str(&format!(
                    "- {}: examined {} of {} files ({}%); {total} verified finding(s) ({risk} RED/AMBER evidence-backed, {clean} clean signals); {} rejected; not investigated: {}\n",
                    repo.name,
                    c.files_examined,
                    c.total_files,
                    coverage_pct(c.files_examined, c.total_files),
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
