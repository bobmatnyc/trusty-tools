//! Tests for investigation rendering (#2357).
//!
//! Why: the Dependency Inventory and Investigation Coverage sections are the
//! report's honesty surface — they must state examined/total, dimensions reached,
//! rejected findings, and the dependency table (capped) exactly.
//! What: exercises the dependency section (with overflow), the coverage section
//! (examined/total, dimensions, rejected note), and the synthesis-prompt summary.
//! Test: included as `#[cfg(test)] mod tests` from `render.rs`.

use super::*;
use crate::report::investigate::deps::{Dependency, DependencyInventory};
use crate::report::investigate::select::DimensionCoverage;
use crate::report::investigate::verify::VerifiedFinding;
use crate::report::investigate::{BatchNote, Budget, FindingVerdict, VerdictSet};
use crate::report::metrics::Severity;

/// A model carrying no repositories — the coverage section's reconciliation
/// line needs a rendered-metrics lookup, and these cases assert the bullets
/// around it.
fn empty_model() -> crate::report::model::ReportModel {
    crate::report::model::ReportModel {
        title: "Test".to_string(),
        template: "report-technical-dd".to_string(),
        analyst: None,
        client: None,
        vendor_methodology: crate::report::model::vendor_methodology(),
        inference: None,
        instructions: None,
        instructions_source: None,
        report_date: "2026-08-21".to_string(),
        generated_date: "2026-08-21".to_string(),
        manifest_path: "manifest.toml".to_string(),
        repositories: Vec::new(),
        gaps: Vec::new(),
        synthesis: None,
        benchmark: None,
        investigation: None,
        section_instructions: Default::default(),
        ticketing: None,
    }
}

fn dep(name: &str, locked: Option<&str>) -> Dependency {
    Dependency {
        name: name.to_string(),
        ecosystem: "cargo".to_string(),
        spec: "1.0".to_string(),
        locked: locked.map(str::to_string),
    }
}

fn finding() -> VerifiedFinding {
    VerifiedFinding {
        trace_verdict: String::new(),
        title: "Hardcoded secret".to_string(),
        severity: Severity::Red,
        dimension: "authentication & secrets".to_string(),
        file: "src/auth.rs".to_string(),
        line: Some(12),
        evidence_quote: "let token = \"abc\";".to_string(),
        description: "d".to_string(),
        business_impact: "i".to_string(),
        remediation: "r".to_string(),
        cost_effort: "low".to_string(),
    }
}

fn repo(
    status: InvestigationStatus,
    findings: Vec<VerifiedFinding>,
    deps: DependencyInventory,
) -> RepoInvestigation {
    RepoInvestigation {
        verdicts: None,
        slug: "acme".to_string(),
        name: "Acme".to_string(),
        status,
        findings,
        deps,
        coverage: Coverage {
            files_examined: 12,
            total_files: 200,
            skipped: 188,
            bytes_sent: 40000,
            dimensions_covered: vec!["authentication & secrets".to_string()],
            dimensions_absent: vec!["scalability".to_string()],
            per_dimension: vec![],
            attributed_files: 0,
            attributed_only: false,
            rejected: 2,
            budget: Budget::default(),
            batches_total: 1,
            batches_succeeded: 1,
            batches_failed: vec![],
        },
        traces: None,
    }
}

/// Why (#6166): the coverage section must state the no-trace count, not only
/// the successes — 13 of 30 candidates refused on the live engagement, and a
/// line that printed only "17 assembled" would read as a complete trace.
/// What: a repository carrying a trace set renders one counted line.
/// Test: this test itself.
#[test]
fn coverage_section_states_the_trace_counts() {
    let mut r = repo(InvestigationStatus::Available, vec![finding()], deps_none());
    r.traces = Some(crate::report::investigate::TraceSet {
        index_id: Some("acme-1234abcd".to_string()),
        traces: vec![],
        candidates: 30,
        assembled: 17,
        no_trace: 13,
        limits: crate::report::investigate::TraceLimits::default(),
    });
    let out = coverage_lines(&r, None);
    assert!(
        out.contains("- traces assembled: 17 of 30 candidate findings (13 no-trace)\n"),
        "{out}"
    );
}

/// A report from before the trace pass renders byte-identically.
#[test]
fn coverage_section_omits_the_trace_line_without_traces() {
    let out = coverage_lines(
        &repo(InvestigationStatus::Available, vec![finding()], deps_none()),
        None,
    );
    assert!(!out.contains("traces assembled"), "{out}");
}

// ─── Trace verdicts (#6166 leg 2) ────────────────────────────────────────────

