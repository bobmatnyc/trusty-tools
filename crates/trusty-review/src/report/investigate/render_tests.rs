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
use crate::report::investigate::{BatchNote, Budget};
use crate::report::metrics::Severity;

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
    let out = coverage_section(&investigation);
    assert!(out.contains("## Investigation Coverage"));
    // #6078: the percentage is part of this line, not a new section.
    assert!(
        out.contains("files examined: 12 of 200 tracked (6.0% coverage, 188 skipped)"),
        "coverage line missing the percentage: {out}"
    );
    assert!(out.contains("NOT investigated: scalability"));
    assert!(out.contains("2 finding(s) rejected (unverifiable evidence)"));
    assert!(out.contains("verified evidence-backed findings: 1"));
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
    let out = coverage_section(&inv);
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
        !coverage_section(&full).contains("attributed-only selection"),
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
    let out = coverage_section(&discovered(
        9,
        vec![DimensionCoverage {
            dimension: "error handling".to_string(),
            files_examined: 4,
            example: Some(
                "src/err.rs (trusty-search hit for \"error swallowed\" (score 0.77, line 12))"
                    .to_string(),
            ),
        }],
    ));
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
    let out = coverage_section(&discovered(0, Vec::new()));
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
    let out = coverage_section(&investigation);
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
fn dependency_section_empty_note() {
    let investigation = Investigation {
        repos: vec![repo(
            InvestigationStatus::Available,
            vec![],
            DependencyInventory::default(),
        )],
    };
    let out = dependency_section(&investigation);
    assert!(out.contains("No manifest-declared dependencies"));
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
    let out = coverage_section(&investigation);
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
