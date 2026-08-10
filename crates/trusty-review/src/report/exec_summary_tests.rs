//! Tests for the deterministic Executive Summary roll-up (#5318).
//!
//! Why: §2 renders whatever this module returns, so the wording, the counting,
//! and — most of all — the two-way split between "here is the roll-up" and
//! "here is exactly which input was missing" are pinned here rather than
//! through a template render.
//! What: composes over models built from in-memory manifests + metrics
//! documents, and asserts the sentences, the provenance, the unavailable
//! statements, and the Top Risks derivation.
//! Test: included as `#[cfg(test)] mod tests` from `exec_summary.rs`.

use super::{ExecSummary, compose, top_risks};
use crate::report::manifest::parse_manifest;
use crate::report::model::ReportModel;
use crate::report::provenance::Provenance;

/// Build a model from a manifest TOML, writing each `(filename, json)` metrics
/// document into `dir` first so relative manifest paths resolve.
fn model(dir: &std::path::Path, metrics: &[(&str, &str)], toml: &str) -> ReportModel {
    for (name, json) in metrics {
        std::fs::write(dir.join(name), json).expect("write metrics");
    }
    let manifest_path = dir.join("manifest.toml");
    let manifest = parse_manifest(toml, &manifest_path).expect("manifest parse");
    ReportModel::build(&manifest, &manifest_path, "report-technical-dd", None).expect("build model")
}

const ACME_METRICS: &str = r#"{
  "loc": { "total": 8200, "by_language": [
    { "language": "Rust", "loc": 6000 },
    { "language": "TypeScript", "loc": 2200 }
  ]},
  "counts": { "files": 120, "functions": 640 },
  "findings": [
    { "title": "Hardcoded credential", "severity": "red", "category": "security",
      "component": "src/config.rs:42", "description": "literal API key on a static" },
    { "title": "Cyclomatic complexity 31", "severity": "amber", "category": "maintainability",
      "component": "src/router.rs:118" },
    { "title": "Strong coverage", "severity": "green", "category": "quality", "component": "" }
  ]
}"#;

const ACME_MANIFEST: &str = r#"
    [report]
    title = "Acme Due Diligence"

    [[repositories]]
    name = "Acme Web"
    path = "/nonexistent/acme-web"
    metrics = "acme.json"
"#;

/// Why: the exact defect in #5318 — a run carrying RED/AMBER findings must
/// produce a paragraph, never a placeholder.
/// What: composes over one application with size, languages, and findings, and
/// asserts every clause plus the declared provenance (the figures came from a
/// metrics document, not this tool's own scan).
/// Test: this test itself.
#[test]
fn composes_from_metrics_findings() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let m = model(tmp.path(), &[("acme.json", ACME_METRICS)], ACME_MANIFEST);

    let ExecSummary::Composed { text, provenance } = compose(&m) else {
        panic!("expected a composed summary, got {:?}", compose(&m));
    };
    assert_eq!(provenance, Provenance::Declared);
    assert!(
        text.contains("covered 1 application (Acme Web)"),
        "scope sentence missing: {text}"
    );
    assert!(
        text.contains("8200 lines of code across Rust and TypeScript"),
        "size/language clause missing: {text}"
    );
    assert!(text.contains("in 120 files"), "file count missing: {text}");
    assert!(
        text.contains("1 RED (critical) and 1 AMBER (medium-risk) findings"),
        "severity counts missing: {text}"
    );
    assert!(
        text.contains("concentrated in maintainability (1) and security (1)"),
        "dimension breakdown missing: {text}"
    );
    assert!(
        text.contains("deterministic roll-up"),
        "roll-up note missing: {text}"
    );
    // GREEN findings never inflate the counts (the no-green-analysis rule).
    assert!(!text.contains("GREEN"), "greens leaked into §2: {text}");
    // With one application there is no concentration to point at.
    assert!(
        !text.contains("carries the most"),
        "single-application report needs no concentration sentence: {text}"
    );
}