fn verdict(v: crate::report::investigate::Verdict, title: &str, reason: &str) -> FindingVerdict {
    FindingVerdict {
        title: title.to_string(),
        file: "src/auth.rs".to_string(),
        verdict: v,
        reason: reason.to_string(),
    }
}

fn with_verdicts(verdicts: Vec<FindingVerdict>) -> RepoInvestigation {
    let mut r = repo(InvestigationStatus::Available, vec![finding()], deps_none());
    let confirmed = verdicts
        .iter()
        .filter(|v| v.verdict == Verdict::Confirmed)
        .count();
    let cleared = verdicts
        .iter()
        .filter(|v| v.verdict == Verdict::Cleared)
        .count();
    let unverifiable = verdicts
        .iter()
        .filter(|v| v.verdict == Verdict::Unverifiable)
        .count();
    r.verdicts = Some(VerdictSet {
        traced: verdicts.len(),
        verdicts,
        confirmed,
        cleared,
        unverifiable,
        model: "anthropic/claude-haiku-4.5".to_string(),
    });
    r
}

/// Why (#6166 leg 2): the counts are what a reader weighs the findings against,
/// and they must state the unverifiable share rather than only the decided one.
/// What: one summary line, and the model that judged them.
#[test]
fn coverage_section_states_the_verdict_counts() {
    let out = coverage_lines(
        &with_verdicts(vec![
            verdict(Verdict::Confirmed, "A", "supported at line 12"),
            verdict(Verdict::Cleared, "B", "guarded at line 41"),
            verdict(Verdict::Unverifiable, "C", "no verdict: verifier timeout"),
        ]),
        None,
    );
    assert!(
        out.contains("- trace verdicts: 1 confirmed, 1 cleared, 1 unverifiable of 3 traced\n"),
        "{out}"
    );
    assert!(
        out.contains("  - judged by: anthropic/claude-haiku-4.5\n"),
        "{out}"
    );
}

/// Why: counts alone let a reader see that findings were cleared or unverified
/// without learning WHICH — and a clearing moved a severity band, so its reason
/// is an owner-visible decision that belongs on the page.
/// What: a sub-bullet per cleared and per unverifiable finding; confirmed ones
/// carry their verdict at the finding itself and are not repeated here.
#[test]
fn coverage_section_names_every_cleared_and_unverifiable_finding() {
    let out = coverage_lines(
        &with_verdicts(vec![
            verdict(Verdict::Confirmed, "Confirmed one", "supported"),
            verdict(Verdict::Cleared, "Cleared one", "guarded at line 41"),
            verdict(
                Verdict::Unverifiable,
                "Unclear one",
                "no verdict: verifier timeout",
            ),
        ]),
        None,
    );
    assert!(
        out.contains("  - cleared: Cleared one — guarded at line 41\n"),
        "{out}"
    );
    assert!(
        out.contains("  - unverifiable: Unclear one — no verdict: verifier timeout\n"),
        "{out}"
    );
    assert!(
        !out.contains("Confirmed one"),
        "a confirmed finding is not repeated in coverage: {out}"
    );
}

/// A report from before the verdict pass renders byte-identically.
#[test]
fn coverage_section_omits_the_verdict_line_without_verdicts() {
    let out = coverage_lines(
        &repo(InvestigationStatus::Available, vec![finding()], deps_none()),
        None,
    );
    assert!(!out.contains("trace verdicts"), "{out}");
}

/// An empty inventory, for fixtures that care about coverage only.
fn deps_none() -> DependencyInventory {
    DependencyInventory {
        deps: vec![],
        total: 0,
        manifests_examined: vec![],
    }
}

/// Why: the dependency table must render declared + locked and an honest overflow.
/// What: an inventory of 2 rows with total 5 renders the rows + "and 3 more".
/// Test: this test itself.
#[test]
fn dependency_table_caps_and_overflows() {
    let inv = DependencyInventory {
        deps: vec![dep("serde", Some("1.0.203")), dep("tokio", None)],
        total: 5,
        manifests_examined: vec!["Cargo.toml".to_string()],
    };
    let out = dependency_table(&inv);
    assert!(out.contains("| serde | cargo | 1.0 | 1.0.203 |"));
    assert!(
        out.contains("| tokio | cargo | 1.0 | — |"),
        "missing lock → em dash"
    );
    assert!(out.contains("and 3 more"));
}

