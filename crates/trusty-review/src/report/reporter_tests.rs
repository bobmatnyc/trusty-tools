//! Tests for the report reporter (scope mapping + markdown/JSON output).
//!
//! Why: the reporter is where the model meets the template; substring assertions
//! pin the deterministic mapping (app names present, honesty markers for
//! unmapped scoring fields) and the atomic dual-output file layout.
//! What: builds a model from a fixture manifest + metrics, renders the bundled
//! generic template, and asserts markdown substrings, JSON round-trip, and the
//! written file stem.
//! Test: included as `#[cfg(test)] mod tests` from `reporter.rs`.

use std::path::Path;

use super::Reporter;
use super::report_stem;
use crate::report::fill::HONESTY_MARKER;
use crate::report::manifest::parse_manifest;
use crate::report::model::ReportModel;
use crate::report::template::TemplateLoader;

/// Build a model from an in-memory manifest whose single repo has metrics.
fn fixture_model(dir: &Path) -> ReportModel {
    // Write a metrics file the manifest will reference by relative path.
    let metrics = r#"{
      "loc": { "total": 5000, "by_language": [
        { "language": "Rust", "loc": 5000 }
      ]},
      "counts": { "files": 20, "functions": 150 }
    }"#;
    std::fs::write(dir.join("acme.json"), metrics).expect("write metrics");

    let toml = r#"
        [report]
        title = "Acme Due Diligence"
        analyst = "bobmatnyc"

        [[repositories]]
        name = "Acme Web"
        path = "/nonexistent/acme-web"
        metrics = "acme.json"
    "#;
    let manifest_path = dir.join("manifest.toml");
    let manifest = parse_manifest(toml, &manifest_path).expect("manifest parse");
    ReportModel::build(&manifest, &manifest_path, "report-technical-dd").expect("build model")
}

/// Build a model whose single repo's metrics carry one RED, one AMBER, and two
/// GREEN findings (live-QA defect #2314 fixture — deterministic findings fill).
fn fixture_model_with_findings(dir: &Path) -> ReportModel {
    let metrics = r#"{
      "loc": { "total": 5000, "by_language": [ { "language": "Rust", "loc": 5000 } ] },
      "counts": { "files": 20, "functions": 150 },
      "findings": [
        { "title": "SQL injection", "severity": "red", "category": "security", "component": "db.rs" },
        { "title": "Stale dependency", "severity": "amber", "category": "maintainability", "component": "deps.toml" },
        { "title": "Strong test coverage", "severity": "green", "category": "quality", "component": "" },
        { "title": "Clean module boundaries", "severity": "green", "category": "architecture", "component": "" }
      ]
    }"#;
    std::fs::write(dir.join("acme.json"), metrics).expect("write metrics");

    let toml = r#"
        [report]
        title = "Acme Due Diligence"
        analyst = "bobmatnyc"

        [[repositories]]
        name = "Acme Web"
        path = "/nonexistent/acme-web"
        metrics = "acme.json"
    "#;
    let manifest_path = dir.join("manifest.toml");
    let manifest = parse_manifest(toml, &manifest_path).expect("manifest parse");
    ReportModel::build(&manifest, &manifest_path, "report-technical-dd").expect("build model")
}

/// Why: rendered markdown must carry filled report/app values and honesty
/// markers for unmapped (M1) scoring fields.
/// What: renders the bundled generic template and asserts key substrings.
/// Test: this test itself.
#[test]
fn render_contains_expected() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let model = fixture_model(tmp.path());
    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("bundled template");
    let reporter = Reporter::new(tmp.path());
    let md = reporter.render(&model, &template);

    // Report-level fill.
    assert!(md.contains("Acme Due Diligence"));
    assert!(md.contains("Acme Web"));
    assert!(md.contains("bobmatnyc"));
    // Metrics-derived per-application fill.
    assert!(md.contains("5000"));
    assert!(md.contains("20 files, 150 functions"));
    assert!(md.contains("Rust"));
    // Unmapped scoring field falls through to the honesty marker.
    assert!(md.contains(HONESTY_MARKER));
    // No raw placeholder survives.
    assert!(!md.contains("{{"));
}

