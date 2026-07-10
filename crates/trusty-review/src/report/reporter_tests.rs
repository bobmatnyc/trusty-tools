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