/// Why: the coverage section is the honesty surface.
/// What: asserts examined/total, a not-investigated dimension, and the rejected
/// note in the exact `investigation: N finding(s) rejected` form.
/// Test: this test itself.
#[test]
fn coverage_section_states_examined_and_rejected() {
    let investigation = Investigation {
        repos: vec![repo(
            InvestigationStatus::Available,
            vec![finding()],
            DependencyInventory::default(),
        )],
    };
    let out = coverage_section(&empty_model(), &investigation);
    assert!(out.contains("## Investigation Coverage"));
    // #6078: the percentage is part of this line, not a new section.
    assert!(
        out.contains("files examined: 12 of 200 tracked (6.0% coverage, 188 skipped)"),
        "coverage line missing the percentage: {out}"
    );
    assert!(out.contains("NOT investigated: scalability"));
    assert!(out.contains("2 finding(s) rejected (unverifiable evidence)"));
    assert!(out.contains("verified findings: 1 (1 RED/AMBER evidence-backed, 0 clean signals)"));
}

/// One repository whose coverage carries #6082's discovery record.
fn discovered(attributed: usize, per_dimension: Vec<DimensionCoverage>) -> Investigation {
    let mut r = repo(
        InvestigationStatus::Available,
        vec![finding()],
        DependencyInventory::default(),
    );
    r.coverage.attributed_files = attributed;
    r.coverage.per_dimension = per_dimension;
    Investigation { repos: vec![r] }
}

/// #6082: declining the heuristic top-up is only honest if the shortfall is
/// SAID. Silence would render a half-filled budget exactly like a full sample.
#[test]
fn coverage_section_states_the_attributed_only_shortfall() {
    let mut inv = discovered(3, Vec::new());
    inv.repos[0].coverage.attributed_only = true;
    inv.repos[0].coverage.files_examined = 3;
    inv.repos[0].coverage.budget.max_files = 40;
    let out = coverage_section(&empty_model(), &inv);
    assert!(
        out.contains(
            "attributed-only selection: 3 of 40 budgeted file(s) carried search or complexity \
             evidence; the remaining 37 were left unread"
        ),
        "{out}"
    );

    // A budget the evidence filled has no shortfall to state.
    let mut full = discovered(40, Vec::new());
    full.repos[0].coverage.attributed_only = true;
    full.repos[0].coverage.files_examined = 40;
    full.repos[0].coverage.budget.max_files = 40;
    assert!(
        !coverage_section(&empty_model(), &full).contains("attributed-only selection"),
        "a filled budget states no shortfall"
    );
}

/// Why: #6082 — the coverage section must say what the examined set was chosen
/// BY, per dimension, or a reader cannot tell a searched repository from a
/// guessed one.
/// What: the discovery line counts the manifest-declared files, and each
/// dimension line names one example with the query that found it.
/// Test: this test itself.
#[test]
fn coverage_section_names_the_discovery_source() {
    let out = coverage_section(
        &empty_model(),
        &discovered(
            9,
            vec![DimensionCoverage {
                dimension: "error handling".to_string(),
                files_examined: 4,
                example: Some(
                    "src/err.rs (trusty-search hit for \"error swallowed\" (score 0.77, line 12))"
                        .to_string(),
                ),
            }],
        ),
    );
    assert!(
        out.contains("evidence discovery: 9 of 12 examined file(s) came from manifest-declared evidence queries"),
        "{out}"
    );
    assert!(
        out.contains("- error handling: 4 file(s) examined — e.g. src/err.rs (trusty-search hit"),
        "{out}"
    );
}

/// Why: the fail-open rule — a run whose search index was unavailable degrades
/// to path names, and the coverage section must NAME that rather than render
/// identically to a searched run.
/// What: zero attributed files renders the degradation and points at Gaps &
/// Caveats for the cause.
/// Test: this test itself.
#[test]
fn coverage_section_names_the_heuristic_degradation() {
    let out = coverage_section(&empty_model(), &discovered(0, Vec::new()));
    assert!(
        out.contains("evidence discovery: path-name heuristics only"),
        "{out}"
    );
    assert!(out.contains("see Gaps & Caveats for why"), "{out}");
}

/// Why: a skipped repo must name why in the coverage section (no silent gap).
/// What: a remote/empty repo renders its skip reason.
/// Test: this test itself.
#[test]
fn coverage_section_names_skip_reason() {
    let investigation = Investigation {
        repos: vec![repo(
            InvestigationStatus::Skipped("no readable source files".to_string()),
            vec![],
            DependencyInventory::default(),
        )],
    };
    let out = coverage_section(&empty_model(), &investigation);
    assert!(out.contains("investigation: skipped (no readable source files)"));
}