/// Why: the model must assemble deterministically from the manifest.
/// What: asserts repository count, metrics presence, and no git info for a
/// non-existent local path.
/// Test: this test itself.
#[test]
fn build_model_from_manifest() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let model = fixture_model(tmp.path());
    assert_eq!(model.repositories.len(), 1);
    let r = &model.repositories[0];
    assert_eq!(r.name, "Acme Web");
    assert!(r.metrics.is_some());
    assert!(r.git_info.is_none()); // path does not exist → not a git repo
    assert_eq!(r.source_kind, "local_path");
}

/// Why: `write` must emit both a markdown and a JSON file that round-trips.
/// What: writes the report and asserts both files exist and the JSON parses.
/// Test: this test itself.
#[test]
fn write_emits_both() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let model = fixture_model(tmp.path());
    let out_dir = tmp.path().join("out");
    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("bundled template");
    let reporter = Reporter::new(&out_dir);
    let written = reporter.write(&model, &template).expect("write ok");
    assert_eq!(written.len(), 2);

    let md = written
        .iter()
        .find(|p| p.extension().unwrap() == "md")
        .unwrap();
    let json = written
        .iter()
        .find(|p| p.extension().unwrap() == "json")
        .unwrap();
    assert!(md.exists());
    assert!(json.exists());

    let json_text = std::fs::read_to_string(json).expect("read json");
    let parsed: serde_json::Value = serde_json::from_str(&json_text).expect("valid json");
    assert_eq!(parsed["title"], "Acme Due Diligence");
}

/// Why: available synthesis must inject verified prose into the exec-summary and
/// RED-finding placeholders and surface the `synthesis: available` note.
/// What: attaches an available `Synthesis` (exec + one RED finding routed to the
/// repo slug) to the model and asserts the prose and note appear in the markdown.
/// Test: this test itself.
#[test]
fn reporter_injects_synthesis_prose() {
    use crate::report::synthesize::{FindingProse, Synthesis, SynthesisStatus};

    let tmp = tempfile::TempDir::new().expect("tempdir");
    let mut model = fixture_model(tmp.path());
    let slug = model.repositories[0].slug.clone();
    model.synthesis = Some(Synthesis {
        status: SynthesisStatus::Available,
        executive_summary: Some("A grounded acquirer-relevant summary.".to_string()),
        top_risks: vec![],
        findings: vec![FindingProse {
            app_slug: slug,
            title: "Injection risk".to_string(),
            severity: "RED".to_string(),
            description: "Raw query concatenation.".to_string(),
            evidence: "one path".to_string(),
            component: "auth".to_string(),
            business_impact: "data loss".to_string(),
            remediation: "parameterise".to_string(),
            cost_effort: "moderate".to_string(),
        }],
        notes: vec![],
    });

    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("bundled template");
    let md = Reporter::new(tmp.path()).render(&model, &template);

    assert!(md.contains("A grounded acquirer-relevant summary."));
    assert!(md.contains("Injection risk"));
    assert!(md.contains("Raw query concatenation."));
    assert!(md.contains("synthesis: available"));
    assert!(!md.contains("{{"), "no raw placeholder survives");
}

/// Why: an unavailable synthesis must keep the deterministic output and surface
/// the fail-closed reason verbatim.
/// What: attaches `Synthesis::unavailable(..)`; asserts the note is present and
/// the exec-summary placeholder still falls through to the honesty marker.
/// Test: this test itself.
#[test]
fn reporter_appends_unavailable_note() {
    use crate::report::synthesize::Synthesis;

    let tmp = tempfile::TempDir::new().expect("tempdir");
    let mut model = fixture_model(tmp.path());
    model.synthesis = Some(Synthesis::unavailable("provider timeout"));

    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("bundled template");
    let md = Reporter::new(tmp.path()).render(&model, &template);

    assert!(md.contains("synthesis: unavailable (provider timeout)"));
    // Deterministic fallback: exec summary was never injected.
    assert!(md.contains(HONESTY_MARKER));
}

