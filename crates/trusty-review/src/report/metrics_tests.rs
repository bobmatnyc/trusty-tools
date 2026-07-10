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
