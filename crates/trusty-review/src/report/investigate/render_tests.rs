//! Tests for investigation rendering (#2357).
//!
//! Why: the Dependency Inventory and Investigation Coverage sections are the
//! report's honesty surface — they must state examined/total, dimensions reached,
//! rejected findings, and the dependency table (capped) exactly.
//! What: exercises the dependency section (with overflow), the coverage section
//! (examined/total, dimensions, rejected note), and the synthesis-prompt summary.
//! Test: included as `#[cfg(test)] mod tests` from `render.rs`.

use super::*;
use crate::report::investigate::Budget;
use crate::report::investigate::deps::{Dependency, DependencyInventory};
use crate::report::investigate::verify::VerifiedFinding;
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
