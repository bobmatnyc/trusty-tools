//! Unit tests for the trusty-analyze → `AnalyzeMetrics` adapter (#2447).
//!
//! Why: the mapping must be provable against fixture JSON with NO live daemon —
//! these tests pin the envelope parsing, the severity convention, the
//! complexity-bucket thresholds, and the fail-open behaviour.
//! What: drives the private mapping helpers and the public fetch seam directly.
//! Test: this file.

use super::*;

// ─── Severity map ────────────────────────────────────────────────────────────

#[test]
fn severity_map_diagnostics() {
    assert_eq!(map_diagnostic_severity("error"), Severity::Red);
    assert_eq!(map_diagnostic_severity("warning"), Severity::Amber);
    assert_eq!(map_diagnostic_severity("info"), Severity::Green);
    assert_eq!(map_diagnostic_severity("hint"), Severity::Green);
    assert_eq!(map_diagnostic_severity("UNKNOWN"), Severity::Green);
}

#[test]
fn severity_map_refactors() {
    assert_eq!(map_refactor_severity("critical"), Severity::Red);
    assert_eq!(map_refactor_severity("high"), Severity::Amber);
    assert_eq!(map_refactor_severity("medium"), Severity::Green);
    assert_eq!(map_refactor_severity("low"), Severity::Green);
}

// ─── Complexity buckets ──────────────────────────────────────────────────────

#[test]
fn buckets_follow_grade_thresholds() {
    // One value at each band boundary: 5→A, 6→B, 15→C, 16→D, 21→F.
    let dist = complexity_buckets(&[5, 6, 15, 16, 21, 3]);
    let by_label: std::collections::HashMap<&str, u64> = dist
        .buckets
        .iter()
        .map(|b| (b.label.as_str(), b.count))
        .collect();
    assert_eq!(by_label.get("A: simple (0-5)"), Some(&2)); // 5 and 3
    assert_eq!(by_label.get("B: moderate (6-10)"), Some(&1));
    assert_eq!(by_label.get("C: elevated (11-15)"), Some(&1));
    assert_eq!(by_label.get("D: high (16-20)"), Some(&1));
    assert_eq!(by_label.get("F: very high (>20)"), Some(&1));
}

#[test]
fn buckets_omit_empty_bands() {
    let dist = complexity_buckets(&[2, 3, 4]);
    assert_eq!(dist.buckets.len(), 1);
    assert_eq!(dist.buckets[0].label, "A: simple (0-5)");
    assert_eq!(dist.buckets[0].count, 3);
}

#[test]
fn buckets_empty_input_yields_no_buckets() {
    assert!(complexity_buckets(&[]).buckets.is_empty());
}

// ─── Finding synthesis ───────────────────────────────────────────────────────

#[test]
fn diagnostic_finding_synthesises_title() {
    let d: WireDiagnostic = serde_json::from_str(
        r#"{ "tool": "clippy", "file": "src/a.rs", "line": 3, "col": 1,
             "severity": "error", "code": "clippy::needless_return",
             "message": "unneeded return statement" }"#,
    )
    .unwrap();
    let f = diagnostic_finding(&d).expect("error → red finding");
    assert_eq!(f.title, "clippy::needless_return");
    assert_eq!(f.severity, Severity::Red);
    assert_eq!(f.category, "clippy");
    assert_eq!(f.component, "src/a.rs");
}