/// Why: a local checkout with no metrics document still has measured size, and
/// the paragraph must say so with `measured` provenance rather than claiming a
/// declared input it never had.
/// What: points a repository at this crate's own source tree (a real, scannable
/// directory) with no `metrics` key and asserts a composed, measured summary
/// with no findings claim.
/// Test: this test itself.
#[test]
fn composes_from_scan_only() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let crate_dir = env!("CARGO_MANIFEST_DIR");
    let toml = format!(
        r#"
        [report]
        title = "Scan Only"

        [[repositories]]
        name = "Self"
        path = "{crate_dir}"
    "#
    );
    let m = model(tmp.path(), &[], &toml);

    let ExecSummary::Composed { text, provenance } = compose(&m) else {
        panic!("expected a composed summary for a scannable checkout");
    };
    assert_eq!(provenance, Provenance::Measured);
    assert!(text.contains("lines of code"), "no measured size: {text}");
    assert!(
        text.contains("raised no RED or AMBER findings"),
        "posture sentence missing: {text}"
    );
}

/// Why: closure condition 2 of #5318 — when §2 genuinely cannot be produced,
/// the reader must be told which input was absent, not pointed at Gaps.
/// What: a remote-only repository (no metrics file, no local checkout to scan)
/// and asserts each named input appears in the statement.
/// Test: this test itself.
#[test]
fn unavailable_names_the_missing_inputs() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let toml = r#"
        [report]
        title = "Remote Only"

        [[repositories]]
        name = "Remote App"
        remote = "acme/remote-app"
    "#;
    let m = model(tmp.path(), &[], toml);

    let ExecSummary::Unavailable(text) = compose(&m) else {
        panic!("a remote-only report has nothing to roll up");
    };
    assert!(
        text.contains("`metrics` file"),
        "metrics input unnamed: {text}"
    );
    assert!(
        text.contains("`--analyze`"),
        "analyze input unnamed: {text}"
    );
    assert!(
        text.contains("no local checkout was scannable"),
        "scan input unnamed: {text}"
    );
    assert!(
        !text.contains("No data available"),
        "must not fall back to the generic placeholder: {text}"
    );
}

/// Why: a report with zero repositories is a different missing input from an
/// unmeasured one and must say so — otherwise it reads "none of the 0
/// application(s)". The manifest loader rejects an empty `[[repositories]]`
/// list, so this state is only reachable through the public [`ReportModel`]
/// struct, which is exactly why the guard exists.
/// What: empties a built model's repository list and composes over it.
/// Test: this test itself.
#[test]
fn unavailable_with_no_repositories() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let mut m = model(tmp.path(), &[("acme.json", ACME_METRICS)], ACME_MANIFEST);
    m.repositories.clear();

    let ExecSummary::Unavailable(text) = compose(&m) else {
        panic!("a report with no repositories has nothing to summarize");
    };
    assert!(text.contains("declared no repositories"), "got: {text}");
}

/// Why: with several applications a reader wants to know where the risk sits,
/// and the sentence must be stable across runs.
/// What: two applications with different RED counts; asserts the concentration
/// sentence names the heavier one with its share.
/// Test: this test itself.
#[test]
fn names_the_application_carrying_the_most_red_findings() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let heavy = r#"{
      "loc": { "total": 100, "by_language": [{ "language": "Rust", "loc": 100 }] },
      "findings": [
        { "title": "A", "severity": "red", "category": "security", "component": "a.rs" },
        { "title": "B", "severity": "red", "category": "security", "component": "b.rs" }
      ]
    }"#;
    let light = r#"{
      "loc": { "total": 50, "by_language": [{ "language": "Go", "loc": 50 }] },
      "findings": [
        { "title": "C", "severity": "red", "category": "security", "component": "c.go" }
      ]
    }"#;
    let toml = r#"
        [report]
        title = "Two Apps"

        [[repositories]]
        name = "Heavy"
        path = "/nonexistent/heavy"
        metrics = "heavy.json"

        [[repositories]]
        name = "Light"
        path = "/nonexistent/light"
        metrics = "light.json"
    "#;
    let m = model(
        tmp.path(),
        &[("heavy.json", heavy), ("light.json", light)],
        toml,
    );

    let text = compose(&m).text().to_string();
    assert!(
        text.contains("Heavy carries the most RED findings (2 of 3)"),
        "concentration sentence wrong: {text}"
    );
    assert!(
        text.contains("covered 2 applications (Heavy, Light)"),
        "scope sentence wrong: {text}"
    );
}

/// The single-application risks manifest used by the cap tests.
const RISKS_MANIFEST: &str = r#"
    [report]
    title = "Risks"

    [[repositories]]
    name = "Acme Web"
    path = "/nonexistent/acme-web"
    metrics = "risks.json"
