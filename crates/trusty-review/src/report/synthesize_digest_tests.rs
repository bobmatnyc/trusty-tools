//! Tests for the compact findings digest (#2357 follow-up).
//!
//! Why: this module is the direct fix for the acceptance-QA regression — the
//! top-level synthesis call blowing its output ceiling once wave-3 verified
//! dozens of findings.  These tests prove: description truncation, the
//! context/elaboration caps + honest tail notes, severity ordering, that
//! investigation-verified findings are excluded from elaboration entirely, and
//! that the rendered digest stays small even at 100 synthetic findings.
//! Test: included as `#[cfg(test)] mod tests` from `synthesize_digest.rs`.

use super::*;
use crate::report::investigate::{Budget, Coverage, Investigation, InvestigationStatus};
use crate::report::investigate::{RepoInvestigation, VerifiedFinding};
use crate::report::metrics::{AnalyzeMetrics, MetricFinding};
use crate::report::model::{ReportModel, RepositoryReport};

// ── Fixtures ─────────────────────────────────────────────────────────────────

fn mf(title: &str, severity: Severity, category: &str, component: &str) -> MetricFinding {
    MetricFinding {
        title: title.to_string(),
        severity,
        category: category.to_string(),
        component: component.to_string(),
    }
}

fn repo(slug: &str, findings: Vec<MetricFinding>) -> RepositoryReport {
    RepositoryReport {
        name: format!("App {slug}"),
        slug: slug.to_string(),
        source: "local".to_string(),
        source_kind: "local_path".to_string(),
        username: None,
        git_ref: None,
        git_info: None,
        local_path: None,
        scan: None,
        metrics: Some(AnalyzeMetrics {
            findings,
            ..Default::default()
        }),
    }
}

fn model_with(repos: Vec<RepositoryReport>, investigation: Option<Investigation>) -> ReportModel {
    ReportModel {
        title: "T".to_string(),
        template: "report-technical-dd".to_string(),
        analyst: None,
        client: None,
        vendor_methodology: crate::report::model::vendor_methodology(),
        instructions: None,
        instructions_source: None,
        report_date: "2026-07-10".to_string(),
        generated_date: "2026-07-10".to_string(),
        manifest_path: "m.toml".to_string(),
        repositories: repos,
        synthesis: None,
        benchmark: None,
        investigation,
        section_instructions: Default::default(),
    }
}

fn verified_finding(title: &str, file: &str, line: u64, description: &str) -> VerifiedFinding {
    VerifiedFinding {
        title: title.to_string(),
        severity: Severity::Red,
        dimension: "authentication & secrets".to_string(),
        file: file.to_string(),
        line: Some(line),
        evidence_quote: "q".to_string(),
        description: description.to_string(),
        business_impact: "impact".to_string(),
        remediation: "fix it".to_string(),
        cost_effort: "low".to_string(),
    }
}

fn coverage() -> Coverage {
    Coverage {
        budget: Budget::default(),
        ..Default::default()
    }
}

// ── Truncation ───────────────────────────────────────────────────────────────

/// Why: an over-length description must truncate with an ellipsis, never blow
/// the digest's per-finding byte budget.
/// What: a 200-char string truncates to 140 chars + "…".
/// Test: this test itself.
#[test]
fn truncates_long_description() {
    let long = "x".repeat(200);
    let out = truncate_description(&long, DESCRIPTION_TRUNCATE_CHARS);
    assert_eq!(out.chars().count(), DESCRIPTION_TRUNCATE_CHARS + 1);
    assert!(out.ends_with('…'));
}

/// Why: a short description must pass through unchanged (no needless marker).
/// What: a 10-char string is returned as-is.
/// Test: this test itself.
#[test]
fn short_description_unchanged() {
    assert_eq!(truncate_description("short one", 140), "short one");
}

// ── Gathering + enrichment ───────────────────────────────────────────────────

/// Why: greens must never reach the digest (structural no-green rule).
/// What: a model with one RED and one GREEN finding yields only the RED.
/// Test: this test itself.
#[test]
fn excludes_greens() {
    let model = model_with(
        vec![repo(
            "a",
            vec![
                mf("Red one", Severity::Red, "auth", "auth.rs"),
                mf("Green one", Severity::Green, "maintainability", "core.rs"),
            ],
        )],
        None,
    );
    let compact = gather_compact_findings(&model);
    assert_eq!(compact.len(), 1);
    assert_eq!(compact[0].title, "Red one");
}