// ─── Live-QA defect #1: deterministic findings fill ───────────────────────────

/// Why: RED/AMBER findings must be listed from `metrics.findings` even with NO
/// `--synthesize` — title/category/component verbatim; prose-only fields
/// (description/evidence/business impact/remediation/cost) must still fall
/// through to the honesty marker rather than being silently omitted. GREENs
/// must appear as one-line topic titles only (no-green-analysis rule) and are
/// independent of synthesis.
/// What: renders the findings fixture with NO synthesis attached and asserts
/// the RED/AMBER titles + verbatim fields render, the green titles render as
/// bare topic bullets, and no raw placeholder survives.
/// Test: this test itself.
#[test]
fn reporter_fills_findings_deterministically() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let model = fixture_model_with_findings(tmp.path());
    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("bundled template");
    let md = Reporter::new(tmp.path()).render(&model, &template);

    // RED: title + component verbatim from metrics.
    assert!(md.contains("SQL injection"));
    assert!(md.contains("db.rs"));
    // AMBER: title verbatim.
    assert!(md.contains("Stale dependency"));
    // GREEN: bare topic titles, one line each (no-green-analysis rule).
    assert!(md.contains("Strong test coverage"));
    assert!(md.contains("Clean module boundaries"));

    // Prose-only fields have no deterministic source and stay honesty-marked —
    // both the RED sentence's business-impact clause and the AMBER sentence's
    // evidence clause fall through to the marker.
    assert!(md.contains(&format!("Business impact: {HONESTY_MARKER}")));
    assert!(md.contains(&format!("Evidence: {HONESTY_MARKER}")));
    assert!(!md.contains("{{"), "no raw placeholder survives");
}

/// Why: with no metrics at all, RED/AMBER/GREEN sections must stay exactly as
/// they were before this fix — honesty-marked, never fabricated.
/// What: renders the base (findings-free) fixture and asserts the green topic
/// slots and finding blocks fall through to markers/absence, not garbage.
/// Test: this test itself.
#[test]
fn reporter_leaves_findings_honesty_marked_without_metrics() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let model = fixture_model(tmp.path());
    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("bundled template");
    let md = Reporter::new(tmp.path()).render(&model, &template);
    assert!(md.contains(HONESTY_MARKER));
    assert!(!md.contains("{{"));
}

/// Why: when verified synthesis prose exists for a metrics-backed finding, it
/// must be MERGED onto the same row — never rendered as a second, duplicate
/// entry for the same finding (the double-fill the coordinator flagged).
/// What: attaches an available `Synthesis` whose one RED `FindingProse` title
/// matches the fixture's metrics RED finding ("SQL injection"); asserts the
/// metrics-verbatim fields (category/component) AND the synthesis prose fields
/// both appear, and the finding's title appears exactly once in the rendered
/// RED findings section (proving it is one merged row, not two).
/// Test: this test itself.
#[test]
fn reporter_merges_synthesis_prose_onto_deterministic_finding() {
    use crate::report::synthesize::{FindingProse, Synthesis, SynthesisStatus};

    let tmp = tempfile::TempDir::new().expect("tempdir");
    let mut model = fixture_model_with_findings(tmp.path());
    let slug = model.repositories[0].slug.clone();
    model.synthesis = Some(Synthesis {
        status: SynthesisStatus::Available,
        executive_summary: None,
        top_risks: vec![],
        findings: vec![FindingProse {
            app_slug: slug,
            title: "SQL injection".to_string(),
            severity: "RED".to_string(),
            description: "Unsanitised query parameters.".to_string(),
            evidence: "one endpoint".to_string(),
            component: "db.rs".to_string(),
            business_impact: "customer data exposure".to_string(),
            remediation: "use parameterised queries".to_string(),
            cost_effort: "low".to_string(),
        }],
        notes: vec![],
    });

    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("bundled template");
    let md = Reporter::new(tmp.path()).render(&model, &template);

    // Metrics-verbatim fields still present.
    assert!(md.contains("SQL injection"));
    assert!(md.contains("db.rs"));
    // Synthesis prose merged onto the same row.
    assert!(md.contains("Unsanitised query parameters."));
    assert!(md.contains("customer data exposure"));

    // Exactly one row for this finding: the RED findings section (5.1) contains
    // the title exactly once, not twice (deterministic row + separate synthesis
    // row would double it).
    let red_section = md
        .split("### 5.1")
        .nth(1)
        .and_then(|s| s.split("### 5.2").next())
        .expect("RED section present");
    assert_eq!(red_section.matches("SQL injection").count(), 1);
    assert!(!md.contains("{{"));
}