/// Why: the prompt summary must forbid a false data-gap claim and list real gaps.
/// What: asserts the examined line, the not-investigated list, and the mandate.
/// Test: this test itself.
#[test]
fn coverage_prompt_summary_lists_gaps() {
    let investigation = Investigation {
        repos: vec![repo(
            InvestigationStatus::Available,
            vec![finding()],
            DependencyInventory::default(),
        )],
    };
    let summary = investigation.coverage_prompt_summary();
    // #6078: the exec-summary surface carries the SAME figure as the section.
    assert!(
        summary.contains("examined 12 of 200 files (6.0%)"),
        "prompt summary missing the percentage: {summary}"
    );
    assert!(summary.contains("not investigated: scalability"));
    assert!(summary.contains("Synthesise the executive summary FROM those findings"));
}

/// #6080: one concept, one number, one label — on both surfaces.
///
/// Why: the coverage section counted evidence-backed findings while the
/// synthesis prompt counted all of them, so one report's executive summary
/// said "102 verified findings" and its coverage section said "79 verified
/// evidence-backed findings" about the same set.
/// What: with one RED and one GREEN, both surfaces state the same triple.
#[test]
fn coverage_prompt_summary_and_section_agree_on_counts() {
    let mut green = finding();
    green.severity = Severity::Green;
    green.evidence_quote = String::new();
    let investigation = Investigation {
        repos: vec![repo(
            InvestigationStatus::Available,
            vec![finding(), green],
            DependencyInventory::default(),
        )],
    };
    let counts = "2 verified finding(s) (1 RED/AMBER evidence-backed, 1 clean signals)";
    let summary = investigation.coverage_prompt_summary();
    assert!(summary.contains(counts), "prompt summary: {summary}");
    let section = coverage_section(&empty_model(), &investigation);
    assert!(
        section.contains("verified findings: 2 (1 RED/AMBER evidence-backed, 1 clean signals)"),
        "coverage section: {section}"
    );
}

/// Why: a repository with nothing tracked would divide by zero; `0.0` is the
/// correct reading (nothing was examined) and must not become `NaN` on the page.
/// What: `coverage_pct` on a zero denominator, and the rounding of a third.
/// Test: this test itself.
#[test]
fn coverage_percentage_handles_empty_repo() {
    assert_eq!(coverage_pct(0, 0), "0.0");
    assert_eq!(coverage_pct(1, 3), "33.3");
    assert_eq!(coverage_pct(200, 200), "100.0");
}

/// Why: no dependencies must render an explicit empty note, not a broken table.
/// What: an all-empty inventory renders the "no dependencies" line.
/// Test: this test itself.
#[test]
fn empty_inventory_with_no_manifest_is_a_named_gap() {
    let investigation = Investigation {
        repos: vec![repo(
            InvestigationStatus::Available,
            vec![],
            DependencyInventory::default(),
        )],
    };
    let out = dependency_section(&investigation);
    assert!(
        out.contains("Dependency manifests not examined"),
        "out: {out}"
    );
    assert!(
        out.contains("unassessed, not as a dependency-free codebase"),
        "out: {out}"
    );
    assert!(
        !out.contains("No manifest-declared dependencies were found"),
        "#6137: the false clean claim must be gone — out: {out}"
    );
}

/// #6137: a manifest that WAS read and declares nothing is a different fact
/// from a root where nothing was read, and the section names which manifest.
#[test]
fn empty_inventory_names_the_manifests_it_read() {
    let investigation = Investigation {
        repos: vec![repo(
            InvestigationStatus::Available,
            vec![],
            DependencyInventory {
                manifests_examined: vec!["Cargo.toml".to_string()],
                ..Default::default()
            },
        )],
    };
    let out = dependency_section(&investigation);
    assert!(out.contains("Cargo.toml"), "out: {out}");
    assert!(
        out.contains("no directly declared dependencies"),
        "out: {out}"
    );
}

/// Why: a truncated/failed batch must be NAMED — which files, which position,
/// why — never silently lowering the finding count (#2357 wave-3.1 regression
/// test for the live-QA incident).
/// What: a repo with 4 batches, one truncated, renders the batch total line and
/// a named bullet with the file list and reason.
/// Test: this test itself.
#[test]
fn coverage_section_reports_batch_failure() {
    let mut repo_inv = repo(
        InvestigationStatus::Available,
        vec![finding()],
        DependencyInventory::default(),
    );
    repo_inv.coverage.batches_total = 4;
    repo_inv.coverage.batches_succeeded = 3;
    repo_inv.coverage.batches_failed = vec![BatchNote {
        index: 2,
        batch_count: 4,
        files: vec!["src/b.rs".to_string(), "src/b2.rs".to_string()],
        reason: "truncated (even after concise retry)".to_string(),
    }];
    let investigation = Investigation {
        repos: vec![repo_inv],
    };
    let out = coverage_section(&empty_model(), &investigation);
    assert!(out.contains("batches: 4 total, 3 succeeded, 1 truncated/failed"));
    assert!(out.contains("batch 2 of 4"));
    assert!(out.contains("truncated (even after concise retry)"));
    assert!(out.contains("src/b.rs, src/b2.rs"));
}

