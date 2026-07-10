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
    assert!(out.contains("files examined: 12 of 200"));
    assert!(out.contains("NOT investigated: scalability"));
    assert!(out.contains("2 finding(s) rejected (unverifiable evidence)"));
    assert!(out.contains("verified evidence-backed findings: 1"));
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
    assert!(summary.contains("examined 12 of 200 files"));
    assert!(summary.contains("not investigated: scalability"));
    assert!(summary.contains("Synthesise the executive summary FROM those findings"));
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