#[test]
fn diagnostic_finding_without_code_uses_tool_name() {
    let d: WireDiagnostic =
        serde_json::from_str(r#"{ "tool": "ruff", "file": "a.py", "severity": "warning" }"#)
            .unwrap();
    let f = diagnostic_finding(&d).expect("warning → amber");
    assert_eq!(f.title, "ruff diagnostic");
    assert_eq!(f.severity, Severity::Amber);
}

#[test]
fn diagnostic_finding_drops_green() {
    let d: WireDiagnostic =
        serde_json::from_str(r#"{ "tool": "ruff", "file": "a.py", "severity": "hint" }"#).unwrap();
    assert!(diagnostic_finding(&d).is_none());
}

#[test]
fn refactor_finding_synthesises_title() {
    let r: WireRefactor = serde_json::from_str(
        r#"{ "file": "src/cfg.rs", "function_name": "parse_config",
             "refactor_type": "extract_method", "severity": "critical" }"#,
    )
    .unwrap();
    let f = refactor_finding(&r).expect("critical → red");
    assert_eq!(f.title, "Extract method — parse_config");
    assert_eq!(f.severity, Severity::Red);
    assert_eq!(f.category, "maintainability");
    assert_eq!(f.component, "src/cfg.rs");
}

#[test]
fn refactor_finding_drops_green() {
    let r: WireRefactor = serde_json::from_str(
        r#"{ "file": "a.rs", "refactor_type": "reduce_nesting", "severity": "medium" }"#,
    )
    .unwrap();
    assert!(refactor_finding(&r).is_none());
}

// ─── Envelope + full mapping ─────────────────────────────────────────────────

#[test]
fn map_metrics_populates_complexity_and_findings() {
    // Real-shaped envelopes: hotspots carry the #2446 numbers.
    let hotspots: HotspotsEnvelope = serde_json::from_str(
        r#"{ "index_id": "demo", "top_n": 1000, "hotspots": [
             { "id": "a:1:9", "file": "a.rs", "start_line": 1, "end_line": 9,
               "content": "fn a() {}", "cyclomatic": 25, "cognitive": 30 },
             { "id": "b:1:4", "file": "b.rs", "start_line": 1, "end_line": 4,
               "content": "fn b() {}", "cyclomatic": 4, "cognitive": 1 }
           ]}"#,
    )
    .unwrap();
    let diagnostics: DiagnosticsEnvelope = serde_json::from_str(
        r#"{ "index_id": "demo", "total": 2, "diagnostics": [
             { "tool": "clippy", "file": "a.rs", "line": 1, "col": 1,
               "severity": "error", "code": "E0001", "message": "boom" },
             { "tool": "clippy", "file": "b.rs", "line": 2, "col": 1,
               "severity": "hint", "code": "H1", "message": "meh" }
           ]}"#,
    )
    .unwrap();
    let refactors: RefactorEnvelope = serde_json::from_str(
        r#"{ "index_id": "demo", "count": 1, "suggestions": [
             { "chunk_id": "a:1:9", "file": "a.rs", "line_start": 1, "line_end": 9,
               "function_name": "a", "refactor_type": "extract_method",
               "severity": "high", "rationale": "x", "suggested_action": "y",
               "complexity_before": 25, "complexity_after": 8, "smells": [] }
           ]}"#,
    )
    .unwrap();

    let m = map_metrics(
        &hotspots.hotspots,
        &diagnostics.diagnostics,
        &refactors.suggestions,
    );

    // loc/counts stay empty — the scanner owns them.
    assert_eq!(m.loc.total, 0);
    assert_eq!(m.counts.files, 0);

    // Two complexity buckets (F for 25, A for 4).
    let labels: Vec<&str> = m
        .complexity
        .buckets
        .iter()
        .map(|b| b.label.as_str())
        .collect();
    assert!(labels.contains(&"F: very high (>20)"));
    assert!(labels.contains(&"A: simple (0-5)"));

    // Findings: the error diagnostic (RED) + the high refactor (AMBER); the
    // hint diagnostic is dropped (GREEN).
    assert_eq!(m.findings.len(), 2);
    let red = m
        .findings
        .iter()
        .find(|f| f.severity == Severity::Red)
        .unwrap();
    assert_eq!(red.title, "E0001");
    let amber = m
        .findings
        .iter()
        .find(|f| f.severity == Severity::Amber)
        .unwrap();
    assert_eq!(amber.title, "Extract method — a");
}

// ─── Fail-open fetch ─────────────────────────────────────────────────────────

#[tokio::test]
async fn fetch_returns_none_on_unreachable_daemon() {
    // Port 1 is never listening; the probe fails and fetch swallows it.
    let src = HttpAnalyzeMetricsSource::new("http://127.0.0.1:1").unwrap();
    assert!(src.fetch("demo").await.is_none());
}

#[test]
fn new_trims_trailing_slash() {
    let src = HttpAnalyzeMetricsSource::new("http://127.0.0.1:7879/").unwrap();
    assert_eq!(src.base_url, "http://127.0.0.1:7879");
}

#[test]
fn derive_index_id_uses_basename() {
    assert_eq!(
        derive_index_id(std::path::Path::new("/home/me/northwind-web")).as_deref(),
        Some("northwind-web")
    );
}

#[test]
fn error_display() {
    let e = AnalyzeAdapterError::Api {
        status: 503,
        body: "down".into(),
    };
    assert!(e.to_string().contains("503"));
}