/// Why: the synthesis-prompt digest must carry the SAME batch-failure fact so
/// the exec summary can only lament a gap for a genuinely failed batch and must
/// name it (the coordinator's exact requirement).
/// What: a batch-failure note appears in `coverage_prompt_summary`'s output.
/// Test: this test itself.
#[test]
fn coverage_prompt_summary_names_failed_batch() {
    let mut repo_inv = repo(
        InvestigationStatus::Available,
        vec![finding()],
        DependencyInventory::default(),
    );
    repo_inv.coverage.batches_total = 4;
    repo_inv.coverage.batches_succeeded = 3;
    repo_inv.coverage.batches_failed = vec![BatchNote {
        index: 2,
        batch_count: 4,
        files: vec!["src/b.rs".to_string()],
        reason: "truncated (even after concise retry)".to_string(),
    }];
    let investigation = Investigation {
        repos: vec![repo_inv],
    };
    let summary = investigation.coverage_prompt_summary();
    assert!(summary.contains("batch 2 of 4 truncated/failed"));
    assert!(summary.contains("truncated (even after concise retry)"));
}

// ─── Verified-vs-rendered reconciliation (#6082 lap 7) ───────────────────────

/// A repository whose metrics carry `red`/`amber`/`green` rendered entries.
fn model_rendering(red: usize, amber: usize, green: usize) -> crate::report::model::ReportModel {
    let mut model = empty_model();
    let mut findings = Vec::new();
    for (n, severity) in [
        (red, Severity::Red),
        (amber, Severity::Amber),
        (green, Severity::Green),
    ] {
        for i in 0..n {
            findings.push(crate::report::metrics::MetricFinding {
                title: format!("{severity:?} {i}"),
                severity,
                category: "maintainability".to_string(),
                component: format!("src/f{i}.rs"),
                description: String::new(),
                remediation: String::new(),
            });
        }
    }
    model
        .repositories
        .push(crate::report::model::RepositoryReport {
            name: "App".to_string(),
            slug: "app".to_string(),
            source: "/tmp/app".to_string(),
            source_kind: "local_path".to_string(),
            username: None,
            git_ref: None,
            git_info: None,
            local_path: None,
            scan: None,
            metrics: Some(crate::report::metrics::AnalyzeMetrics {
                findings,
                ..Default::default()
            }),
            analyze_gap: None,
            authorship: None,
            inspect_priority: Vec::new(),
            crate_topology: None,
        });
    model
}

/// The coverage section states how the verified count adds up to the rendered
/// one.
///
/// Fails before the fix: the section said "verified findings: 3" beside a
/// section 5.2 carrying 5 entries, with nothing on the page connecting them.
#[test]
fn coverage_reconciles_verified_against_rendered() {
    let mut repo_inv = repo(
        InvestigationStatus::Available,
        vec![finding(), finding(), finding()],
        DependencyInventory::default(),
    );
    repo_inv.slug = "app".to_string();
    let inv = Investigation {
        repos: vec![repo_inv],
    };
    let out = coverage_section(&model_rendering(1, 3, 1), &inv);
    assert!(
        out.contains(
            "- section 5.2 reconciliation: 3 investigation-verified finding(s) + 2 finding(s) \
             trusty-analyze measured mechanically = 5 entries rendered (1 RED, 3 AMBER, 1 GREEN)"
        ),
        "the reconciliation must state numbers that sum:\n{out}"
    );
}

/// A rendered set smaller than the verified set is an arithmetic this line
/// cannot explain, so it renders nothing rather than a false sum.
#[test]
fn coverage_omits_the_reconciliation_when_it_would_not_add_up() {
    let mut repo_inv = repo(
        InvestigationStatus::Available,
        vec![finding(), finding(), finding()],
        DependencyInventory::default(),
    );
    repo_inv.slug = "app".to_string();
    let inv = Investigation {
        repos: vec![repo_inv],
    };
    let out = coverage_section(&model_rendering(0, 1, 0), &inv);
    assert!(!out.contains("reconciliation"), "{out}");
}