/// Why: a wave-3-verified finding must be enriched with its truncated
/// description, its `file:line`, and marked `verified`.
/// What: a metric finding whose title matches an investigation-verified one
/// picks up the enriched fields.
/// Test: this test itself.
#[test]
fn enriches_from_investigation() {
    let inv = Investigation {
        repos: vec![RepoInvestigation {
            slug: "a".to_string(),
            name: "App a".to_string(),
            status: InvestigationStatus::Available,
            findings: vec![verified_finding(
                "SQL injection",
                "src/db.rs",
                42,
                "Raw query concatenation observed in the handler.",
            )],
            deps: Default::default(),
            coverage: coverage(),
        }],
    };
    let model = model_with(
        vec![repo(
            "a",
            vec![mf("SQL injection", Severity::Red, "security", "db.rs")],
        )],
        Some(inv),
    );
    let compact = gather_compact_findings(&model);
    assert_eq!(compact.len(), 1);
    assert!(compact[0].verified);
    assert_eq!(compact[0].file, "src/db.rs:42");
    assert_eq!(
        compact[0].description,
        "Raw query concatenation observed in the handler."
    );
}

/// Why: a finding with no investigation counterpart (e.g. pure trusty-analyze
/// metrics, no `--synthesize` investigation) must fall back to the bare metric
/// fields, unchanged from pre-#2357-follow-up behaviour.
/// What: no `model.investigation` present → description empty, file = component,
/// verified = false.
/// Test: this test itself.
#[test]
fn falls_back_without_investigation() {
    let model = model_with(
        vec![repo(
            "a",
            vec![mf("Legacy finding", Severity::Amber, "quality", "core.rs")],
        )],
        None,
    );
    let compact = gather_compact_findings(&model);
    assert_eq!(compact.len(), 1);
    assert!(!compact[0].verified);
    assert_eq!(compact[0].file, "core.rs");
    assert!(compact[0].description.is_empty());
}

// ── Cap + severity ordering ──────────────────────────────────────────────────

/// Why: the context digest must show RED before AMBER and cap at 40 with an
/// honest overflow count — the direct fix for the acceptance-QA blowup.
/// What: 45 findings (5 AMBER first, then 40 RED) → the 40 shown are ALL RED,
/// and `context_overflow == 5` (the AMBER ones, since they rank lower).
/// Test: this test itself.
#[test]
fn caps_context_at_40_red_first() {
    let mut findings = Vec::new();
    for i in 0..5 {
        findings.push(mf(&format!("amber-{i}"), Severity::Amber, "cat", "c.rs"));
    }
    for i in 0..40 {
        findings.push(mf(&format!("red-{i}"), Severity::Red, "cat", "c.rs"));
    }
    let model = model_with(vec![repo("a", findings)], None);
    let compact = gather_compact_findings(&model);
    assert_eq!(compact.len(), 45);

    let split = build_split(&compact);
    assert_eq!(split.context.len(), CONTEXT_FINDINGS_CAP);
    assert_eq!(split.context_overflow, 5);
    assert!(
        split.context.iter().all(|f| f.severity == Severity::Red),
        "all 40 shown must be RED (higher severity ranks first)"
    );
}

