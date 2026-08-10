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

// ─── Named gaps (#5239) ──────────────────────────────────────────────────────

/// A source that answers with a fixed outcome, so the enrichment walk can be
/// driven without a daemon.
struct StubSource(fn() -> AnalyzeFetch);

#[async_trait::async_trait]
impl AnalyzeMetricsSource for StubSource {
    async fn fetch(&self, _index_id: &str) -> Option<AnalyzeMetrics> {
        match (self.0)() {
            AnalyzeFetch::Fetched(m) => Some(*m),
            AnalyzeFetch::Missing(_) => None,
        }
    }

    async fn fetch_named(&self, _index_id: &str) -> AnalyzeFetch {
        (self.0)()
    }
}

/// A source that implements ONLY `fetch`, exercising the trait's default
/// `fetch_named` — the shape an out-of-crate implementor keeps compiling with.
struct MinimalSource;

#[async_trait::async_trait]
impl AnalyzeMetricsSource for MinimalSource {
    async fn fetch(&self, _index_id: &str) -> Option<AnalyzeMetrics> {
        None
    }
}

/// Build a one-repository model whose single entry is an unpopulated local
/// checkout — the only shape the enrichment walk acts on.
fn model_with_local_repo(name: &str) -> crate::report::model::ReportModel {
    let manifest = crate::report::manifest::parse_manifest(
        &format!("[report]\ntitle = \"T\"\n\n[[repositories]]\nname = \"{name}\"\npath = \".\"\n"),
        std::path::Path::new("m.toml"),
    )
    .expect("fixture manifest parses");
    let mut model = crate::report::model::ReportModel::build(
        &manifest,
        std::path::Path::new("m.toml"),
        "report-technical-dd",
        None,
    )
    .expect("model builds");
    // `.` always resolves to a directory, so `local_path` is populated; pin it
    // to a stable name so the derived index id does not depend on the CWD.
    model.repositories[0].local_path = Some(std::path::PathBuf::from("/tmp/northwind-web"));
    model
}

/// Why: the gap phrasing reaches a third party's desk; it must be fixed prose,
/// never a variant name and never run-specific detail.
/// Test: itself.
#[test]
fn gap_labels_are_stable() {
    assert_eq!(
        AnalyzeGap::NotIndexed.as_str(),
        "trusty-analyze index not built"
    );
    assert_eq!(
        AnalyzeGap::Unreachable.as_str(),
        "trusty-analyze unreachable"
    );
    assert_eq!(
        AnalyzeGap::Unavailable.as_str(),
        "trusty-analyze data unavailable"
    );
}

/// Why: the trait's default `fetch_named` is what keeps every existing
/// implementor compiling; it must still produce a NAMED outcome rather than
/// silently dropping the fact that nothing was fetched.
/// Test: itself.
#[tokio::test]
async fn default_fetch_named_reports_unavailable() {
    match MinimalSource.fetch_named("demo").await {
        AnalyzeFetch::Missing(gap) => assert_eq!(gap, AnalyzeGap::Unavailable),
        AnalyzeFetch::Fetched(_) => panic!("MinimalSource never fetches"),
    }
}

/// Why: #5239's core claim — a repository the daemon could not serve is named
/// in the report, and the line says "not assessed", not nothing at all.
/// Test: itself.
#[tokio::test]
async fn enrich_names_unreachable_repositories() {
    let mut model = model_with_local_repo("Northwind Web");
    let source = StubSource(|| AnalyzeFetch::Missing(AnalyzeGap::Unreachable));

    let gaps = enrich_with_analyze_gaps(&mut model, &source).await;

    assert_eq!(gaps.len(), 1, "one line per gap kind: {gaps:?}");
    assert!(
        gaps[0].starts_with("trusty-analyze unreachable"),
        "{}",
        gaps[0]
    );
    assert!(gaps[0].contains("Northwind Web"), "{}", gaps[0]);
    assert!(
        gaps[0].contains("not assessed, not clean"),
        "the line must refuse to read as a clean pass: {}",
        gaps[0]
    );
    assert!(model.repositories[0].metrics.is_none());
}

/// Why: the fail-open contract is unchanged — a populated repo yields metrics
/// and NO gap line, so a clean report stays clean.
/// Test: itself.
#[tokio::test]
async fn enrich_reports_no_gaps_when_every_repo_is_populated() {
    let mut model = model_with_local_repo("Northwind Web");
    let source = StubSource(|| {
        AnalyzeFetch::Fetched(Box::new(map_metrics(
            &[WireHotspot { cyclomatic: 3 }],
            &[],
            &[],
        )))
    });

    let gaps = enrich_with_analyze_gaps(&mut model, &source).await;

    assert!(gaps.is_empty(), "populated repo is not a gap: {gaps:?}");
    assert!(model.repositories[0].metrics.is_some());
}

/// Why: a remote entry was never eligible for a local index, so calling it an
/// unassessed gap would be a false alarm in every report with a remote repo.
/// Test: itself.
#[tokio::test]
async fn enrich_ignores_repositories_with_no_local_checkout() {
    let mut model = model_with_local_repo("Northwind Web");
    model.repositories[0].local_path = None;
    let source = StubSource(|| AnalyzeFetch::Missing(AnalyzeGap::Unreachable));

    assert!(
        enrich_with_analyze_gaps(&mut model, &source)
            .await
            .is_empty()
    );
}