"#;

/// Why: the Top Risks table sits inside §2 and collapsed for the same reason
/// the paragraph did; its rows come from the same findings, worst first, and a
/// truncated table must say so.
/// What: SEVEN non-GREEN findings (2 RED + 5 AMBER) against a cap of five, so
/// two are provably dropped — a fixture sized exactly at the cap could not tell
/// a working cap from no cap at all. Asserts RED-first ordering, the dropped
/// findings' absence by title, the pre-cap `eligible` count, the truncation
/// caption, that GREENs never appear, and the row text.
/// Test: this test itself.
#[test]
fn top_risks_are_red_first_and_capped() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let metrics = r#"{
      "findings": [
        { "title": "A1", "severity": "amber", "category": "x", "component": "a1.rs" },
        { "title": "A2", "severity": "amber", "category": "x", "component": "a2.rs" },
        { "title": "A3", "severity": "amber", "category": "x", "component": "a3.rs" },
        { "title": "A4", "severity": "amber", "category": "x", "component": "a4.rs" },
        { "title": "A5", "severity": "amber", "category": "x", "component": "a5.rs" },
        { "title": "R1", "severity": "red", "category": "security", "component": "r1.rs",
          "description": "leaks a key" },
        { "title": "R2", "severity": "red", "category": "security", "component": "r2.rs" },
        { "title": "G1", "severity": "green", "category": "quality", "component": "" }
      ]
    }"#;
    let m = model(tmp.path(), &[("risks.json", metrics)], RISKS_MANIFEST);

    let risks = top_risks(&m);
    assert_eq!(risks.eligible, 7, "pre-cap count wrong: {risks:?}");
    assert_eq!(risks.rows.len(), 5, "cap not applied: {risks:?}");

    // The cap actually dropped rows: the two lowest-priority AMBERs are gone.
    let titles: Vec<&str> = risks
        .rows
        .iter()
        .map(|r| r.description.split(' ').next().unwrap_or(""))
        .collect();
    assert!(
        !titles.contains(&"A4") && !titles.contains(&"A5"),
        "the cap kept findings it should have dropped: {titles:?}"
    );
    assert_eq!(titles, vec!["R1", "R2", "A1", "A2", "A3"]);

    assert_eq!(risks.rows[0].severity, "RED");
    assert_eq!(risks.rows[1].severity, "RED");
    assert!(risks.rows[2..].iter().all(|r| r.severity == "AMBER"));
    assert_eq!(risks.rows[0].description, "R1 — leaks a key (r1.rs)");
    assert_eq!(risks.rows[0].apps, "Acme Web");
    assert!(
        !titles.contains(&"G1"),
        "a GREEN finding must never become a top risk: {titles:?}"
    );

    let note = risks
        .truncation_note()
        .expect("truncated table needs a note");
    assert!(note.contains("Top 5 of 7"), "note wording: {note}");
    assert!(note.contains("2 further"), "hidden count wrong: {note}");
}

/// Why: the truncation caption must appear only when something was actually
/// dropped — a caption on a complete table would misinform in the other
/// direction.
/// What: exactly five non-GREEN findings, so nothing is cut; asserts every row
/// survives and no note is produced. The GREEN finding proves `eligible` counts
/// eligible ROWS, not total findings.
/// Test: this test itself.
#[test]
fn truncation_note_absent_when_nothing_was_dropped() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let metrics = r#"{
      "findings": [
        { "title": "A1", "severity": "amber", "category": "x", "component": "a1.rs" },
        { "title": "A2", "severity": "amber", "category": "x", "component": "a2.rs" },
        { "title": "A3", "severity": "amber", "category": "x", "component": "a3.rs" },
        { "title": "R1", "severity": "red", "category": "security", "component": "r1.rs" },
        { "title": "R2", "severity": "red", "category": "security", "component": "r2.rs" },
        { "title": "G1", "severity": "green", "category": "quality", "component": "" }
      ]
    }"#;
    let m = model(tmp.path(), &[("risks.json", metrics)], RISKS_MANIFEST);

    let risks = top_risks(&m);
    assert_eq!(risks.eligible, 5, "the GREEN must not count as eligible");
    assert_eq!(risks.rows.len(), 5);
    assert_eq!(risks.truncation_note(), None);
}