/// Why: elaboration targets must exclude verified findings entirely and cap at
/// 10, regardless of how many verified findings exist alongside them.
/// What: 3 verified + 15 unverified → elaboration_targets has 10 (all
/// unverified), overflow 5; context still shows all 18.
/// Test: this test itself.
#[test]
fn elaboration_targets_exclude_verified_and_cap_at_10() {
    let mut metric_findings = Vec::new();
    for i in 0..3 {
        metric_findings.push(mf(&format!("verified-{i}"), Severity::Red, "auth", "a.rs"));
    }
    for i in 0..15 {
        metric_findings.push(mf(
            &format!("plain-{i}"),
            Severity::Amber,
            "quality",
            "c.rs",
        ));
    }
    let inv = Investigation {
        repos: vec![RepoInvestigation {
            slug: "a".to_string(),
            name: "App a".to_string(),
            status: InvestigationStatus::Available,
            findings: (0..3)
                .map(|i| verified_finding(&format!("verified-{i}"), "a.rs", 1, "d"))
                .collect(),
            deps: Default::default(),
            coverage: coverage(),
        }],
    };
    let model = model_with(vec![repo("a", metric_findings)], Some(inv));
    let compact = gather_compact_findings(&model);
    assert_eq!(compact.iter().filter(|f| f.verified).count(), 3);

    let split = build_split(&compact);
    assert_eq!(
        split.context.len(),
        18,
        "context shows everything (capped at 40)"
    );
    assert_eq!(split.elaboration_targets.len(), 10);
    assert_eq!(split.elaboration_overflow, 5);
    assert!(
        split.elaboration_targets.iter().all(|f| !f.verified),
        "verified findings must never appear as elaboration targets"
    );
}

// ── Rendering ────────────────────────────────────────────────────────────────

/// Why: the tail note must name the count of additional findings, never just
/// silently truncate the list.
/// What: an overflowing context split renders the "… and N additional" note.
/// Test: this test itself.
#[test]
fn renders_context_with_tail_note() {
    let mut findings = Vec::new();
    for i in 0..42 {
        findings.push(mf(&format!("f-{i}"), Severity::Red, "cat", "c.rs"));
    }
    let model = model_with(vec![repo("a", findings)], None);
    let compact = gather_compact_findings(&model);
    let split = build_split(&compact);
    let rendered = render_context_section(&split);
    assert!(rendered.contains("40 of 42 shown"));
    assert!(rendered.contains("2 additional lower-severity finding(s)"));
}

/// Why: when investigation verified everything, the model must be told
/// explicitly to return an empty `findings` array rather than left guessing.
/// What: all-verified findings render the "none" elaboration line.
/// Test: this test itself.
#[test]
fn renders_elaboration_targets_or_none() {
    let inv = Investigation {
        repos: vec![RepoInvestigation {
            slug: "a".to_string(),
            name: "App a".to_string(),
            status: InvestigationStatus::Available,
            findings: vec![verified_finding("Only finding", "a.rs", 1, "d")],
            deps: Default::default(),
            coverage: coverage(),
        }],
    };
    let model = model_with(
        vec![repo(
            "a",
            vec![mf("Only finding", Severity::Red, "auth", "a.rs")],
        )],
        Some(inv),
    );
    let compact = gather_compact_findings(&model);
    let split = build_split(&compact);
    let rendered = render_elaboration_section(&split);
    assert!(rendered.contains("none — every RED/AMBER finding already has verified"));

    // And the inverse: an unverified finding DOES appear as a target.
    let model2 = model_with(
        vec![repo(
            "a",
            vec![mf("Needs prose", Severity::Amber, "quality", "b.rs")],
        )],
        None,
    );
    let compact2 = gather_compact_findings(&model2);
    let split2 = build_split(&compact2);
    let rendered2 = render_elaboration_section(&split2);
    assert!(rendered2.contains("Needs prose"));
}

/// Why: this is the acceptance criterion in the task itself — the digest must
/// stay bounded even at 100 synthetic findings, never scale unboundedly.
/// What: 100 RED findings render a context + elaboration digest whose combined
/// byte size stays well under a generous safety bound (20 KB).
/// Test: this test itself.
#[test]
fn digest_stays_bounded_at_100_findings() {
    let mut findings = Vec::new();
    for i in 0..100 {
        findings.push(mf(
            &format!("finding-{i}"),
            Severity::Red,
            "authentication & secrets",
            &format!("src/file_{i}.rs"),
        ));
    }
    let model = model_with(vec![repo("a", findings)], None);
    let compact = gather_compact_findings(&model);
    assert_eq!(compact.len(), 100);

    let split = build_split(&compact);
    let rendered = format!(
        "{}{}",
        render_context_section(&split),
        render_elaboration_section(&split)
    );
    assert!(
        rendered.len() < 20_000,
        "digest for 100 findings must stay well-bounded, got {} bytes",
        rendered.len()
    );
    assert!(rendered.contains("60 additional")); // 100 - 40 context shown
}
