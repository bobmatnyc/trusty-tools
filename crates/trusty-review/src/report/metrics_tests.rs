//! Tests for the v0 trusty-analyze metrics schema and loader.
//!
//! Why: the schema must parse both a full analyzer document and a minimal `{}`
//! (forward-compatibility for partial output), and the derived helpers must be
//! stable since they feed per-application template fields.
//! What: covers full parse, minimal parse, language ordering, and disk load.
//! Test: included as `#[cfg(test)] mod tests` from `metrics.rs`.

use super::{AnalyzeMetrics, Severity, load_metrics};

const FULL: &str = r#"{
  "schema_version": "v0",
  "repository": "acme-web",
  "loc": { "total": 1200, "by_language": [
    { "language": "TypeScript", "loc": 800 },
    { "language": "Rust", "loc": 400 }
  ]},
  "counts": { "files": 42, "functions": 310 },
  "complexity": { "buckets": [
    { "label": "low (1-5)", "count": 250 },
    { "label": "high (>20)", "count": 12 }
  ]},
  "findings": [
    { "title": "SQL injection", "severity": "red", "category": "security", "component": "db.rs" }
  ]
}"#;

/// Why: a full analyzer document must deserialize into every field.
/// What: parses `FULL` and asserts LoC, counts, buckets, and finding severity.
/// Test: this test itself.
#[test]
fn parse_full_metrics() {
    let m: AnalyzeMetrics = serde_json::from_str(FULL).expect("parse full");
    assert_eq!(m.loc.total, 1200);
    assert_eq!(m.loc.by_language.len(), 2);
    assert_eq!(m.counts.files, 42);
    assert_eq!(m.counts.functions, 310);
    assert_eq!(m.complexity.buckets.len(), 2);
    assert_eq!(m.findings.len(), 1);
    assert_eq!(m.findings[0].severity, Severity::Red);
}

/// Why: partial analyzer output must not break the parse (all fields default).
/// What: parses `{}` and asserts zeroed totals and empty collections.
/// Test: this test itself.
#[test]
fn parse_minimal_metrics() {
    let m: AnalyzeMetrics = serde_json::from_str("{}").expect("parse minimal");
    assert_eq!(m.loc.total, 0);
    assert!(m.loc.by_language.is_empty());
    assert!(m.findings.is_empty());
}

/// Why: the tech-stack field wants the largest languages first.
/// What: asserts `primary_languages` orders by descending LoC and truncates.
/// Test: this test itself.
#[test]
fn primary_languages_orders_by_loc() {
    let m: AnalyzeMetrics = serde_json::from_str(FULL).expect("parse");
    let langs = m.primary_languages(1);
    assert_eq!(langs, vec!["TypeScript".to_string()]);
    let all = m.primary_languages(10);
    assert_eq!(all, vec!["TypeScript".to_string(), "Rust".to_string()]);
}

/// Why: the loader must read + parse a metrics file from disk with typed errors.
/// What: writes `FULL` to a temp file and loads it back.
/// Test: this test itself.
#[test]
fn load_roundtrip() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let path = tmp.path().join("metrics.json");
    std::fs::write(&path, FULL).expect("write");
    let m = load_metrics(&path).expect("load ok");
    assert_eq!(m.repository, "acme-web");
}

/// Write `body` to a fresh `metrics.json` and hand back the tempdir plus path.
///
/// The tempdir is returned because dropping it deletes the file.
fn artifact(body: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let path = tmp.path().join("metrics.json");
    std::fs::write(&path, body).expect("write");
    (tmp, path)
}

/// #5747: the producer of this artifact and its reader are separate binaries, so
/// a tag naming a major this build cannot read must fail the way an unparseable
/// file does. Every field carries `#[serde(default)]`, so a renamed key
/// otherwise parses to zero and renders `0 lines of code` as a stated fact in a
/// document an acquirer prices a deal from.
#[test]
fn an_artifact_from_an_unknown_schema_major_is_a_named_error() {
    // Well-formed JSON; `total` renamed the way a v9 producer might have.
    let (_tmp, path) = artifact(
        r#"{"schema_version": "v9", "repository": "acme-web",
             "loc": {"total_lines": 1200}}"#,
    );

    let err = load_metrics(&path).expect_err("must refuse a major this build cannot read");
    let rendered = err.to_string();
    assert!(
        rendered.contains("metrics.json") && rendered.contains("schema_version"),
        "the error must name the artifact and the version mismatch: {rendered}"
    );
    assert!(
        rendered.contains("v9"),
        "the error must quote the version it refused: {rendered}"
    );
}

/// A tag this build cannot resolve to a major at all is refused for the same
/// reason a wrong major is: the loader cannot claim the file is v0, and
/// assuming it is renders whatever `#[serde(default)]` produced.
#[test]
fn an_artifact_with_an_uninterpretable_schema_tag_is_a_named_error() {
    let (_tmp, path) = artifact(r#"{"schema_version": "analyze-2027", "counts": {"files": 42}}"#);

    let err = load_metrics(&path).expect_err("must refuse a tag it cannot resolve to a major");
    assert!(
        err.to_string().contains("analyze-2027"),
        "the error must quote the tag it refused: {err}"
    );
}

/// A newer MINOR of a known major still loads — that is exactly the added-field
/// case every field's `#[serde(default)]` exists for. Refusing it would make an
/// additive producer change require a coordinated release of both binaries.
#[test]
fn an_artifact_with_a_newer_minor_of_a_known_major_still_parses() {
    let (_tmp, path) = artifact(
        r#"{"schema_version": "v0.3", "repository": "acme-web",
             "counts": {"files": 42, "functions": 310}, "future_key": true}"#,
    );

    let m = load_metrics(&path).expect("a newer minor of a known major must load");
    assert_eq!(m.repository, "acme-web");
    assert_eq!(m.counts.files, 42);
}

/// #5747: unlike the ticketing artifact, nothing in this workspace writes a
/// metrics JSON — DOC-67 §7 has `tga audit` omit `RepositoryEntry.metrics`
/// outright, and the field has been documented as "informational" since #2317.
/// An untagged file is therefore an author who followed the documentation, not
/// a truncated one, and v0 is the only schema this artifact has ever had.
#[test]
fn an_untagged_artifact_is_read_as_v0() {
    let (_tmp, path) = artifact(r#"{"repository": "acme-web", "loc": {"total": 1200}}"#);

    let m = load_metrics(&path).expect("an untagged artifact predates the tag and must still load");
    assert_eq!(m.repository, "acme-web");
    assert_eq!(m.loc.total, 1200);
}