// ─── Live-QA defect #2: ordinal helper ────────────────────────────────────────

/// Why: naive `{n}th` formatting misrenders e.g. "71th"/"11th"-adjacent cases;
/// the ordinal helper must special-case the 11/12/13 teens exception and
/// otherwise follow the last digit.
/// What: asserts the documented edge-case set.
/// Test: this test itself.
#[test]
fn ordinal_edge_cases() {
    use super::ordinal;
    let cases = [
        (1, "1st"),
        (2, "2nd"),
        (3, "3rd"),
        (11, "11th"),
        (12, "12th"),
        (13, "13th"),
        (21, "21st"),
        (71, "71st"),
        (101, "101st"),
        (111, "111th"),
    ];
    for (n, expected) in cases {
        assert_eq!(ordinal(n), expected, "ordinal({n})");
    }
}

// ─── Live-QA defect #3: leading-comment header stripping ─────────────────────

/// Why: a generated report must never carry the bundled template's leading
/// instructional comment — and, before this fix, its literal `{{field_name}}`
/// and `per_application` BEGIN/END documentation examples were mangled into
/// the output (including duplicating the real per-application section).
/// What: renders the real bundled generic template and asserts the header's
/// distinctive instructional text is absent, the output starts at the title,
/// and the per-application heading appears exactly once (proving the header's
/// embedded example did not get expanded with live data).
/// Test: this test itself.
#[test]
fn reporter_strips_leading_comment_header() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let model = fixture_model(tmp.path());
    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("bundled template");
    let md = Reporter::new(tmp.path()).render(&model, &template);

    assert!(
        md.trim_start()
            .starts_with("# Technical Due-Diligence Analysis:")
    );
    assert!(!md.contains("PLACEHOLDER SYNTAX"));
    assert!(!md.contains("HOW IT'S USED"));
    assert!(!md.contains("trusty-review template:"));
    // The header's embedded `per_application` example must not have been
    // expanded with real data — the real section 4 heading appears exactly once.
    assert_eq!(md.matches("### 4.N. Acme Web").count(), 1);
}

/// Why: a custom template with no leading comment must render unaffected —
/// stripping must never remove real content when there is no header to strip.
/// What: renders a minimal inline template (no leading `<!-- … -->`) and
/// asserts the placeholder fills normally with nothing removed.
/// Test: this test itself.
#[test]
fn reporter_custom_template_without_header_is_unaffected() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let model = fixture_model(tmp.path());
    let md = Reporter::new(tmp.path()).render(&model, "# Custom {{target_codename}}\nBody.\n");
    assert_eq!(md, "# Custom Acme Due Diligence\nBody.\n");
}

/// Build a ranked single-metric benchmark for the fixture repo's slug.
fn ranked_benchmark(slug: &str) -> crate::report::benchmark::BenchmarkReport {
    use crate::report::benchmark::{
        BenchmarkReport, BenchmarkStatus, MetricPlacement, RepositoryBenchmark,
    };
    BenchmarkReport {
        corpus_size: 6,
        warnings: vec![],
        repositories: vec![RepositoryBenchmark {
            slug: slug.to_string(),
            name: "Acme Web".to_string(),
            status: BenchmarkStatus::Ranked,
            peers: 6,
            placements: vec![MetricPlacement {
                metric: "total_loc".to_string(),
                target_value: 5000.0,
                percentile: 60.0,
                quartile: 3,
                rank: 4,
                population: 7,
            }],
        }],
    }
}

/// Why: a ranked benchmark must fill the per-application Benchmark Position table,
/// the appendix dataset row, and surface a `## Benchmark Status` note with the
/// population size — never leaving those as honesty markers for a ranked repo.
/// What: attaches a ranked `BenchmarkReport` and asserts the filled quartile/rank
/// text and status note appear, with no raw placeholder surviving.
/// Test: this test itself.
#[test]
fn reporter_fills_benchmark() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let mut model = fixture_model(tmp.path());
    let slug = model.repositories[0].slug.clone();
    model.benchmark = Some(ranked_benchmark(&slug));

    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("bundled template");
    let md = Reporter::new(tmp.path()).render(&model, &template);

    // Per-application Benchmark Position table filled with the criterion + rank.
    assert!(md.contains("Total LoC"));
    assert!(md.contains("Q3"));
    assert!(md.contains("4 of 7"));
    assert!(md.contains("7 repos"));
    // Appendix headline benchmark row filled (percentile compliance).
    assert!(md.contains("| Acme Web | 7 repos | 60 | Q3 | 4 of 7 |"));
    // Status note carries the population size / peer count.
    assert!(md.contains("## Benchmark Status"));
    assert!(md.contains("corpus size 6"));
    assert!(md.contains("ranked against 6 peer(s)"));
    assert!(!md.contains("{{"), "no raw placeholder survives");
}

/// Why: a too-small corpus must be disclosed — the honesty marker text appears in
/// the table and the status note, and ranking never happens silently.
/// What: attaches a `CorpusTooSmall` benchmark and asserts the small-n message.
/// Test: this test itself.
#[test]
fn reporter_small_corpus_marks() {
    use crate::report::benchmark::{BenchmarkReport, BenchmarkStatus, RepositoryBenchmark};

    let tmp = tempfile::TempDir::new().expect("tempdir");
    let mut model = fixture_model(tmp.path());
    let slug = model.repositories[0].slug.clone();
    model.benchmark = Some(BenchmarkReport {
        corpus_size: 3,
        warnings: vec!["corpus directory /x does not exist yet".to_string()],
        repositories: vec![RepositoryBenchmark {
            slug,
            name: "Acme Web".to_string(),
            status: BenchmarkStatus::CorpusTooSmall(3),
            peers: 3,
            placements: vec![],
        }],
    });

    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("bundled template");
    let md = Reporter::new(tmp.path()).render(&model, &template);

    assert!(md.contains("benchmark: corpus too small (n=3)"));
    assert!(md.contains("## Benchmark Status"));
    assert!(md.contains("warning: corpus directory /x does not exist yet"));
    assert!(!md.contains("{{"));
}

/// Why: with benchmarking OFF the render must be byte-identical to M2 — no status
/// note, and the benchmark tables render exactly as their honesty-marked M2 form.
/// What: renders the same fixture with `benchmark = None` and asserts no status
/// section, then asserts the render equals a second identical render.
/// Test: this test itself.
#[test]
fn benchmark_off_is_unchanged() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let model = fixture_model(tmp.path());
    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("bundled template");
    let md = Reporter::new(tmp.path()).render(&model, &template);
    assert!(!md.contains("## Benchmark Status"));
    // The appendix benchmark row is present exactly once as honesty markers.
    let marker_row = format!(
        "| {HONESTY_MARKER} | {HONESTY_MARKER} | {HONESTY_MARKER} | {HONESTY_MARKER} | {HONESTY_MARKER} |"
    );
    assert!(md.contains(&marker_row));
    assert!(!md.contains("{{"));
}

/// Why: output filenames must be date-prefixed and slug-stable.
/// What: asserts the stem is `{date}-{title-slug}`.
/// Test: this test itself.
#[test]
fn stem_is_date_slug() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let model = fixture_model(tmp.path());
    let stem = report_stem(&model);
    assert!(stem.ends_with("-acme-due-diligence"));
    assert_eq!(stem, format!("{}-acme-due-diligence", model.generated_date));
}
